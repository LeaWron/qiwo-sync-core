pub mod file_selector;
pub mod frost_init;
pub mod installation;
pub mod sync_engine;
pub mod types;
pub mod webdav_client;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use sync_engine::SyncEngine;
use types::SyncRequest;

/// Builds a `{"error": "..."}` payload with proper JSON escaping.
///
/// Error text routinely contains characters that are illegal inside a JSON
/// string literal — Windows paths (`C:\Users\...`) and WebDAV XML error bodies
/// (`<?xml version="1.0"?>`) both break a hand-written `format!` template.
pub(crate) fn error_json(message: impl std::fmt::Display) -> String {
    serde_json::json!({ "error": message.to_string() }).to_string()
}

/// C API: Execute sync operation.
/// Takes a JSON request string, returns a JSON result string.
/// Caller must free the returned string with free_c_string().
///
/// # Safety
/// `json_request` must be a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qiwo_sync(json_request: *const c_char) -> *mut c_char {
    let c_str = unsafe { CStr::from_ptr(json_request) };
    let request_str = match c_str.to_str() {
        Ok(s) => s,
        Err(e) => {
            return to_c_string(&error_json(format!("Invalid UTF-8: {e}")));
        }
    };

    let request: SyncRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return to_c_string(&error_json(format!("Parse request: {e}")));
        }
    };

    to_c_string(&run_sync(request))
}

/// Runs one sync request on a private runtime and returns the JSON payload.
///
/// A current-thread runtime is enough here: every step is I/O bound, and a
/// multi-thread runtime would spin up and tear down a worker pool per call.
pub(crate) fn run_sync(request: SyncRequest) -> String {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => return error_json(format!("Runtime: {e}")),
    };

    match rt.block_on(SyncEngine::new().execute(request)) {
        Ok(summary) => serde_json::to_string(&summary)
            .unwrap_or_else(|e| error_json(format!("Serialize: {e}"))),
        // `{:#}` flattens the anyhow context chain into one line.
        Err(e) => error_json(format!("{e:#}")),
    }
}

/// Free a string returned by qiwo_sync().
///
/// # Safety
/// `ptr` must have been returned by qiwo_sync().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_c_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(ptr);
    }
}

fn to_c_string(s: &str) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new("error").unwrap())
        .into_raw()
}

#[cfg(test)]
mod tests {
    use super::error_json;

    /// The old hand-written `format!(r#"{{"error":"{}"}}"#, e)` template produced
    /// invalid JSON for every one of these — they are real error strings this
    /// crate emits on Windows and against a WebDAV server that rejects a request.
    #[test]
    fn error_payloads_stay_parseable_with_hostile_characters() {
        let cases = [
            r#"GET https://dav/x failed: HTTP 401 <?xml version="1.0"?><d:error/>"#,
            r"rime-frost directory does not exist: D:\work\rime-frost",
            r"failed to open C:\Users\Lea\AppData\Roaming\Rime\user.yaml",
            "trailing backslash\\",
            "control\tchars\nand \u{7} bell",
        ];

        for case in cases {
            let payload = error_json(case);
            let parsed: serde_json::Value =
                serde_json::from_str(&payload).unwrap_or_else(|e| panic!("{payload}: {e}"));
            assert_eq!(parsed["error"], case);
        }
    }
}
