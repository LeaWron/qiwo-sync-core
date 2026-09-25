use std::process::Command;

#[test]
fn configured_validation_is_private_and_does_not_touch_rime() {
    let dir = std::env::temp_dir().join(format!("qiwo-configured-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("webdav.json");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_qiwo-rime-sync"))
            .args([
                "sync-configured",
                "--frontend",
                "fcitx5-rime",
                "--device-id",
                "test-device",
                "--check",
                "--config",
            ])
            .arg(&config)
            .arg("--rime-user-dir")
            .arg(&dir)
            .output()
            .unwrap()
    };
    let save = |value: serde_json::Value| {
        std::fs::write(&config, value.to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    };
    save(
        serde_json::json!({"enabled":true,"remoteUrl":"https://dav.example.invalid/sync", "username":"user", "password":"private-test-value"}),
    );
    assert!(run().status.success()); // .invalid proves validation is offline.
    assert!(!dir.join("installation.yaml").exists());
    for url in [
        "https://user:private-test-value@dav.example.invalid/sync",
        "https://dav.example.invalid/sync?token=private-test-value",
        "https://dav.example.invalid/sync#private-test-value",
        "file:///tmp/sync",
    ] {
        save(serde_json::json!({"enabled":true,"remoteUrl":url}));
        let output = run();
        assert_eq!(output.status.code(), Some(3));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-test-value"));
    }
    save(serde_json::json!({"enabled":false,"remoteUrl":"https://dav.example.invalid/sync"}));
    assert_eq!(run().status.code(), Some(3));
    save(
        serde_json::json!({"enabled":true,"remoteUrl":"https://dav.example.invalid/sync", "username":"user"}),
    );
    assert_eq!(run().status.code(), Some(3));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        save(serde_json::json!({"enabled":true,"remoteUrl":"https://dav.example.invalid/sync"}));
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(run().status.code(), Some(3));
    }
    std::fs::remove_dir_all(dir).unwrap();
}
