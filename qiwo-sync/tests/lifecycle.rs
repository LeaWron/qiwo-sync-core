use chrono::Utc;
use qiwo_sync::lifecycle::{ManagedState, ObjectRef, Selection};
use std::collections::BTreeSet;
fn state() -> ManagedState {
    let mut state = ManagedState::default();
    for path in [
        "sync/old/dict.userdb.txt",
        "sync/older/dict.userdb.txt",
        "sync/current/dict.userdb.txt",
        "default.custom.yaml",
    ] {
        state.files.insert(
            path.into(),
            ObjectRef {
                sha256: "a".repeat(64),
                size: 10,
            },
        );
    }
    state
}
fn delete(path: &str) -> Selection {
    Selection::DeleteFiles {
        paths: BTreeSet::from([path.into()]),
    }
}
#[test]
fn retirement_is_recoverable_idempotent_and_blocks_future_files_under_exact_id() {
    let original = state();
    let plan = original
        .plan(
            "current",
            "op1",
            Selection::RetireDevice {
                device_id: "old".into(),
            },
        )
        .unwrap();
    let retired = original.apply(&plan, Utc::now()).unwrap();
    assert!(retired.blocks_path("sync/old/new.userdb.txt"));
    assert!(!retired.blocks_path("sync/older/dict.userdb.txt"));
    assert_eq!(retired.files.len(), 3);
    assert_eq!(retired.trash["op1"].files.len(), 1);
    assert_eq!(retired.apply(&plan, Utc::now()).unwrap().revision, 1);
    let restore = retired
        .plan(
            "current",
            "restore1",
            Selection::Restore {
                batch_id: "op1".into(),
            },
        )
        .unwrap();
    let restored = retired.apply(&restore, Utc::now()).unwrap();
    assert_eq!(restored.files, original.files);
    assert!(!restored.blocks_path("sync/old/dict.userdb.txt"));
    assert!(restored.trash["op1"].restored_at.is_some());
    assert!(
        restored
            .plan(
                "current",
                "again",
                Selection::Restore {
                    batch_id: "op1".into()
                }
            )
            .is_err()
    );
}
#[test]
fn stale_preview_cannot_overwrite_newer_state_and_operation_ids_cannot_be_reused() {
    let original = state();
    let a = original
        .plan("current", "a", delete("default.custom.yaml"))
        .unwrap();
    let b = original
        .plan("current", "b", delete("sync/old/dict.userdb.txt"))
        .unwrap();
    let changed = original.apply(&a, Utc::now()).unwrap();
    assert!(changed.apply(&b, Utc::now()).is_err());
    let reused = changed
        .plan("current", "a", delete("sync/old/dict.userdb.txt"))
        .unwrap();
    assert!(changed.apply(&reused, Utc::now()).is_err());
    let mut tampered = a.clone();
    tampered.files.clear();
    assert!(original.apply(&tampered, Utc::now()).is_err());
}
#[test]
fn current_device_paths_and_metadata_are_protected() {
    let original = state();
    assert!(
        original
            .plan(
                "current",
                "a",
                Selection::RetireDevice {
                    device_id: "current".into()
                }
            )
            .is_err()
    );
    for path in [
        "sync/current/dict.userdb.txt",
        "../x",
        ".qiwo-sync-manifest.json",
        "installation.yaml",
        "sync/old/../x",
    ] {
        assert!(
            original.plan("current", "a", delete(path)).is_err(),
            "{path}"
        );
    }
    let mut invalid = original;
    invalid.protocol_version = 3;
    assert!(invalid.validate().is_err());
}
#[test]
fn restore_refuses_recreated_paths_and_files_of_another_retired_batch() {
    let original = state();
    let first = original
        .plan("current", "delete", delete("sync/old/dict.userdb.txt"))
        .unwrap();
    let deleted = original.apply(&first, Utc::now()).unwrap();
    let retire = deleted
        .plan(
            "current",
            "retire",
            Selection::RetireDevice {
                device_id: "old".into(),
            },
        )
        .unwrap();
    let retired = deleted.apply(&retire, Utc::now()).unwrap();
    assert!(
        retired
            .plan(
                "current",
                "restore",
                Selection::Restore {
                    batch_id: "delete".into()
                }
            )
            .is_err()
    );
    let mut recreated = deleted.clone();
    recreated.deleted_paths.clear();
    recreated.files.insert(
        "sync/old/dict.userdb.txt".into(),
        ObjectRef {
            sha256: "b".repeat(64),
            size: 20,
        },
    );
    assert!(
        recreated
            .plan(
                "current",
                "restore",
                Selection::Restore {
                    batch_id: "delete".into()
                }
            )
            .is_err()
    );
}
#[test]
fn tombstones_survive_restart_and_empty_device_retirement_is_restorable() {
    let original = state();
    let plan = original
        .plan(
            "current",
            "retire-empty",
            Selection::RetireDevice {
                device_id: "empty".into(),
            },
        )
        .unwrap();
    let retired = original.apply(&plan, Utc::now()).unwrap();
    let loaded: ManagedState =
        serde_json::from_slice(&serde_json::to_vec(&retired).unwrap()).unwrap();
    assert!(loaded.blocks_path("sync/empty/first.userdb.txt"));
    assert!(
        loaded
            .plan("empty", "op", delete("default.custom.yaml"))
            .is_err()
    );
    let plan = loaded
        .plan(
            "current",
            "undo",
            Selection::Restore {
                batch_id: "retire-empty".into(),
            },
        )
        .unwrap();
    assert!(
        !loaded
            .apply(&plan, Utc::now())
            .unwrap()
            .blocks_path("sync/empty/new")
    );
}

#[test]
fn legacy_preview_is_always_read_only_and_reports_incomplete_information() {
    use qiwo_sync::{
        inventory::{FileFacts, Inventory, InventoryDevice, InventoryFile},
        lifecycle::preview_legacy,
    };
    let inventory = Inventory {
        scanned_at: Utc::now(),
        current_device_id: "current".into(),
        remote_status: "ok".into(),
        manifest_status: "ok".into(),
        local_complete: true,
        remote_complete: false,
        local_bytes: 0,
        remote_bytes: 0,
        unknown_remote_sizes: 1,
        warnings: vec![],
        devices: vec![InventoryDevice {
            id: "old".into(),
            is_current: false,
            file_count: 1,
            local_bytes: 0,
            remote_bytes: 0,
            unknown_remote_sizes: 1,
            latest_file_modified_at: None,
            last_sync_at: None,
        }],
        files: vec![InventoryFile {
            path: "sync/old/dict.userdb.txt".into(),
            category: "snapshot".into(),
            device_id: Some("old".into()),
            local: None,
            remote: Some(FileFacts {
                size: None,
                modified_at: None,
            }),
            tracked: Some(false),
            sync_eligible: Some(true),
            issues: vec![],
        }],
    };
    let plan = preview_legacy(
        &inventory,
        Selection::RetireDevice {
            device_id: "old".into(),
        },
    )
    .unwrap();
    assert!(!plan.executable);
    assert_eq!(plan.unknown_sizes, 1);
    assert_eq!(plan.paths.len(), 1);
    assert!(plan.blockers.iter().any(|b| b.contains("不完整")));
    assert!(
        preview_legacy(
            &inventory,
            Selection::RetireDevice {
                device_id: "current".into()
            }
        )
        .is_err()
    );
    assert!(preview_legacy(&inventory, delete("missing.custom.yaml")).is_err());
}
