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
    assert_eq!(capability.stdout, b"qiwo-cleanup-v1\n");
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
