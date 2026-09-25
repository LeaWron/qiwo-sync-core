//! Opt-in test. The wrapper creates, owns and removes an isolated remote fixture.
use qiwo_sync::{
    inventory,
    lifecycle::{self, Selection, job::Job, local, migration, transfer},
    types::{Frontend, SyncMode, SyncRequest},
};
use std::path::PathBuf;

struct TempRoot(PathBuf);
impl TempRoot {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "qiwo-compatible-live-{}",
            lifecycle::random_id().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "requires an owned disposable fixture, created by tools/test_dav_compatible_live.py"]
async fn isolated_live_migrate_sync_retire_restore() {
    let path = std::env::var("QIWO_DAV_FIXTURE_CONFIG").expect("use the isolated fixture wrapper");
    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let root = config["remoteUrl"].as_str().unwrap();
    let url = reqwest::Url::parse(root).unwrap();
    let id = url
        .path()
        .trim_end_matches('/')
        .rsplit_once("/probes/compat-")
        .expect("refuse a production sync root")
        .1;
    assert!(id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(url.scheme(), "https");
    let a = TempRoot::new();
    let request = SyncRequest {
        frontend: Frontend::Fcitx5Rime,
        rime_user_dir: a.0.clone(),
        remote_url: Some(root.into()),
        username: Some(config["username"].as_str().unwrap().into()),
        password: Some(config["password"].as_str().unwrap().into()),
        device_id: "fixture-current".into(),
        mode: SyncMode::Sync,
        frost_dir: None,
        dry_run: false,
    };
    let store = transfer::store(&request).unwrap();
    assert_eq!(
        store.read_legacy("owner.txt").await.unwrap(),
        format!("qiwo-dav-compat:{id}").as_bytes()
    );
    if std::env::var("QIWO_DAV_CAPABILITY_ONLY").as_deref() == Ok("1") {
        for round in 1..=3 {
            let result = store.diagnose_compatible().await;
            if let Ok(bytes) =
                local::read(&a.0, ".qiwo-sync/managed-capability-last-check.json", 4096)
            {
                println!(
                    "capability round {round}: {}",
                    String::from_utf8(bytes).unwrap()
                );
            }
            result.unwrap();
        }
        return;
    }
    let inventory = inventory::inspect(
        &a.0,
        root,
        request.username.as_deref().unwrap(),
        request.password.as_deref().unwrap(),
        &request.device_id,
    )
    .await
    .unwrap();
    assert!(inventory.remote_complete);
    let plan = migration::preview(&store, &inventory, &request.device_id, "fixture-migration")
        .await
        .unwrap();
    assert_eq!(plan.files.len(), 2);
    lifecycle::job::run(&request, &Job::Migrate { plan: plan.clone() })
        .await
        .unwrap();
    lifecycle::job::run(&request, &Job::Migrate { plan })
        .await
        .unwrap();
    let state = store.load().await.unwrap().0;
    assert_eq!(state.protocol_version, 3);
    assert!(local::enrollment(&a.0).unwrap().is_some());
    local::atomic_write(&a.0, "custom_phrase.txt", b"fixture-upload\tzzfixture\t1\n").unwrap();
    transfer::execute(&request).await.unwrap();
    let b = TempRoot::new();
    let mut other = request.clone();
    other.rime_user_dir = b.0.clone();
    other.device_id = "fixture-old".into();
    let current = store.load().await.unwrap().0;
    let join = Job::Join {
        migration_id: "fixture-migration".into(),
        actor: other.device_id.clone(),
        endpoint: store.endpoint_id(),
        state_digest: current.digest().unwrap(),
        files: current.files.clone(),
    };
    lifecycle::job::run(&other, &join).await.unwrap();
    let retire = current
        .plan(
            &request.device_id,
            "fixture-retire",
            Selection::RetireDevice {
                device_id: other.device_id.clone(),
            },
        )
        .unwrap();
    lifecycle::job::run(&request, &Job::Cleanup { plan: retire })
        .await
        .unwrap();
    local::atomic_write(
        &b.0,
        "sync/fixture-old/words.userdb.txt",
        b"stale-offline-edit",
    )
    .unwrap();
    assert!(transfer::execute(&other).await.is_err());
    assert!(!b.0.join("sync/fixture-old/words.userdb.txt").exists());
    let current = store.load().await.unwrap().0;
    let restore = current
        .plan(
            &request.device_id,
            "fixture-restore",
            Selection::Restore {
                batch_id: "fixture-retire".into(),
            },
        )
        .unwrap();
    lifecycle::job::run(&request, &Job::Cleanup { plan: restore })
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(a.0.join("sync/fixture-old/words.userdb.txt")).unwrap(),
        b"fixture-original-words\n"
    );
    assert_eq!(
        store.read_legacy("default.custom.yaml").await.unwrap(),
        b"patch: {}\n"
    );
    assert_eq!(
        store
            .read_legacy("sync/fixture-old/words.userdb.txt")
            .await
            .unwrap(),
        b"fixture-original-words\n"
    );
}
