//! End-to-end three-way merge tests against an in-memory WebDAV stub.
//!
//! `sync_engine.rs` holds the merge, the conflict backup and the manifest
//! bookkeeping, and had no coverage at all — which is how the `sync-user-dict`
//! manifest bug and the silent corrupt-manifest downgrade both survived.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use qiwo_sync::sync_engine::SyncEngine;
use qiwo_sync::types::{Frontend, SyncMode, SyncRequest};

// ---------------------------------------------------------------- WebDAV stub

#[derive(Default)]
struct Store {
    files: HashMap<String, Vec<u8>>,
}

/// Tracks how many PUTs are being served at the same moment, so a test can show
/// that transfers really do overlap rather than just that they all completed.
#[derive(Default)]
struct PutGauge {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

impl PutGauge {
    fn enter(&self) {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

struct DavStub {
    base_url: String,
    store: Arc<Mutex<Store>>,
    gauge: Arc<PutGauge>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl DavStub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}/dav");
        let store = Arc::new(Mutex::new(Store::default()));
        let gauge = Arc::new(PutGauge::default());
        let stop = Arc::new(AtomicBool::new(false));

        let worker_store = Arc::clone(&store);
        let worker_gauge = Arc::clone(&gauge);
        let worker_stop = Arc::clone(&stop);
        // One thread per connection: a single accept-and-serve loop would
        // serialise the client and hide whether transfers overlap at all.
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if worker_stop.load(Ordering::SeqCst) {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        let store = Arc::clone(&worker_store);
                        let gauge = Arc::clone(&worker_gauge);
                        thread::spawn(move || handle_conn(stream, &store, &gauge));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url,
            store,
            gauge,
            stop,
            handle: Some(handle),
        }
    }

    fn peak_concurrent_puts(&self) -> usize {
        self.gauge.peak.load(Ordering::SeqCst)
    }

    fn put(&self, path: &str, body: &str) {
        self.store
            .lock()
            .unwrap()
            .files
            .insert(path.to_string(), body.as_bytes().to_vec());
    }

    fn get(&self, path: &str) -> Option<String> {
        self.store
            .lock()
            .unwrap()
            .files
            .get(path)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    fn manifest_paths(&self) -> Vec<String> {
        let raw = self
            .get(".qiwo-sync-manifest.json")
            .expect("remote manifest was published");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("manifest is valid JSON");
        let mut paths: Vec<String> = value["files"]
            .as_object()
            .expect("files object")
            .keys()
            .cloned()
            .collect();
        paths.sort();
        paths
    }
}

impl Drop for DavStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept loop.
        let _ =
            std::net::TcpStream::connect(self.base_url.replace("http://", "").replace("/dav", ""));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_conn(mut stream: TcpStream, store: &Arc<Mutex<Store>>, gauge: &PutGauge) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            break;
        }
        if header.trim().is_empty() {
            break;
        }
        if let Some(value) = header.to_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).unwrap();
    }

    // "/dav/a/b" -> "a/b"; percent-decoding is not needed, the tests use ASCII.
    let key = target
        .trim_start_matches("/dav")
        .trim_start_matches('/')
        .to_string();

    let (status, payload): (u16, Vec<u8>) = match method.as_str() {
        // Every collection is reported as existing so the engine never MKCOLs.
        "PROPFIND" => (207, Vec::new()),
        "MKCOL" => (201, Vec::new()),
        "PUT" => {
            gauge.enter();
            // A short hold makes overlap observable; without it a fast local
            // handler could finish before the next request even arrives.
            thread::sleep(std::time::Duration::from_millis(40));
            store.lock().unwrap().files.insert(key, body);
            gauge.leave();
            (201, Vec::new())
        }
        "GET" => match store.lock().unwrap().files.get(&key) {
            Some(bytes) => (200, bytes.clone()),
            None => (404, Vec::new()),
        },
        _ => (405, Vec::new()),
    };

    let reason = match status {
        200 => "OK",
        201 => "Created",
        207 => "Multi-Status",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&payload);
    let _ = stream.flush();
}

// ------------------------------------------------------------------- fixtures

fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("qiwo-sync-merge-{name}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn read(dir: &Path, relative: &str) -> String {
    std::fs::read_to_string(dir.join(relative)).unwrap()
}

fn run(mode: SyncMode, user_dir: &Path, base_url: &str) -> qiwo_sync::types::SyncSummary {
    let request = SyncRequest {
        frontend: Frontend::Weasel,
        rime_user_dir: user_dir.to_path_buf(),
        remote_url: Some(base_url.to_string()),
        username: None,
        password: None,
        device_id: "test-device".into(),
        mode,
        frost_dir: None,
        dry_run: false,
    };

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(SyncEngine::new().execute(request))
        .expect("sync succeeded")
}

// ---------------------------------------------------------------------- tests

#[test]
fn first_sync_uploads_local_files_and_publishes_a_manifest() {
    let dav = DavStub::start();
    let user_dir = temp_dir("first");
    write(&user_dir, "default.custom.yaml", "patch:\n  a: 1\n");
    write(
        &user_dir,
        "sync/test-device/dict.userdb.txt",
        "local dict\n",
    );
    // Distributed data must not be uploaded.
    write(&user_dir, "rime_frost.dict.yaml", "big dictionary\n");

    let summary = run(SyncMode::Sync, &user_dir, &dav.base_url);

    assert_eq!(summary.uploaded, 2, "{:?}", summary.messages);
    assert_eq!(summary.downloaded, 0);
    assert_eq!(dav.get("default.custom.yaml").unwrap(), "patch:\n  a: 1\n");
    assert_eq!(
        dav.manifest_paths(),
        vec![
            "default.custom.yaml".to_string(),
            "sync/test-device/dict.userdb.txt".to_string()
        ],
        "the distributed dictionary must stay out of the manifest"
    );

    let _ = std::fs::remove_dir_all(user_dir);
}

/// Transfers must actually overlap. They used to run one round trip at a time,
/// which on a mobile link dominates the wall clock once `sync/` holds a snapshot
/// set per device.
#[test]
fn transfers_run_concurrently_within_the_configured_bound() {
    let dav = DavStub::start();
    let user_dir = temp_dir("concurrent");
    for i in 0..8 {
        write(
            &user_dir,
            &format!("sync/test-device/dict-{i}.userdb.txt"),
            &format!("snapshot {i}\n"),
        );
    }

    let summary = run(SyncMode::Sync, &user_dir, &dav.base_url);
    assert_eq!(summary.uploaded, 8, "{:?}", summary.messages);

    let peak = dav.peak_concurrent_puts();
    assert!(
        peak > 1,
        "transfers were serialised (peak in-flight PUT = {peak})"
    );
    assert!(
        peak <= 4,
        "concurrency bound exceeded (peak in-flight PUT = {peak}, limit 4)"
    );

    let _ = std::fs::remove_dir_all(user_dir);
}

/// A snapshot larger than any single buffer must round-trip intact — the upload
/// streams from disk and the download streams into the staging file, so neither
/// holds the whole thing in memory.
#[test]
fn a_multi_megabyte_file_round_trips_through_streaming() {
    let dav = DavStub::start();
    let user_dir = temp_dir("streaming");
    // Not uniform, so a truncated or duplicated chunk would change the content.
    let big: String = (0..200_000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    write(&user_dir, "sync/test-device/big.userdb.txt", &big);

    let summary = run(SyncMode::Sync, &user_dir, &dav.base_url);
    assert_eq!(summary.uploaded, 1, "{:?}", summary.messages);
    assert_eq!(
        dav.get("sync/test-device/big.userdb.txt").unwrap(),
        big,
        "uploaded body must match byte for byte"
    );

    // Now pull it back into an empty directory.
    let fresh = temp_dir("streaming-pull");
    let pulled = run(SyncMode::Pull, &fresh, &dav.base_url);
    assert_eq!(pulled.downloaded, 1, "{:?}", pulled.messages);
    assert_eq!(read(&fresh, "sync/test-device/big.userdb.txt"), big);

    let _ = std::fs::remove_dir_all(user_dir);
    let _ = std::fs::remove_dir_all(fresh);
}

#[test]
fn remote_only_file_is_downloaded() {
    let dav = DavStub::start();
    let user_dir = temp_dir("remote-only");
    dav.put("weasel.custom.yaml", "patch:\n  from: remote\n");
    dav.put(
        ".qiwo-sync-manifest.json",
        &manifest_json(&[("weasel.custom.yaml", "patch:\n  from: remote\n")]),
    );

    let summary = run(SyncMode::Sync, &user_dir, &dav.base_url);

    assert_eq!(summary.downloaded, 1, "{:?}", summary.messages);
    assert_eq!(
        read(&user_dir, "weasel.custom.yaml"),
        "patch:\n  from: remote\n"
    );

    let _ = std::fs::remove_dir_all(user_dir);
}

#[test]
fn both_sides_changed_backs_up_local_and_keeps_remote() {
    let dav = DavStub::start();
    let user_dir = temp_dir("conflict");

    // Establish a shared baseline first.
    write(&user_dir, "default.custom.yaml", "base\n");
    run(SyncMode::Sync, &user_dir, &dav.base_url);

    // Then diverge on both sides.
    write(&user_dir, "default.custom.yaml", "local edit\n");
    dav.put("default.custom.yaml", "remote edit\n");
    dav.put(
        ".qiwo-sync-manifest.json",
        &manifest_json(&[("default.custom.yaml", "remote edit\n")]),
    );

    let summary = run(SyncMode::Sync, &user_dir, &dav.base_url);

    assert_eq!(summary.conflicts_backed_up, 1, "{:?}", summary.messages);
    assert_eq!(
        read(&user_dir, "default.custom.yaml"),
        "remote edit\n",
        "remote wins the conflict"
    );

    let backups = user_dir.join(".qiwo-sync").join("backups");
    let backed_up: Vec<PathBuf> = walk(&backups);
    assert_eq!(backed_up.len(), 1, "exactly one backup: {backed_up:?}");
    assert_eq!(
        std::fs::read_to_string(&backed_up[0]).unwrap(),
        "local edit\n",
        "the overwritten local version is what got backed up"
    );

    let _ = std::fs::remove_dir_all(user_dir);
}

/// The regression that motivated `MergeScope`: a dictionary-only run used to
/// republish a manifest built from a full local scan, so anything another device
/// had uploaded but this one had never pulled vanished from it.
#[test]
fn sync_user_dict_keeps_remote_only_config_in_the_published_manifest() {
    let dav = DavStub::start();
    let user_dir = temp_dir("user-dict");
    write(
        &user_dir,
        "sync/test-device/dict.userdb.txt",
        "local dict\n",
    );

    // Another device published a config file this one has never pulled.
    dav.put("other.custom.yaml", "patch:\n  from: other-device\n");
    dav.put(
        ".qiwo-sync-manifest.json",
        &manifest_json(&[("other.custom.yaml", "patch:\n  from: other-device\n")]),
    );

    run(SyncMode::SyncUserDict, &user_dir, &dav.base_url);

    assert!(
        dav.manifest_paths()
            .contains(&"other.custom.yaml".to_string()),
        "a user-dict run must not drop out-of-scope remote entries, got {:?}",
        dav.manifest_paths()
    );
    assert!(
        !user_dir.join("other.custom.yaml").exists(),
        "and it must not download them either"
    );

    let _ = std::fs::remove_dir_all(user_dir);
}

#[test]
fn a_corrupt_local_manifest_fails_instead_of_silently_overwriting() {
    let dav = DavStub::start();
    let user_dir = temp_dir("corrupt");
    write(&user_dir, "default.custom.yaml", "local\n");
    write(&user_dir, ".qiwo-sync/manifest.json", "{ not json");

    let request = SyncRequest {
        frontend: Frontend::Weasel,
        rime_user_dir: user_dir.clone(),
        remote_url: Some(dav.base_url.clone()),
        username: None,
        password: None,
        device_id: "test-device".into(),
        mode: SyncMode::Sync,
        frost_dir: None,
        dry_run: false,
    };
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(SyncEngine::new().execute(request));

    let error = format!(
        "{:#}",
        result.expect_err("a corrupt baseline must not be ignored")
    );
    assert!(
        error.contains("corrupt"),
        "the error should say what is wrong: {error}"
    );

    let _ = std::fs::remove_dir_all(user_dir);
}

// --------------------------------------------------------------------- helpers

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if !dir.exists() {
        return found;
    }
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// Builds a remote manifest whose hashes actually match the given contents, so
/// the engine's "same hash on both sides" shortcut behaves realistically.
fn manifest_json(files: &[(&str, &str)]) -> String {
    use sha2::{Digest, Sha256};

    let entries: serde_json::Map<String, serde_json::Value> = files
        .iter()
        .map(|(path, contents)| {
            let sha = format!("{:x}", Sha256::digest(contents.as_bytes()));
            (
                (*path).to_string(),
                serde_json::json!({
                    "relativePath": path,
                    "size": contents.len(),
                    "sha256": sha,
                    "lastWriteUtc": "2026-08-31T00:00:00Z",
                }),
            )
        })
        .collect();

    serde_json::json!({
        "version": 1,
        "deviceId": "other-device",
        "frontend": "Weasel",
        "updatedAtUtc": "2026-08-31T00:00:00Z",
        "files": entries,
    })
    .to_string()
}
