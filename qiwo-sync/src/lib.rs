pub mod file_selector;
pub mod frost_init;
pub mod installation;
pub mod jni_bridge;
pub mod sync_engine;
pub mod types;
pub mod webdav_client;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use sync_engine::SyncEngine;
use types::SyncRequest;

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
            return to_c_string(&format!(
                r#"{{"error":"Invalid UTF-8: {}"}}"#,
                e
            ));
        }
    };

    let request: SyncRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return to_c_string(&format!(r#"{{"error":"Parse request: {}"}}"#, e));
        }
    };

    let engine = SyncEngine::new();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            return to_c_string(&format!(
                r#"{{"error":"Runtime: {}"}}"#,
                e
            ));
        }
    };

    match rt.block_on(engine.execute(request)) {
        Ok(summary) => {
            let json = serde_json::to_string(&summary).unwrap_or_else(|e| {
                format!(r#"{{"error":"Serialize: {}"}}"#, e)
            });
            to_c_string(&json)
        }
        Err(e) => to_c_string(&format!(r#"{{"error":"{}"}}"#, e)),
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
