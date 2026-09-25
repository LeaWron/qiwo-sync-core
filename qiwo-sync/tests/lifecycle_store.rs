use chrono::Utc;
use qiwo_sync::lifecycle::{ManagedState, ObjectRef, Selection, store::Store};
use std::collections::BTreeSet;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};
struct Reply {
    method: &'static str,
    status: u16,
    etag: Option<&'static str>,
    body: String,
    disconnect: bool,
}
fn serve(replies: Vec<Reply>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let root = format!("http://{}/dav", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        for reply in replies {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                if let Ok((socket, _)) = listener.accept() {
                    break socket;
                }
                assert!(Instant::now() < deadline, "expected request did not arrive");
                thread::sleep(Duration::from_millis(2));
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut raw = Vec::new();
            let mut byte = [0];
            while !raw.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                raw.push(byte[0]);
            }
            let header = String::from_utf8(raw).unwrap();
            assert_eq!(
                header.lines().next().unwrap(),
                format!("{} /dav/.qiwo-managed-v2/state.json HTTP/1.1", reply.method)
            );
            let header = header.to_ascii_lowercase();
            let len = header
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length:")
                        .map(|n| n.trim().parse().unwrap())
                })
                .unwrap_or(0);
            let mut body = vec![0; len];
            socket.read_exact(&mut body).unwrap();
            if reply.method == "PUT" {
                assert!(header.contains("if-match: \"one\"\r\n"));
                let state: ManagedState = serde_json::from_slice(&body).unwrap();
                state.validate().unwrap();
                assert!(!state.files.contains_key("old.custom.yaml"));
                assert!(state.trash.contains_key("delete1"));
            }
            if reply.disconnect {
                continue;
            }
            let etag = reply
                .etag
                .map(|s| format!("ETag: {s}\r\n"))
                .unwrap_or_default();
            write!(
                socket,
                "HTTP/1.1 {} Test\r\n{etag}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.status,
                reply.body.len(),
                reply.body
            )
            .unwrap();
        }
    });
    (root, worker)
}
fn initial() -> ManagedState {
    let mut state = ManagedState::default();
    state.files.insert(
        "old.custom.yaml".into(),
        ObjectRef {
            sha256: "a".repeat(64),
            size: 12,
        },
    );
    state
}
fn get(state: &ManagedState, etag: Option<&'static str>) -> Reply {
    Reply {
        method: "GET",
        status: 200,
        etag,
        body: serde_json::to_string(state).unwrap(),
        disconnect: false,
    }
}
fn put(status: u16, disconnect: bool) -> Reply {
    Reply {
        method: "PUT",
        status,
        etag: None,
        body: String::new(),
        disconnect,
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
#[test]
fn commit_uses_conditional_single_state_write_and_never_deletes_objects() {
    let state = initial();
    let plan = state
        .plan(
            "current",
            "delete1",
            Selection::DeleteFiles {
                paths: BTreeSet::from(["old.custom.yaml".into()]),
            },
        )
        .unwrap();
    let (url, server) = serve(vec![get(&state, Some("\"one\"")), put(204, false)]);
    let result = runtime()
        .block_on(Store::new(&url, "", "").unwrap().commit(&plan))
        .unwrap();
    assert_eq!(result.revision, 1);
    server.join().unwrap();
}
#[test]
fn concurrent_writer_412_does_not_retry_or_overwrite() {
    let state = initial();
    let plan = state
        .plan(
            "current",
            "delete1",
            Selection::DeleteFiles {
                paths: BTreeSet::from(["old.custom.yaml".into()]),
            },
        )
        .unwrap();
    let (url, server) = serve(vec![get(&state, Some("\"one\"")), put(412, false)]);
    let result = runtime()
        .block_on(Store::new(&url, "", "").unwrap().commit(&plan))
        .unwrap_err();
    assert!(result.to_string().contains("其他设备"));
    server.join().unwrap();
}
#[test]
fn lost_commit_response_can_be_reconciled_using_same_operation_id() {
    let state = initial();
    let plan = state
        .plan(
            "current",
            "delete1",
            Selection::DeleteFiles {
                paths: BTreeSet::from(["old.custom.yaml".into()]),
            },
        )
        .unwrap();
    let committed = state.apply(&plan, Utc::now()).unwrap();
    let (url, server) = serve(vec![
        get(&state, Some("\"one\"")),
        put(204, true),
        get(&committed, Some("\"two\"")),
    ]);
    let store = Store::new(&url, "", "").unwrap();
    let rt = runtime();
    assert!(rt.block_on(store.commit(&plan)).is_err());
    assert_eq!(rt.block_on(store.commit(&plan)).unwrap().revision, 1);
    server.join().unwrap();
}
#[test]
fn missing_weak_or_unquoted_etag_blocks_all_writes() {
    let state = initial();
    let plan = state
        .plan(
            "current",
            "delete1",
            Selection::DeleteFiles {
                paths: BTreeSet::from(["old.custom.yaml".into()]),
            },
        )
        .unwrap();
    for etag in [None, Some("W/\"one\""), Some("unquoted")] {
        let (url, server) = serve(vec![get(&state, etag)]);
        assert!(
            runtime()
                .block_on(Store::new(&url, "", "").unwrap().commit(&plan))
                .unwrap_err()
                .to_string()
                .contains("ETag")
        );
        server.join().unwrap();
    }
}
