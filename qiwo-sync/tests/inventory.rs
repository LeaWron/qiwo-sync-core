use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use qiwo_sync::inventory::{Inventory, inspect};

type Routes = HashMap<String, (u16, String)>;
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(routes: Routes) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/dav", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let headers = String::from_utf8(request).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                socket.read_exact(&mut body).unwrap();
                let key = headers
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ");
                observed.lock().unwrap().push(key.clone());
                let (status, body) = routes.get(&key).cloned().unwrap_or((404, String::new()));
                let reason = match status {
                    200 => "OK",
                    207 => "Multi-Status",
                    403 => "Forbidden",
                    405 => "Method Not Allowed",
                    _ => "Not Found",
                };
                write!(socket, "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "qiwo-inventory-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, bytes: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn xml(entries: &[(&str, bool, u64)]) -> String {
    let mut body = String::from("<d:multistatus xmlns:d='DAV:'>");
    for (path, dir, size) in entries {
        body.push_str(&format!("<d:response><d:href>{path}</d:href><d:propstat><d:prop><d:resourcetype>{}</d:resourcetype><d:getcontentlength>{size}</d:getcontentlength></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>", if *dir { "<d:collection/>" } else { "" }));
    }
    body.push_str("</d:multistatus>");
    body
}
fn manifest(paths: &[&str]) -> String {
    let files: serde_json::Map<String, serde_json::Value> = paths.iter().map(|path| (path.to_string(), serde_json::json!({
        "relativePath": path, "size": 7, "sha256": "a", "lastWriteUtc": "2026-01-01T00:00:00Z"
    }))).collect();
    serde_json::json!({ "version": 1, "deviceId": "last-publisher", "updatedAtUtc": "2026-01-01T00:00:00Z", "files": files }).to_string()
}
fn run(server: &Server, root: &std::path::Path) -> Inventory {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(inspect(root, &server.url, "", "", "Current PC"))
        .unwrap()
}

#[test]
fn inventories_real_files_manifest_orphans_and_empty_device_without_writes() {
    let temp = Temp::new();
    temp.write("default.custom.yaml", "local");
    temp.write("sync/old/dict.userdb.txt", "snapshot");
    temp.write(".qiwo-sync/webdav.ini", "password=SECRET");
    temp.write("installation.yaml", "installation_id: untouched\n");
    temp.write("build/ignored.bin", "binary");
    let server = Server::new(Routes::from([
        (
            "GET /dav/.qiwo-sync-manifest.json".into(),
            (
                200,
                manifest(&[
                    "default.custom.yaml",
                    "gone.custom.yaml",
                    "sync/old/dict.userdb.txt",
                    ".qiwo-sync/webdav.ini",
                    ".git/config",
                ]),
            ),
        ),
        (
            "PROPFIND /dav/".into(),
            (
                207,
                xml(&[
                    ("/dav/", true, 0),
                    ("/dav/default.custom.yaml", false, 10),
                    ("/dav/legacy.bin", false, 20),
                    ("/dav/sync/", true, 0),
                    ("/dav/.qiwo-sync/", true, 0),
                ]),
            ),
        ),
        (
            "PROPFIND /dav/sync/".into(),
            (
                207,
                xml(&[
                    ("/dav/sync/", true, 0),
                    ("/dav/sync/old/", true, 0),
                    ("/dav/sync/empty/", true, 0),
                ]),
            ),
        ),
        (
            "PROPFIND /dav/sync/old/".into(),
            (
                207,
                xml(&[
                    ("/dav/sync/old/", true, 0),
                    ("/dav/sync/old/dict.userdb.txt", false, 30),
                ]),
            ),
        ),
        (
            "PROPFIND /dav/sync/empty/".into(),
            (207, xml(&[("/dav/sync/empty/", true, 0)])),
        ),
    ]));
    let inventory = run(&server, &temp.0);
    assert!(inventory.local_complete && inventory.remote_complete);
    assert_eq!(inventory.remote_bytes, 60);
    assert_eq!(inventory.local_bytes, 13);
    assert!(
        inventory
            .files
            .iter()
            .find(|f| f.path == "gone.custom.yaml")
            .unwrap()
            .issues
            .contains(&"missing-remote".into())
    );
    assert_eq!(
        inventory
            .files
            .iter()
            .find(|f| f.path == "legacy.bin")
            .unwrap()
            .issues,
        ["untracked", "excluded"]
    );
    assert!(
        inventory
            .devices
            .iter()
            .any(|d| d.id == "empty" && d.file_count == 0)
    );
    assert!(inventory.devices.iter().all(|d| d.last_sync_at.is_none()));
    assert_eq!(inventory.devices[0].id, "current-pc");
    assert_eq!(
        std::fs::read_to_string(temp.0.join("installation.yaml")).unwrap(),
        "installation_id: untouched\n"
    );
    assert!(!temp.0.join(".qiwo-sync/manifest.json").exists());
    assert!(!temp.0.join("sync/current-pc").exists());
    let json = serde_json::to_string(&inventory).unwrap();
    assert!(
        !json.contains("SECRET") && !json.contains("webdav.ini") && !json.contains("ignored.bin")
    );
    assert_eq!(server.requests.lock().unwrap().len(), 5);
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.starts_with("GET /dav/.qiwo-sync-manifest.json")
                || r.starts_with("PROPFIND "))
    );
}

#[test]
fn corrupt_manifest_does_not_label_remote_files_untracked() {
    let temp = Temp::new();
    let server = Server::new(Routes::from([
        (
            "GET /dav/.qiwo-sync-manifest.json".into(),
            (200, "broken SECRET".into()),
        ),
        (
            "PROPFIND /dav/".into(),
            (
                207,
                xml(&[("/dav/", true, 0), ("/dav/a.custom.yaml", false, 4)]),
            ),
        ),
    ]));
    let result = run(&server, &temp.0);
    assert_eq!(result.manifest_status, "invalid");
    assert_eq!(result.files[0].tracked, None);
    assert!(result.files[0].issues.is_empty());
    assert!(!serde_json::to_string(&result).unwrap().contains("SECRET"));
}

#[test]
fn unsupported_listing_preserves_local_and_manifest_without_claiming_missing() {
    let temp = Temp::new();
    temp.write("custom_phrase.txt", "hello");
    let server = Server::new(Routes::from([
        (
            "GET /dav/.qiwo-sync-manifest.json".into(),
            (200, manifest(&["gone.custom.yaml"])),
        ),
        ("PROPFIND /dav/".into(), (405, "SECRET".into())),
    ]));
    let result = run(&server, &temp.0);
    assert!(!result.remote_complete);
    assert!(
        result
            .files
            .iter()
            .any(|f| f.path == "custom_phrase.txt" && f.local.is_some())
    );
    assert!(
        result
            .files
            .iter()
            .all(|f| !f.issues.contains(&"missing-remote".into()))
    );
    assert!(!serde_json::to_string(&result).unwrap().contains("SECRET"));
}

#[test]
fn absent_remote_and_local_directories_are_not_initialized() {
    let temp = Temp::new();
    let root = temp.0.join("absent");
    let server = Server::new(Routes::new());
    let result = run(&server, &root);
    assert_eq!(result.remote_status, "missing");
    assert_eq!(result.manifest_status, "missing");
    assert!(result.local_complete && result.remote_complete);
    assert!(!root.exists());
}

#[test]
fn unsafe_manifest_is_rejected_and_partial_listing_never_claims_absence() {
    let temp = Temp::new();
    let server = Server::new(Routes::from([
        (
            "GET /dav/.qiwo-sync-manifest.json".into(),
            (200, manifest(&["../secret"])),
        ),
        (
            "PROPFIND /dav/".into(),
            (
                207,
                xml(&[("/dav/", true, 0), ("https://evil.test/file", false, 3)]),
            ),
        ),
    ]));
    let result = run(&server, &temp.0);
    assert_eq!(result.manifest_status, "invalid");
    assert!(!result.remote_complete);
    assert!(result.files.is_empty());
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[cfg(unix)]
#[test]
fn local_symlink_is_not_followed_even_for_remote_known_paths() {
    let temp = Temp::new();
    let outside = Temp::new();
    outside.write("secret.custom.yaml", "secret");
    std::os::unix::fs::symlink(&outside.0, temp.0.join("sync")).unwrap();
    let server = Server::new(Routes::from([
        (
            "GET /dav/.qiwo-sync-manifest.json".into(),
            (200, manifest(&["sync/secret.custom.yaml"])),
        ),
        ("PROPFIND /dav/".into(), (207, xml(&[("/dav/", true, 0)]))),
    ]));
    let result = run(&server, &temp.0);
    assert!(!result.local_complete);
    assert!(result.files[0].local.is_none());
    assert!(result.files[0].issues.contains(&"local-unknown".into()));
}
