//! All native desktop adapters execute the same residual-cleanup job.
use qiwo_sync::{
    lifecycle::{job::Job, local},
    types::{SyncMode, SyncRequest},
};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    sync: super::SyncArgs,
    #[arg(long)]
    request_id: String,
}

pub async fn run(args: &Args) -> i32 {
    let id = &args.request_id;
    if id.len() != 64
        || id
            .bytes()
            .any(|b| !b.is_ascii_digit() && !(b'a'..=b'f').contains(&b))
        || args.sync.dry_run
    {
        eprintln!("Invalid cleanup request");
        return 3;
    }
    let result = async {
        let a = &args.sync;
        let request = SyncRequest {
            frontend: super::parse_frontend(&a.frontend).map_err(anyhow::Error::msg)?,
            rime_user_dir: a.rime_user_dir.clone(),
            remote_url: Some(a.remote_url.clone()),
            username: a.username.clone(),
            password: super::resolve_password(a.password_env.as_deref()),
            device_id: qiwo_sync::installation::installed_device_id(&a.rime_user_dir)?,
            mode: SyncMode::Sync,
            frost_dir: None,
            dry_run: false,
        };
        let bytes = local::read(
            &request.rime_user_dir,
            &format!(".qiwo-sync/managed-requests/{id}.json"),
            4 * 1024 * 1024,
        )?;
        let job: Job = serde_json::from_slice(&bytes)?;
        let Job::CleanupResiduals { plan } = &job else {
            anyhow::bail!("Only residual cleanup is supported");
        };
        anyhow::ensure!(&plan.id == id, "Cleanup request identity differs");
        qiwo_sync::lifecycle::job::run(&request, &job).await
    }
    .await;
    let reply = serde_json::json!({"ok": result.is_ok(), "message": result.as_ref().err().map(|_| "清理未完成，请重新扫描并核对备份")});
    if local::atomic_write(
        &args.sync.rime_user_dir,
        &format!(".qiwo-sync/managed-requests/{id}.result.json"),
        &serde_json::to_vec(&reply).unwrap(),
    )
    .is_err()
    {
        return 4;
    }
    if result.is_ok() { 0 } else { 1 }
}
