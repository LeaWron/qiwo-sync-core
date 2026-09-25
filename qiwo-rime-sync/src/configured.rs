//! Private-file adapter for a frontend-managed full sync. Rime export/import
//! remain the frontend's responsibility; this process only does the network leg.
use qiwo_sync::sync_engine::SyncEngine;
use qiwo_sync::types::{SyncMode, SyncRequest};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    frontend: String,
    #[arg(long)]
    rime_user_dir: PathBuf,
    #[arg(long)]
    device_id: String,
    #[arg(long)]
    config: PathBuf,
    /// Validate settings only; no writes or network requests.
    #[arg(long)]
    check: bool,
    /// Internal native frontend preflight, before each Rime export/import.
    #[arg(long, conflicts_with = "check", requires = "native_managed")]
    prepare_managed: bool,
    /// Internal native frontend serialized transfer or management task.
    #[arg(long)]
    native_managed: bool,
    #[arg(long, requires = "native_managed")]
    managed_request: Option<String>,
}

fn request(args: &Args) -> Result<SyncRequest, &'static str> {
    // Error messages intentionally never echo settings or parse-error input.
    let file = std::fs::File::open(&args.config).map_err(|_| "Cannot read sync settings")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect sync settings")?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err("Invalid sync settings file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Sync settings must be private (mode 0600)");
        }
    }
    let value: serde_json::Value =
        serde_json::from_reader(file).map_err(|_| "Invalid sync settings JSON")?;
    if value.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return Err("WebDAV sync is disabled");
    }
    let url = value
        .get("remoteUrl")
        .and_then(|v| v.as_str())
        .ok_or("Missing remoteUrl")?;
    // Delegate URL parsing to the core's validator; reject credential-bearing
    // URLs so even the network library's errors cannot disclose a password.
    qiwo_sync::webdav_client::validate_private_settings_url(url)
        .map_err(|_| "Use an HTTP(S) URL without credentials, query or fragment")?;
    let username = value
        .get("username")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let password = if let Some(name) = value.get("passwordEnv").and_then(|v| v.as_str()) {
        Some(std::env::var(name).map_err(|_| "Password environment variable is unavailable")?)
    } else {
        value
            .get("password")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    if username.is_some() != password.is_some() {
        return Err("Username and password must be provided together");
    }
    if args.device_id.is_empty()
        || args.device_id == "."
        || args.device_id == ".."
        || qiwo_sync::installation::safe_device_id(&args.device_id) != args.device_id
    {
        return Err("Invalid Rime device ID");
    }
    if !args.rime_user_dir.is_absolute() || !args.rime_user_dir.is_dir() {
        return Err("Rime user directory must be an existing absolute directory");
    }
    Ok(SyncRequest {
        frontend: super::parse_frontend(&args.frontend).map_err(|_| "Unknown frontend")?,
        rime_user_dir: args.rime_user_dir.clone(),
        remote_url: Some(url.to_owned()),
        username: username.map(str::to_owned),
        password,
        device_id: args.device_id.clone(),
        mode: SyncMode::Sync,
        frost_dir: None,
        dry_run: false,
    })
}

pub async fn run(args: &Args) -> i32 {
    let request = match request(args) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("{message}");
            return 3;
        }
    };
    if args.check {
        return 0;
    }
    let managed = async {
        use qiwo_sync::lifecycle::{job::Job, local, transfer};
        if let Some(id) = &args.managed_request {
            if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Invalid request ID".into());
            }
            let path = format!(".qiwo-sync/managed-requests/{id}.json");
            let result: Result<(), String> = async {
                let bytes = local::read(&request.rime_user_dir, &path, 4 * 1024 * 1024)
                    .map_err(|e| e.to_string())?;
                let job: Job = serde_json::from_slice(&bytes)
                    .map_err(|_| "Invalid management request".to_string())?;
                if !matches!(job, Job::CleanupResiduals { .. }) {
                    return Err("空间迁移已取消，旧管理任务不能执行；请使用设备残留清理".into());
                }
                qiwo_sync::lifecycle::job::run(&request, &job)
                    .await
                    .map_err(|e| e.to_string())
            }
            .await;
            let reply = serde_json::json!({"ok":result.is_ok(), "message":result.as_ref().err()});
            local::atomic_write(
                &request.rime_user_dir,
                &format!(".qiwo-sync/managed-requests/{id}.result.json"),
                &serde_json::to_vec(&reply).unwrap(),
            )
            .map_err(|e| e.to_string())?;
            result?;
            return Ok(qiwo_sync::types::SyncSummary::new(
                request.mode,
                request.frontend,
                &request.device_id,
            ));
        }
        let enrolled = local::enrollment(&request.rime_user_dir)
            .map_err(|e| e.to_string())?
            .is_some();
        if args.prepare_managed {
            if enrolled {
                transfer::prepare(&request)
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                qiwo_sync::cleanup::prune_foreign_cache(&request)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            return Ok(qiwo_sync::types::SyncSummary::new(
                request.mode,
                request.frontend,
                &request.device_id,
            ));
        }
        if enrolled && args.native_managed {
            transfer::execute(&request).await.map_err(|e| e.to_string())
        } else {
            SyncEngine::new()
                .execute(request)
                .await
                .map_err(|e| e.to_string())
        }
    }
    .await;
    match managed {
        Ok(summary) => {
            println!(
                "{}",
                serde_json::to_string(&summary).expect("serializable summary")
            );
            0
        }
        // The caller gets a stable status without exposing server-provided
        // response bodies or secrets. Existing CLI commands retain diagnostics.
        Err(_) => {
            eprintln!("WebDAV sync failed; check connectivity and credentials");
            4
        }
    }
}
