use chrono::Utc;
use qiwo_sync::{
    inventory::{FileFacts, Inventory, InventoryFile},
    lifecycle::{
        self, ManagedState, Selection, TransportProfile, local, migration,
        store::{Store, reference},
        transfer,
    },
    types::{Frontend, SyncMode, SyncRequest},
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
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
    requests: Vec<(String, String)>,
    ignore_conditions: bool,
    ignore_create_condition: bool,
    unquoted_etag: bool,
    corrupt_objects: bool,
    drop_state_reply: bool,
    ignore_move_condition: bool,
    move_conflict_without_target: bool,
    corrupt_staging: bool,
    unconditional_final_put: bool,
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
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut byte = [0];
                while !bytes.ends_with(b"\r\n\r\n") {
                    if socket.read_exact(&mut byte).is_err() {
                        return;
                    }
                    bytes.push(byte[0]);
                }
                let header = String::from_utf8(bytes).unwrap();
                let mut words = header.lines().next().unwrap().split_whitespace();
                let method = words.next().unwrap().to_owned();
                let path = words
                    .next()
                    .unwrap()
                    .strip_prefix("/dav/")
                    .unwrap()
                    .to_owned();
                let headers: BTreeMap<_, _> = header
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.to_lowercase(), v.trim().to_owned()))
                    .collect();
                let len = headers
                    .get("content-length")
                    .map(|s| s.parse().unwrap())
                    .unwrap_or(0);
                let mut body = vec![0; len];
                socket.read_exact(&mut body).unwrap();
                let mut remote = data.lock().unwrap();
                remote.requests.push((method.clone(), path.clone()));
                let current = remote.files.get(&path).cloned();
                let etag = current.as_ref().map(|b| {
                    if remote.unquoted_etag {
                        reference(b).sha256
                    } else {
                        format!("\"{}\"", reference(b).sha256)
                    }
                });
                let (status, response) = match method.as_str() {
                    "GET" => current.map(|b| (200, b)).unwrap_or((404, vec![])),
                    "MKCOL" => (201, vec![]),
                    "PUT" => {
                        if (path == ".qiwo-managed-v2/state.json"
                            || path.starts_with(".qiwo-managed-v2/objects/"))
                            && !headers.contains_key("if-match")
                        {
                            remote.unconditional_final_put = true;
                        }
                        let rejected = !remote.ignore_conditions
                            && ((headers.get("if-none-match").is_some_and(|s| s == "*")
                                && current.is_some()
                                && !remote.ignore_create_condition)
                                || headers
                                    .get("if-match")
                                    .is_some_and(|s| Some(s) != etag.as_ref()));
                        if rejected {
                            (412, vec![])
                        } else {
                            if (remote.corrupt_objects
                                && path.starts_with(".qiwo-managed-v2/objects/"))
                                || (remote.corrupt_staging
                                    && path.starts_with(".qiwo-managed-v2/staging/"))
                            {
                                body = b"corrupt".to_vec();
                            }
                            remote.files.insert(path.clone(), body);
                            if path == ".qiwo-managed-v2/state.json" && remote.drop_state_reply {
                                remote.drop_state_reply = false;
                                continue;
                            }
                            (if current.is_some() { 204 } else { 201 }, vec![])
                        }
                    }
                    "MOVE" => {
                        assert_eq!(headers.get("overwrite").map(String::as_str), Some("F"));
                        let target_url = reqwest::Url::parse(&headers["destination"]).unwrap();
                        let target = target_url.path().strip_prefix("/dav/").unwrap().to_owned();
                        if (remote.move_conflict_without_target
                            && target.starts_with(".qiwo-managed-v2/objects/"))
                            || (remote.files.contains_key(&target) && !remote.ignore_move_condition)
                        {
                            (409, vec![])
                        } else if let Some(mut content) = current {
                            let existed = remote.files.contains_key(&target);
                            if remote.corrupt_objects
                                && target.starts_with(".qiwo-managed-v2/objects/")
                            {
                                content = b"corrupt".to_vec();
                            }
                            remote.files.insert(target.clone(), content);
                            remote.files.remove(&path);
                            if target == ".qiwo-managed-v2/state.json" && remote.drop_state_reply {
                                remote.drop_state_reply = false;
                                continue;
                            }
                            (if existed { 204 } else { 201 }, vec![])
                        } else {
                            (404, vec![])
                        }
                    }
                    _ => panic!("unexpected mutation {method}"),
                };
                let etag = if method == "GET" {
                    etag.map(|e| {
                        let e = if remote.unquoted_etag {
                            e.trim_matches('"').to_owned()
                        } else {
                            e
                        };
                        format!("ETag: {e}\r\n")
                    })
                    .unwrap_or_default()
                } else {
                    String::new()
                };
                drop(remote);
                write!(socket,"HTTP/1.1 {status} Test\r\n{etag}Content-Length: {}\r\nConnection: close\r\n\r\n",response.len()).unwrap();
                socket.write_all(&response).unwrap();
            }
        });
        Self {
            url,
            remote,
            stop,
            worker: Some(worker),
        }
    }
    fn store(&self) -> Store {
        Store::new(&self.url, "", "").unwrap()
    }
    fn put(&self, path: &str, bytes: &[u8]) {
        self.remote
            .lock()
            .unwrap()
            .files
            .insert(path.into(), bytes.to_vec());
    }
    fn inventory(&self) -> Inventory {
        let files = self
            .remote
            .lock()
            .unwrap()
            .files
            .iter()
            .filter(|(p, _)| !p.starts_with('.'))
            .map(|(path, bytes)| InventoryFile {
                path: path.clone(),
                category: "other".into(),
                device_id: None,
                local: None,
                remote: Some(FileFacts {
                    size: Some(bytes.len() as u64),
                    modified_at: None,
                }),
                tracked: Some(true),
                sync_eligible: Some(true),
                issues: vec![],
            })
            .collect();
        Inventory {
            scanned_at: Utc::now(),
            current_device_id: "current".into(),
            remote_status: "ok".into(),
            manifest_status: "ok".into(),
            local_complete: true,
            remote_complete: true,
            local_bytes: 0,
            remote_bytes: 0,
            unknown_remote_sizes: 0,
            warnings: vec![],
            devices: vec![],
            files,
        }
    }
    async fn migrate(&self) -> (migration::MigrationPlan, ManagedState) {
        let store = self.store();
        let plan = migration::preview(&store, &self.inventory(), "current", "migration")
            .await
            .unwrap();
        let state = migration::execute(&store, &plan).await.unwrap();
        (plan, state)
    }
}
impl Drop for Dav {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Local(std::path::PathBuf);
impl Local {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("qiwo-managed-{}", lifecycle::operation_id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn put(&self, path: &str, data: &[u8]) {
        local::atomic_write(&self.0, path, data).unwrap();
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

fn compatible_dav() -> Dav {
    let dav = Dav::new();
    {
        let mut remote = dav.remote.lock().unwrap();
        remote.unquoted_etag = true;
        remote.ignore_create_condition = true;
    }
    dav.put("default.custom.yaml", b"original");
    dav.put("sync/old/words.userdb.txt", b"old words");
    dav
}

async fn compatible_plan(dav: &Dav, root: &Local, id: &str) -> (Store, migration::MigrationPlan) {
    let store = dav.store().with_journal(&root.0);
    let plan = migration::preview_with_profile(
        &store,
        &dav.inventory(),
        "current",
        id,
        TransportProfile::OpaqueMove,
    )
    .await
    .unwrap();
    (store, plan)
}

#[tokio::test]
async fn compatible_migration_transfer_retire_restore_and_stale_client() {
    let dav = compatible_dav();
    let a = Local::new();
    let (store, plan) = compatible_plan(&dav, &a, "compatible-migration").await;
    let state = migration::execute(&store, &plan).await.unwrap();
    assert_eq!(state.protocol_version, 3);
    assert_eq!(state.profile(), TransportProfile::OpaqueMove);
    local::enroll(&a.0, &store, &state, "current", &plan.operation_id).unwrap();
    transfer::execute(&a.request(&dav)).await.unwrap();
    a.put("custom_phrase.txt", b"new upload");
    transfer::execute(&a.request(&dav)).await.unwrap();
    let b = Local::new();
    let mut request_b = b.request(&dav);
    request_b.device_id = "old".into();
    let state = store.load().await.unwrap().0;
    local::enroll(&b.0, &store, &state, "old", &plan.operation_id).unwrap();
    transfer::execute(&request_b).await.unwrap();
    let retire = state
        .plan(
            "current",
            "retire-old",
            Selection::RetireDevice {
                device_id: "old".into(),
            },
        )
        .unwrap();
    store.commit(&retire).await.unwrap();
    b.put("sync/old/words.userdb.txt", b"offline edit");
    assert!(transfer::execute(&request_b).await.is_err());
    assert!(!b.0.join("sync/old/words.userdb.txt").exists());
    let deleted = store.load().await.unwrap().0;
    assert!(
        store
            .commit(
                &state
                    .plan(
                        "current",
                        "stale-delete",
                        Selection::DeleteFiles {
                            paths: ["default.custom.yaml".into()].into()
                        }
                    )
                    .unwrap()
            )
            .await
            .is_err()
    );
    let restore = deleted
        .plan(
            "current",
            "restore-old",
            Selection::Restore {
                batch_id: "retire-old".into(),
            },
        )
        .unwrap();
    store.commit(&restore).await.unwrap();
    transfer::execute(&a.request(&dav)).await.unwrap();
    assert_eq!(
        std::fs::read(a.0.join("sync/old/words.userdb.txt")).unwrap(),
        b"old words"
    );
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(a.0.join(".qiwo-sync/managed-v2.json")).unwrap())
            .unwrap();
    assert_eq!(marker["protocolVersion"], 3);
    assert!(marker["transportProfile"].is_string());
    let remote = dav.remote.lock().unwrap();
    assert!(!remote.unconditional_final_put);
    assert_eq!(remote.files["default.custom.yaml"], b"original");
    assert_eq!(remote.files["sync/old/words.userdb.txt"], b"old words");
}

#[tokio::test]
async fn compatible_capability_failures_cannot_publish_state() {
    for mode in ["cas", "move", "staging", "object", "false-conflict"] {
        let dav = compatible_dav();
        let root = Local::new();
        let (store, plan) = compatible_plan(&dav, &root, "migration").await;
        {
            let mut remote = dav.remote.lock().unwrap();
            match mode {
                "cas" => remote.ignore_conditions = true,
                "move" => remote.ignore_move_condition = true,
                "staging" => remote.corrupt_staging = true,
                "object" => remote.corrupt_objects = true,
                _ => remote.move_conflict_without_target = true,
            }
        }
        assert!(migration::execute(&store, &plan).await.is_err(), "{mode}");
        let remote = dav.remote.lock().unwrap();
        assert!(
            !remote.files.contains_key(".qiwo-managed-v2/state.json"),
            "{mode}"
        );
        assert!(!remote.unconditional_final_put, "{mode}");
    }
}

#[tokio::test]
async fn compatible_lost_move_reply_reconciles_and_competing_migration_cannot_replace() {
    let dav = compatible_dav();
    let a = Local::new();
    let b = Local::new();
    let (store_a, plan_a) = compatible_plan(&dav, &a, "migrate-a").await;
    let (store_b, plan_b) = compatible_plan(&dav, &b, "migrate-b").await;
    dav.remote.lock().unwrap().drop_state_reply = true;
    let (a, b) = futures_util::future::join(
        migration::execute(&store_a, &plan_a),
        migration::execute(&store_b, &plan_b),
    )
    .await;
    assert_ne!(a.is_ok(), b.is_ok(), "only one initial migration can win");
    let (store, plan) = if a.is_ok() {
        (&store_a, &plan_a)
    } else {
        (&store_b, &plan_b)
    };
    let first = store.load().await.unwrap().0;
    assert_eq!(
        migration::execute(store, plan)
            .await
            .unwrap()
            .digest()
            .unwrap(),
        first.digest().unwrap()
    );
    assert!(!dav.remote.lock().unwrap().unconditional_final_put);
}

#[tokio::test]
async fn compatible_retry_reuses_durable_upload_journal_and_rejects_corrupt_existing_objects() {
    let dav = compatible_dav();
    let root = Local::new();
    let (store, plan) = compatible_plan(&dav, &root, "migration").await;
    dav.remote.lock().unwrap().move_conflict_without_target = true;
    assert!(migration::execute(&store, &plan).await.is_err());
    let journal_dir = root.0.join(".qiwo-sync/managed-uploads");
    let first: Vec<_> = std::fs::read_dir(&journal_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(first.len(), 1);
    let saved = std::fs::read(&first[0]).unwrap();
    dav.remote.lock().unwrap().move_conflict_without_target = false;
    migration::execute(&store, &plan).await.unwrap();
    assert_eq!(std::fs::read(&first[0]).unwrap(), saved);
    let object_path = format!(".qiwo-managed-v2/objects/{}", reference(b"original").sha256);
    dav.put(&object_path, b"corrupt");
    assert!(
        store
            .put_object_for(b"original".to_vec(), TransportProfile::OpaqueMove)
            .await
            .is_err()
    );
    assert_eq!(dav.remote.lock().unwrap().files[&object_path], b"corrupt");
}

#[tokio::test]
async fn legacy_preview_and_enrollment_cannot_silently_adopt_compatible_protocol() {
    let dav = compatible_dav();
    let root = Local::new();
    let (store, mut plan) = compatible_plan(&dav, &root, "migration").await;
    plan.transport_profile = None;
    assert!(
        migration::execute(&store, &plan)
            .await
            .unwrap_err()
            .to_string()
            .contains("旧版本")
    );
    assert!(
        !dav.remote
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|(m, _)| m != "GET")
    );
    plan.transport_profile = Some(TransportProfile::OpaqueMove);
    let state = migration::execute(&store, &plan).await.unwrap();
    local::enroll(&root.0, &store, &state, "current", "migration").unwrap();
    let mut marker = local::enrollment(&root.0).unwrap().unwrap();
    marker.protocol_version = None;
    marker.transport_profile = None;
    assert!(marker.verify(&store, &state, "current").is_err());
}

#[tokio::test]
async fn migration_keeps_legacy_and_verifies_all_objects_before_publication() {
    let dav = Dav::new();
    dav.put("default.custom.yaml", b"legacy");
    dav.put("sync/old/words.userdb.txt", b"words");
    dav.put("rime_frost.dict.yaml", b"distributed");
    let before = dav.remote.lock().unwrap().files.clone();
    let (plan, state) = dav.migrate().await;
    assert_eq!(state.files.len(), 2);
    assert_eq!(plan.excluded, vec!["rime_frost.dict.yaml"]);
    let remote = dav.remote.lock().unwrap();
    for (path, bytes) in before {
        assert_eq!(remote.files[&path], bytes);
    }
    assert!(
        remote
            .requests
            .iter()
            .all(|(method, path)| method == "GET" || path.starts_with(".qiwo-managed-v2/"))
    );
}

#[tokio::test]
async fn servers_ignoring_conditions_and_corrupt_objects_never_get_state() {
    for corrupt in [false, true] {
        let dav = Dav::new();
        dav.put("custom_phrase.txt", b"phrase");
        let plan = migration::preview(&dav.store(), &dav.inventory(), "current", "migration")
            .await
            .unwrap();
        {
            let mut r = dav.remote.lock().unwrap();
            r.ignore_conditions = !corrupt;
            r.corrupt_objects = corrupt;
        }
        assert!(migration::execute(&dav.store(), &plan).await.is_err());
        assert!(
            !dav.remote
                .lock()
                .unwrap()
                .files
                .contains_key(".qiwo-managed-v2/state.json")
        );
    }
}

#[tokio::test]
async fn changed_legacy_files_abort_and_lost_publication_resumes_same_plan() {
    let dav = Dav::new();
    dav.put("custom_phrase.txt", b"before");
    let plan = migration::preview(&dav.store(), &dav.inventory(), "current", "migration")
        .await
        .unwrap();
    dav.put("custom_phrase.txt", b"after");
    assert!(migration::execute(&dav.store(), &plan).await.is_err());
    assert!(
        !dav.remote
            .lock()
            .unwrap()
            .files
            .contains_key(".qiwo-managed-v2/state.json")
    );
    dav.put("custom_phrase.txt", b"before");
    dav.remote.lock().unwrap().drop_state_reply = true;
    assert!(migration::execute(&dav.store(), &plan).await.is_err());
    assert_eq!(
        migration::execute(&dav.store(), &plan)
            .await
            .unwrap()
            .revision,
        1
    );
    let mut other = plan.clone();
    other.operation_id = "other".into();
    assert!(migration::execute(&dav.store(), &other).await.is_err());
}

#[tokio::test]
async fn deletion_cannot_be_revived_by_stale_push_and_restore_downloads_original() {
    let dav = Dav::new();
    dav.put("default.custom.yaml", b"original");
    dav.put("sync/old/words.userdb.txt", b"words");
    let (_, state) = dav.migrate().await;
    let local = Local::new();
    let request = local.request(&dav);
    local::enroll(&local.0, &dav.store(), &state, "current", "migration").unwrap();
    transfer::execute(&request).await.unwrap();
    let (state, _) = dav.store().load().await.unwrap();
    let plan = state
        .plan(
            "current",
            "delete",
            Selection::DeleteFiles {
                paths: ["default.custom.yaml".into()].into(),
            },
        )
        .unwrap();
    dav.store().commit(&plan).await.unwrap();
    local.put("default.custom.yaml", b"stale local");
    local.put("sync/unmanaged/words.userdb.txt", b"zombie");
    let mut push = request.clone();
    push.mode = SyncMode::Push;
    transfer::execute(&push).await.unwrap();
    assert!(!local.0.join("default.custom.yaml").exists());
    assert!(!local.0.join("sync/unmanaged/words.userdb.txt").exists());
    let (state, _) = dav.store().load().await.unwrap();
    assert!(!state.files.contains_key("default.custom.yaml"));
    assert!(state.receipts.contains_key("delete"));
    let restore = state
        .plan(
            "current",
            "restore",
            Selection::Restore {
                batch_id: "delete".into(),
            },
        )
        .unwrap();
    dav.store().commit(&restore).await.unwrap();
    transfer::execute(&request).await.unwrap();
    assert_eq!(
        std::fs::read(local.0.join("default.custom.yaml")).unwrap(),
        b"original"
    );
    assert!(local.0.join(".qiwo-sync/managed-recovery").is_dir());
}

#[tokio::test]
async fn retired_actor_stops_and_endpoint_change_or_missing_state_never_falls_back() {
    let dav = Dav::new();
    dav.put("sync/current/words.userdb.txt", b"mine");
    let (_, state) = dav.migrate().await;
    let local = Local::new();
    local::enroll(&local.0, &dav.store(), &state, "current", "migration").unwrap();
    local.put("sync/current/words.userdb.txt", b"mine");
    let request = local.request(&dav);
    let retire = state
        .plan(
            "other",
            "retire",
            Selection::RetireDevice {
                device_id: "current".into(),
            },
        )
        .unwrap();
    dav.store().commit(&retire).await.unwrap();
    assert!(transfer::prepare(&request).await.is_err());
    assert!(!local.0.join("sync/current/words.userdb.txt").exists());
    let other = Dav::new();
    let mut changed = request.clone();
    changed.remote_url = Some(other.url.clone());
    assert!(transfer::execute(&changed).await.is_err());
    dav.remote
        .lock()
        .unwrap()
        .files
        .remove(".qiwo-managed-v2/state.json");
    assert!(transfer::execute(&request).await.is_err());
    assert!(
        dav.remote
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, path)| method == "GET" || path.starts_with(".qiwo-managed-v2/"))
    );
}

#[tokio::test]
async fn two_clients_preserve_lifecycle_and_backup_conflicting_local_edits() {
    let dav = Dav::new();
    dav.put("default.custom.yaml", b"initial");
    let (_, state) = dav.migrate().await;
    let a = Local::new();
    let b = Local::new();
    for root in [&a, &b] {
        local::enroll(&root.0, &dav.store(), &state, "current", "migration").unwrap();
        transfer::execute(&root.request(&dav)).await.unwrap();
    }
    a.put("default.custom.yaml", b"a edit");
    transfer::execute(&a.request(&dav)).await.unwrap();
    b.put("default.custom.yaml", b"b edit");
    let summary = transfer::execute(&b.request(&dav)).await.unwrap();
    assert_eq!(summary.conflicts_backed_up, 1);
    assert_eq!(
        std::fs::read(b.0.join("default.custom.yaml")).unwrap(),
        b"a edit"
    );
    assert!(
        dav.store()
            .load()
            .await
            .unwrap()
            .0
            .receipts
            .contains_key("migration")
    );
}

#[cfg(unix)]
#[test]
fn enrollment_and_quarantine_reject_symlinks_without_touching_target() {
    let local = Local::new();
    let outside = Local::new();
    outside.put("secret", b"untouched");
    std::os::unix::fs::symlink(&outside.0, local.0.join("sync")).unwrap();
    assert!(local::quarantine(&local.0, &ManagedState::default(), "current").is_err());
    assert_eq!(
        std::fs::read(outside.0.join("secret")).unwrap(),
        b"untouched"
    );
}

#[tokio::test]
async fn corrupt_download_and_account_change_preserve_local_content() {
    let dav = Dav::new();
    dav.put("default.custom.yaml", b"cloud");
    let (_, state) = dav.migrate().await;
    let local = Local::new();
    local::enroll(&local.0, &dav.store(), &state, "current", "migration").unwrap();
    local.put("default.custom.yaml", b"local unsaved");
    let object_path = format!(
        ".qiwo-managed-v2/objects/{}",
        state.files["default.custom.yaml"].sha256
    );
    dav.put(&object_path, b"corrupt");
    assert!(transfer::execute(&local.request(&dav)).await.is_err());
    assert_eq!(
        std::fs::read(local.0.join("default.custom.yaml")).unwrap(),
        b"local unsaved"
    );
    let marker = local::enrollment(&local.0).unwrap().unwrap();
    let account = Store::new(&dav.url, "another-account", "rotated-password").unwrap();
    assert!(marker.verify(&account, &state, "current").is_err());
    assert_eq!(
        Store::new(&dav.url, "user", "old").unwrap().endpoint_id(),
        Store::new(&dav.url, "user", "new").unwrap().endpoint_id()
    );
}

#[tokio::test]
async fn explicit_join_binds_preview_and_quarantines_unregistered_foreign_snapshots() {
    use qiwo_sync::lifecycle::job::{self, Job};
    let dav = Dav::new();
    dav.put("default.custom.yaml", b"managed");
    let (_, state) = dav.migrate().await;
    let local = Local::new();
    local.put("default.custom.yaml", b"local copy");
    local.put("sync/zombie/words.userdb.txt", b"old");
    let mut request = local.request(&dav);
    request.device_id = "new-device".into();
    let job = Job::Join {
        migration_id: "migration".into(),
        actor: "new-device".into(),
        endpoint: dav.store().endpoint_id(),
        state_digest: state.digest().unwrap(),
        files: state.files.clone(),
    };
    let mut wrong = request.clone();
    wrong.device_id = "changed".into();
    assert!(job::run(&wrong, &job).await.is_err());
    assert!(local::enrollment(&local.0).unwrap().is_none());
    job::run(&request, &job).await.unwrap();
    assert!(!local.0.join("sync/zombie/words.userdb.txt").exists());
    assert_eq!(
        std::fs::read(local.0.join("default.custom.yaml")).unwrap(),
        b"managed"
    );
    assert!(local.0.join(".qiwo-sync/managed-recovery").is_dir());
}

#[tokio::test]
async fn migration_retry_after_state_loss_cannot_reinitialize_enrolled_space() {
    use qiwo_sync::lifecycle::job::{self, Job};
    let dav = Dav::new();
    dav.put("default.custom.yaml", b"original");
    let (plan, state) = dav.migrate().await;
    let local = Local::new();
    local::enroll(&local.0, &dav.store(), &state, "current", "migration").unwrap();
    dav.remote
        .lock()
        .unwrap()
        .files
        .remove(".qiwo-managed-v2/state.json");
    let offset = dav.remote.lock().unwrap().requests.len();
    assert!(
        job::run(&local.request(&dav), &Job::Migrate { plan })
            .await
            .is_err()
    );
    let remote = dav.remote.lock().unwrap();
    assert!(!remote.files.contains_key(".qiwo-managed-v2/state.json"));
    assert!(
        remote.requests[offset..]
            .iter()
            .all(|(method, _)| method == "GET")
    );
}

#[tokio::test]
async fn unquoted_etag_and_ignored_create_condition_reports_non_retryable_failure() {
    let dav = Dav::new();
    dav.put("default.custom.yaml", b"original");
    {
        let mut remote = dav.remote.lock().unwrap();
        remote.ignore_create_condition = true;
        remote.unquoted_etag = true;
    }
    let store = dav.store();
    let plan = migration::preview(&store, &dav.inventory(), "current", "migration")
        .await
        .unwrap();
    let error = migration::execute(&store, &plan)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("If-None-Match"), "{error}");
    assert!(error.contains("重复重试无效"), "{error}");
    let remote = dav.remote.lock().unwrap();
    assert_eq!(remote.files["default.custom.yaml"], b"original");
    assert!(!remote.files.contains_key(".qiwo-managed-v2/state.json"));
    assert!(
        !remote
            .requests
            .iter()
            .any(|(method, path)| method == "PUT" && !path.starts_with(".qiwo-managed-v2/probes/"))
    );
}
