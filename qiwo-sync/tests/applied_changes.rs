use qiwo_sync::changes::{self, ApplyKind, Purpose};
use std::{fs, path::PathBuf};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "qiwo-applied-{}",
            qiwo_sync::lifecycle::random_id().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn put(&self, path: &str, bytes: &[u8]) {
        let staged = self.0.join("staged");
        fs::write(&staged, bytes).unwrap();
        changes::apply(&self.0, path, Some(&staged), "current").unwrap();
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn classification_does_not_confuse_base_dictionaries_with_learning_snapshots() {
    let cases = [
        ("sync/other/pinyin.userdb.txt", Purpose::LearningSnapshot),
        ("sync/current/pinyin.userdb.txt", Purpose::Unknown),
        ("sync/other/base.dict.yaml", Purpose::Dictionary),
        ("base.dict.yaml", Purpose::Dictionary),
        ("custom_phrase.txt", Purpose::Dictionary),
        ("rime.schema.yaml", Purpose::Schema),
        ("default.custom.yaml", Purpose::Configuration),
        ("sync/other/arbitrary.txt", Purpose::Unknown),
        ("sync/other/some.yaml", Purpose::Unknown),
        (".qiwo-sync/backup.dict.yaml", Purpose::Internal),
        (".qiwo-sync-manifest.json", Purpose::Internal),
        ("sync/other/user.yaml", Purpose::Internal),
    ];
    for (path, expected) in cases {
        assert_eq!(changes::classify(path, "current"), expected, "{path}");
    }
}
#[test]
fn only_changed_applied_content_produces_pending_work() {
    let root = Root::new();
    root.put("sync/other/words.userdb.txt", b"words");
    root.put("default.custom.yaml", b"configuration");
    root.put("sync/other/unknown.txt", b"unknown");
    root.put("sync/other/user.yaml", b"internal");
    root.put("default.custom.yaml", b"configuration");
    let state = changes::state(&root.0).unwrap();
    assert_eq!(state.pending_merge.len(), 1);
    assert_eq!(state.pending_deploy.len(), 1);
    assert_eq!(state.unknown.len(), 1);
    assert_eq!(state.pending_deploy[0].operation, "added");
    root.put("default.custom.yaml", b"new configuration");
    changes::apply(&root.0, "default.custom.yaml", None, "current").unwrap();
    let state = changes::state(&root.0).unwrap();
    let kinds: Vec<_> = state
        .pending_deploy
        .iter()
        .map(|c| c.operation.as_str())
        .collect();
    assert!(kinds.contains(&"modified") && kinds.contains(&"deleted"));
}
#[test]
fn successful_deployment_cannot_acknowledge_changes_arriving_during_deployment() {
    let root = Root::new();
    root.put("default.custom.yaml", b"first");
    let task = changes::begin(&root.0, ApplyKind::Deploy).unwrap();
    root.put("default.custom.yaml", b"second");
    changes::complete(&root.0, &task.id, "succeeded").unwrap();
    let state = changes::state(&root.0).unwrap();
    assert_eq!(state.pending_deploy.len(), 1);
    assert_eq!(state.pending_deploy[0].operation, "modified");
    assert!(changes::complete(&root.0, &task.id, "succeeded").is_err());
}
#[test]
fn failure_and_cancellation_preserve_pending_work_after_restart() {
    let root = Root::new();
    root.put("sync/other/words.userdb.txt", b"words");
    for outcome in ["failed", "cancelled"] {
        let task = changes::begin(&root.0, ApplyKind::Merge).unwrap();
        changes::complete(&root.0, &task.id, outcome).unwrap();
        assert_eq!(changes::state(&root.0).unwrap().pending_merge.len(), 1);
    }
    let task = changes::begin(&root.0, ApplyKind::Merge).unwrap();
    changes::complete(&root.0, &task.id, "succeeded").unwrap();
    assert!(changes::state(&root.0).unwrap().pending_merge.is_empty());
}
#[test]
fn file_failure_is_separate_from_already_applied_configuration_changes() {
    let root = Root::new();
    let run = qiwo_sync::lifecycle::random_id().unwrap();
    changes::file_result(&root.0, &run, "running").unwrap();
    root.put("default.custom.yaml", b"applied");
    changes::file_result(&root.0, &run, "failed").unwrap();
    let state = changes::state(&root.0).unwrap();
    assert_eq!(state.file_sync.unwrap().outcome, "failed");
    assert_eq!(state.pending_deploy.len(), 1);
}

#[test]
fn cancellation_is_scoped_and_survives_later_phase_updates() {
    let root = Root::new();
    let task = changes::native_task(&root.0, None, Some("waiting-merge"), false).unwrap();
    assert!(changes::native_task(&root.0, Some(&"f".repeat(64)), None, true).is_err());
    changes::native_task(&root.0, Some(&task.id), None, true).unwrap();
    let updated = changes::native_task(&root.0, Some(&task.id), Some("merging"), false).unwrap();
    assert!(updated.cancel_requested);
    assert!(
        root.0
            .join(".qiwo-sync/native-cancel")
            .join(&task.id)
            .is_file()
    );
    changes::native_task(&root.0, Some(&task.id), Some("cancelled"), false).unwrap();
    assert!(changes::native_task(&root.0, Some(&task.id), None, true).is_err());
}

#[test]
fn unacknowledged_native_work_survives_restart_and_intents_require_applied_bytes() {
    let root = Root::new();
    root.put("default.custom.yaml", b"applied");
    let task = changes::begin(&root.0, ApplyKind::Deploy).unwrap();
    let state = changes::state(&root.0).unwrap();
    assert_eq!(state.apply_tasks[0].id, task.id);
    assert_eq!(state.pending_deploy.len(), 1);
    let mut event = state.pending_deploy[0].clone();
    event.committed = false;
    let path = root
        .0
        .join(".qiwo-sync/applied-changes")
        .join(format!("{}.json", event.id));
    fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
    assert_eq!(changes::state(&root.0).unwrap().pending_deploy.len(), 1);
    fs::write(root.0.join("default.custom.yaml"), b"not applied").unwrap();
    assert!(changes::state(&root.0).unwrap().pending_deploy.is_empty());
}
