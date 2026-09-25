use qiwo_sync::{
    cleanup::{self, Selection},
    sync_engine::SyncEngine,
    types::{Frontend, SyncMode, SyncRequest},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Default)]
struct Remote {
    files: BTreeMap<String, Vec<u8>>,
    directories: BTreeSet<String>,
    requests: Vec<(String, String)>,
    fail_delete: Option<String>,
    fail_listing: bool,
    invalid_self: bool,
    backup_before_delete: Option<PathBuf>,
}
impl Remote {
    fn add_parents(&mut self, path: &str) {
        let mut path = path.trim_end_matches('/');
        while let Some((parent, _)) = path.rsplit_once('/') {
            self.directories.insert(parent.into());
            path = parent;
        }
    }
}
struct Dav {
    url: String,
    remote: Arc<Mutex<Remote>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Dav {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/dav", listener.local_addr().unwrap());
        let remote = Arc::new(Mutex::new(Remote::default()));
        let data = remote.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let Ok((mut socket, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut header = Vec::new();
                let mut byte = [0];
                while !header.ends_with(b"\r\n\r\n") {
                    if socket.read_exact(&mut byte).is_err() {
                        break;
                    }
                    header.push(byte[0]);
                }
                let header = String::from_utf8(header).unwrap();
                let mut words = header.lines().next().unwrap().split_whitespace();
                let method = words.next().unwrap();
                let path = words
                    .next()
                    .unwrap()
                    .strip_prefix("/dav")
                    .unwrap()
                    .trim_start_matches('/');
                let length = header
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut bytes = vec![0; length];
                socket.read_exact(&mut bytes).unwrap();
                let mut remote = data.lock().unwrap();
                remote.requests.push((method.into(), path.into()));
                let (status, body) = match method {
                    "GET" => remote
                        .files
                        .get(path)
                        .map(|v| (200, v.clone()))
                        .unwrap_or((404, vec![])),
                    "PUT" => {
                        remote.add_parents(path);
                        remote.files.insert(path.into(), bytes);
                        (204, vec![])
                    }
                    "DELETE" => {
                        if let Some(root) = &remote.backup_before_delete
                            && !path.ends_with('/')
                        {
                            assert!(
                                root.join(format!("remote/{path}")).is_file(),
                                "backup must precede deletion"
                            );
                        }
                        if remote.fail_delete.as_deref() == Some(path) {
                            (503, vec![])
                        } else {
                            if path.ends_with('/') {
                                assert!(
                                    !remote.files.keys().any(|p| p.starts_with(path)),
                                    "never DELETE a nonempty directory"
                                );
                                assert!(
                                    !remote.directories.iter().any(|p| p.starts_with(path)),
                                    "delete children first"
                                );
                                remote.directories.remove(path.trim_end_matches('/'));
                            } else {
                                remote.files.remove(path);
                            }
                            (204, vec![])
                        }
                    }
                    "MKCOL" => {
                        remote.add_parents(path);
                        remote.directories.insert(path.trim_end_matches('/').into());
                        (201, vec![])
                    }
                    "PROPFIND" if remote.fail_listing => (503, vec![]),
                    "PROPFIND"
                        if !path.is_empty()
                            && !remote.directories.contains(path.trim_end_matches('/')) =>
                    {
                        (404, vec![])
                    }
                    "PROPFIND" => {
                        let mut children = BTreeMap::new();
                        for (key, value) in &remote.files {
                            if let Some(rest) = key.strip_prefix(path)
                                && !rest.is_empty()
                            {
                                let name = rest.split('/').next().unwrap();
                                let is_dir = rest.contains('/');
                                children.insert(
                                    format!("{path}{name}{}", if is_dir { "/" } else { "" }),
                                    (is_dir, value.len()),
                                );
                            }
                        }
                        for dir in &remote.directories {
                            if let Some(rest) = dir.strip_prefix(path)
                                && !rest.is_empty()
                                && !rest.contains('/')
                            {
                                children.insert(format!("{dir}/"), (true, 0));
                            }
                        }
                        let mut xml = "<d:multistatus xmlns:d=\"DAV:\">".to_string();
                        children.insert(path.to_string(), (!remote.invalid_self, 0));
                        for (name, (dir, size)) in children {
                            xml.push_str(&format!("<d:response><d:href>/dav/{name}</d:href><d:propstat><d:prop><d:resourcetype>{}</d:resourcetype><d:getcontentlength>{size}</d:getcontentlength></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>", if dir { "<d:collection/>" } else { "" }));
                        }
                        xml.push_str("</d:multistatus>");
                        (207, xml.into_bytes())
                    }
                    _ => (405, vec![]),
                };
                // Deliberately raw ETags and ignored If-Match: maintenance must not
                // create a managed space or claim to establish atomic CAS.
                let tag = qiwo_sync::lifecycle::store::reference(&body).sha256;
                write!(socket, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nETag: {tag}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                let _ = socket.write_all(&body);
            }
        });
        Self {
            url,
            remote,
            stop,
            worker: Some(worker),
        }
    }
    fn put(&self, path: &str, bytes: &[u8]) {
        let mut remote = self.remote.lock().unwrap();
        remote.add_parents(path);
        remote.files.insert(path.into(), bytes.into());
    }
    fn manifest(&self) -> Vec<u8> {
        let files: serde_json::Map<_,_> = self.remote.lock().unwrap().files.iter().filter(|(p,_)| !p.starts_with('.')).map(|(p,b)| {
            let hash = qiwo_sync::lifecycle::store::reference(b);
            (p.clone(), serde_json::json!({"relativePath":p,"sha256":hash.sha256,"size":hash.size,"lastWriteUtc":"2026-09-25T00:00:00Z"}))
        }).collect();
        let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"deviceId":"other","updatedAtUtc":"2026-09-25T00:00:00Z","files":files,"extra":"preserve"})).unwrap();
        self.put(".qiwo-sync-manifest.json", &bytes);
        bytes
    }
    fn writes(&self) -> usize {
        self.remote
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(m, _)| !matches!(m.as_str(), "GET" | "PROPFIND"))
            .count()
    }
}
impl Drop for Dav {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Local(PathBuf);
impl Local {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "qiwo-cleanup-{}",
            qiwo_sync::lifecycle::random_id().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn put(&self, path: &str, bytes: &[u8]) {
        let p = self.0.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    fn request(&self, dav: &Dav) -> SyncRequest {
        SyncRequest {
            frontend: Frontend::Fcitx5Rime,
            rime_user_dir: self.0.clone(),
            remote_url: Some(dav.url.clone()),
            username: None,
            password: None,
            device_id: "current".into(),
            mode: SyncMode::Sync,
            frost_dir: None,
            dry_run: false,
        }
    }
}
impl Drop for Local {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn device() -> Selection {
    Selection::Device {
        device_id: "old".into(),
    }
}

#[tokio::test]
async fn removes_nested_empty_directories_and_supports_directory_only_preview() {
    let dav = Dav::new();
    let local = Local::new();
    dav.put("sync/old/nested/words.userdb.txt", b"old");
    dav.manifest();
    local.put("sync/old/nested/words.userdb.txt", b"cache");
    let request = local.request(&dav);
    let plan = cleanup::preview(&request, device()).await.unwrap();
    let report = cleanup::execute(&request, &plan).await.unwrap();
    assert_eq!(
        report.remote_directories_deleted,
        ["sync/old/nested", "sync/old"]
    );
    assert_eq!(
        report.local_directories_removed,
        report.remote_directories_deleted
    );
    assert!(local.0.join("sync").is_dir());
    assert!(dav.remote.lock().unwrap().directories.contains("sync"));

    dav.remote
        .lock()
        .unwrap()
        .directories
        .extend(["sync/old".into(), "sync/old/empty".into()]);
    std::fs::create_dir_all(local.0.join("sync/old/empty")).unwrap();
    let plan = cleanup::preview(&request, device()).await.unwrap();
    assert!(plan.files.is_empty());
    assert_eq!(plan.directories.len(), 2);
    let report = cleanup::execute(&request, &plan).await.unwrap();
    assert_eq!(
        report.remote_directories_deleted,
        ["sync/old/empty", "sync/old"]
    );
    assert!(!local.0.join("sync/old").exists());
}

#[tokio::test]
async fn directory_cleanup_preserves_unselected_content_and_rejects_unverified_listing() {
    let dav = Dav::new();
    let local = Local::new();
    dav.put("sync/old/nested/a.txt", b"selected");
    dav.put("sync/old/nested/b.txt", b"keep");
    dav.manifest();
    local.put("sync/old/nested/a.txt", b"selected");
    let request = local.request(&dav);
    let plan = cleanup::preview(
        &request,
        Selection::Files {
            paths: ["sync/old/nested/a.txt".into()].into(),
        },
    )
    .await
    .unwrap();
    local.put("sync/old/nested/new.txt", b"new after preview");
    let report = cleanup::execute(&request, &plan).await.unwrap();
    assert!(report.remote_directories_deleted.is_empty());
    assert!(report.local_directories_removed.is_empty());
    assert_eq!(report.directories_kept.len(), 4);
    assert_eq!(
        std::fs::read(local.0.join("sync/old/nested/new.txt")).unwrap(),
        b"new after preview"
    );

    let plan = cleanup::preview(&request, device()).await.unwrap();
    dav.remote.lock().unwrap().invalid_self = true;
    assert!(cleanup::execute(&request, &plan).await.is_err());
    assert!(
        !dav.remote
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|(m, p)| m == "DELETE" && p.ends_with('/'))
    );
    assert!(!dav.remote.lock().unwrap().directories.is_empty());
}

#[tokio::test]
async fn directory_scope_cannot_escape_selected_device_and_failed_delete_can_be_previewed_again() {
    let dav = Dav::new();
    let local = Local::new();
    dav.remote
        .lock()
        .unwrap()
        .directories
        .extend(["sync".into(), "sync/old".into()]);
    let request = local.request(&dav);
    let original = cleanup::preview(&request, device()).await.unwrap();
    for path in [
        "sync",
        "sync/current",
        "sync/another",
        ".qiwo-sync",
        "sync/old/../current",
    ] {
        let mut plan = original.clone();
        plan.directories.insert(path.into());
        assert!(cleanup::execute(&request, &plan).await.is_err(), "{path}");
    }
    assert_eq!(dav.writes(), 0);
    dav.remote.lock().unwrap().fail_delete = Some("sync/old/".into());
    assert!(cleanup::execute(&request, &original).await.is_err());
    assert!(cleanup::execute(&request, &original).await.is_err());
    dav.remote.lock().unwrap().fail_delete = None;
    let new = cleanup::preview(&request, device()).await.unwrap();
    assert!(cleanup::execute(&request, &new).await.unwrap().complete);
    assert!(!dav.remote.lock().unwrap().directories.contains("sync/old"));
}

#[tokio::test]
async fn cleanup_keeps_original_layout_backs_up_both_sides_and_allows_same_id_to_return() {
    let dav = Dav::new();
    let local = Local::new();
    dav.put("sync/old/words.userdb.txt", b"remote old");
    dav.put("sync/current/words.userdb.txt", b"current");
    dav.put("default.custom.yaml", b"patch: {}");
    let manifest = dav.manifest();
    local.put(".qiwo-sync/manifest.json", &manifest);
    local.put("sync/old/words.userdb.txt", b"local divergent");
    local.put("words.userdb/data", b"learned words");
    let request = local.request(&dav);
    let plan = cleanup::preview(&request, device()).await.unwrap();
    assert_eq!(dav.writes(), 0);
    dav.remote.lock().unwrap().backup_before_delete = Some(
        local
            .0
            .join(format!(".qiwo-sync/cleanup-backups/{}", plan.id)),
    );
    dav.put("sync/old/new-after-preview.txt", b"do not delete");
    let report = cleanup::execute(&request, &plan).await.unwrap();
    assert!(report.complete);
    assert_eq!(report.remote_deleted.len(), 1);
    assert_eq!(report.local_removed.len(), 1);
    assert_eq!(report.directories_kept, ["云端 sync/old/"]);
    assert!(!local.0.join("sync/old").exists());
    assert_eq!(
        std::fs::read(PathBuf::from(&report.backup_path).join("remote/sync/old/words.userdb.txt"))
            .unwrap(),
        b"remote old"
    );
    assert_eq!(
        std::fs::read(PathBuf::from(&report.backup_path).join("local/sync/old/words.userdb.txt"))
            .unwrap(),
        b"local divergent"
    );
    assert_eq!(
        std::fs::read(local.0.join("words.userdb/data")).unwrap(),
        b"learned words"
    );
    {
        let remote = dav.remote.lock().unwrap();
        assert!(!remote.files.contains_key("sync/old/words.userdb.txt"));
        assert!(remote.files.contains_key("sync/old/new-after-preview.txt"));
        assert!(remote.files.contains_key("default.custom.yaml"));
        let value: serde_json::Value =
            serde_json::from_slice(&remote.files[".qiwo-sync-manifest.json"]).unwrap();
        assert_eq!(value["extra"], "preserve");
        assert!(value["files"].get("sync/old/words.userdb.txt").is_none());
        assert!(
            !remote
                .requests
                .iter()
                .any(|(m, p)| m == "MKCOL" || m == "MOVE" || p.contains("qiwo-managed"))
        );
    }
    let returning = Local::new();
    returning.put("sync/old/words.userdb.txt", b"new session");
    let mut request = returning.request(&dav);
    request.device_id = "old".into();
    request.mode = SyncMode::Push;
    SyncEngine::new().execute(request).await.unwrap();
    assert_eq!(
        dav.remote.lock().unwrap().files["sync/old/words.userdb.txt"],
        b"new session"
    );
    assert!(
        cleanup::execute(&local.request(&dav), &plan)
            .await
            .unwrap_err()
            .to_string()
            .contains("不能重复删除")
    );
    assert_eq!(
        dav.remote.lock().unwrap().files["sync/old/words.userdb.txt"],
        b"new session"
    );
}

#[tokio::test]
async fn stale_preview_and_incomplete_inventory_cannot_delete() {
    let dav = Dav::new();
    let local = Local::new();
    dav.put("sync/old/words.userdb.txt", b"old");
    dav.manifest();
    let request = local.request(&dav);
    let plan = cleanup::preview(&request, device()).await.unwrap();
    dav.put("sync/old/words.userdb.txt", b"changed");
    assert!(
        cleanup::execute(&request, &plan)
            .await
            .unwrap_err()
            .to_string()
            .contains("文件已变化")
    );
    assert_eq!(dav.writes(), 0);
    dav.remote.lock().unwrap().fail_listing = true;
    assert!(cleanup::preview(&request, device()).await.is_err());
    assert_eq!(dav.writes(), 0);
}

#[tokio::test]
async fn partial_failure_keeps_backups_and_refuses_replaying_old_task() {
    let dav = Dav::new();
    let local = Local::new();
    dav.put("sync/old/a.txt", b"a");
    dav.put("sync/old/b.txt", b"b");
    dav.manifest();
    let request = local.request(&dav);
    let plan = cleanup::preview(&request, device()).await.unwrap();
    dav.remote.lock().unwrap().fail_delete = Some("sync/old/b.txt".into());
    assert!(cleanup::execute(&request, &plan).await.is_err());
    let report: cleanup::Report = serde_json::from_slice(
        &std::fs::read(local.0.join(format!(
            ".qiwo-sync/cleanup-backups/{}/result.json",
            plan.id
        )))
        .unwrap(),
    )
    .unwrap();
    assert!(!report.complete);
    assert_eq!(report.remote_deleted, vec!["sync/old/a.txt"]);
    assert!(
        PathBuf::from(report.backup_path)
            .join("remote/sync/old/b.txt")
            .is_file()
    );
    dav.put("sync/old/a.txt", b"recreated");
    let before = dav.writes();
    assert!(cleanup::execute(&request, &plan).await.is_err());
    assert_eq!(dav.writes(), before);
    let new_plan = cleanup::preview(&request, device()).await.unwrap();
    assert!(new_plan.files.contains_key("sync/old/b.txt"));
}

#[tokio::test]
async fn current_device_shared_paths_and_symlinks_are_protected() {
    let dav = Dav::new();
    let local = Local::new();
    dav.put("sync/current/words.userdb.txt", b"current");
    dav.put("default.custom.yaml", b"config");
    dav.put("sync/old/words.userdb.txt", b"old");
    dav.manifest();
    let request = local.request(&dav);
    for path in [
        "sync/current/words.userdb.txt",
        "default.custom.yaml",
        "sync/old/../current/words.userdb.txt",
        "sync/old/.private",
    ] {
        assert!(
            cleanup::preview(
                &request,
                Selection::Files {
                    paths: BTreeSet::from([path.into()])
                }
            )
            .await
            .is_err()
        );
    }
    #[cfg(unix)]
    {
        let outside = Local::new();
        outside.put("words.userdb.txt", b"protected");
        std::fs::create_dir(local.0.join("sync")).unwrap();
        std::os::unix::fs::symlink(&outside.0, local.0.join("sync/old")).unwrap();
        assert!(cleanup::preview(&request, device()).await.is_err());
        assert_eq!(
            std::fs::read(outside.0.join("words.userdb.txt")).unwrap(),
            b"protected"
        );
    }
    assert_eq!(dav.writes(), 0);
}

#[tokio::test]
async fn untracked_files_can_be_cleaned_without_creating_a_remote_manifest() {
    let dav = Dav::new();
    let local = Local::new();
    dav.put("sync/old/words.userdb.txt", b"untracked");
    let request = local.request(&dav);
    let plan = cleanup::preview(&request, device()).await.unwrap();
    cleanup::execute(&request, &plan).await.unwrap();
    let remote = dav.remote.lock().unwrap();
    assert!(remote.files.is_empty());
    assert!(
        !remote
            .requests
            .iter()
            .any(|(m, _)| m == "PUT" || m == "MKCOL")
    );
}

#[tokio::test]
async fn foreign_cache_does_not_reupload_but_current_device_can_publish() {
    let dav = Dav::new();
    let local = Local::new();
    dav.put("sync/active/words.userdb.txt", b"authoritative");
    dav.manifest();
    local.put("sync/old/words.userdb.txt", b"stale cache");
    local.put("sync/active/words.userdb.txt", b"changed cache");
    local.put("sync/current/words.userdb.txt", b"own words");
    let request = local.request(&dav);
    cleanup::prune_foreign_cache(&request).await.unwrap();
    assert!(!local.0.join("sync/old").exists());
    SyncEngine::new().execute(request).await.unwrap();
    let remote = dav.remote.lock().unwrap();
    assert!(!remote.files.contains_key("sync/old/words.userdb.txt"));
    assert_eq!(
        remote.files["sync/active/words.userdb.txt"],
        b"authoritative"
    );
    assert_eq!(remote.files["sync/current/words.userdb.txt"], b"own words");
    assert!(
        !remote
            .requests
            .iter()
            .any(|(m, p)| m == "PUT" && p.starts_with("sync/active/"))
    );
    assert_eq!(
        std::fs::read(local.0.join("sync/active/words.userdb.txt")).unwrap(),
        b"authoritative"
    );
}
