use std::{fs, process::Command};

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_qiwo-rime-sync"))
}

#[test]
fn capability_is_read_only_and_malformed_ids_never_create_jobs() {
    let root = std::env::temp_dir().join(format!("qiwo-cleanup-cli-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let capability = command()
        .arg("cleanup-capability")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(capability.status.success());
    assert_eq!(capability.stdout, b"qiwo-cleanup-v2\n");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    for id in ["../escape", "ABC", ""] {
        let result = command()
            .args([
                "cleanup-residuals",
                "--frontend",
                "weasel",
                "--rime-user-dir",
            ])
            .arg(&root)
            .args([
                "--remote-url",
                "http://127.0.0.1:9/unused",
                "--request-id",
                id,
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scoped_cancellation_does_not_start_network_or_delete_data() {
    let root = std::env::temp_dir().join(format!("qiwo-cancel-cli-{}", std::process::id()));
    let requests = root.join(".qiwo-sync/managed-requests");
    fs::create_dir_all(&requests).unwrap();
    let id = "b".repeat(64);
    fs::write(requests.join(format!("{id}.cancel")), b"").unwrap();
    fs::write(root.join("learning.userdb.txt"), b"preserved").unwrap();
    let result = command()
        .args([
            "cleanup-residuals",
            "--frontend",
            "squirrel",
            "--rime-user-dir",
        ])
        .arg(&root)
        .args([
            "--remote-url",
            "http://127.0.0.1:9/never-contacted",
            "--request-id",
            &id,
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let reply: serde_json::Value =
        serde_json::from_slice(&fs::read(requests.join(format!("{id}.result.json"))).unwrap())
            .unwrap();
    assert_eq!(reply["cancelled"], true);
    assert_eq!(
        fs::read(root.join("learning.userdb.txt")).unwrap(),
        b"preserved"
    );
    assert!(!root.join(".qiwo-sync/cleanup-backups").exists());
    fs::remove_dir_all(root).unwrap();
}
