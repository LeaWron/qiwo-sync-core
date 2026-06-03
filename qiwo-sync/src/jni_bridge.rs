use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jstring;

use crate::sync_engine::SyncEngine;
use crate::types::SyncRequest;

/// JNI entry point: execute sync with JSON request, return JSON result.
///
/// Java signature:
/// `com.qiwo.sync.QiwoSync.nativeSync(String jsonRequest) -> String`
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_qiwo_sync_QiwoSync_nativeSync(
    mut env: JNIEnv,
    _class: JClass,
    json_request: JString,
) -> jstring {
    let input: String = match env.get_string(&json_request) {
        Ok(s) => s.into(),
        Err(e) => {
            let msg = format!(r#"{{"error":"JNI: {}"}}"#, e);
            return env.new_string(msg).unwrap().into_raw();
        }
    };

    let request: SyncRequest = match serde_json::from_str(&input) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!(r#"{{"error":"Parse: {}"}}"#, e);
            return env.new_string(msg).unwrap().into_raw();
        }
    };

    let engine = SyncEngine::new();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            let msg = format!(r#"{{"error":"Runtime: {}"}}"#, e);
            return env.new_string(msg).unwrap().into_raw();
        }
    };

    let result = rt.block_on(engine.execute(request));
    let json = match result {
        Ok(summary) => serde_json::to_string(&summary)
            .unwrap_or_else(|e| format!(r#"{{"error":"Serialize: {}"}}"#, e)),
        Err(e) => format!(r#"{{"error":"{}"}}"#, e),
    };

    env.new_string(json).unwrap().into_raw()
}
