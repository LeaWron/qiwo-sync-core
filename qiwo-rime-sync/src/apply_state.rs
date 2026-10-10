use qiwo_sync::changes::{self, ApplyKind};

#[derive(clap::Args)]
pub struct NativeArgs {
    #[arg(long)]
    pub rime_user_dir: PathBuf,
    #[arg(long)]
    pub id: Option<String>,
    #[arg(long)]
    pub phase: Option<String>,
    #[arg(long, requires = "id")]
    pub cancel: bool,
}
pub fn native(args: &NativeArgs) -> anyhow::Result<i32> {
    let _guard = if args.id.is_none() {
        Some(qiwo_sync::operation::Guard::acquire(&args.rime_user_dir)?)
    } else {
        None
    };
    let task = changes::native_task(
        &args.rime_user_dir,
        args.id.as_deref(),
        args.phase.as_deref(),
        args.cancel,
    )?;
    println!("{}", task.id);
    Ok(0)
}
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct StateArgs {
    #[arg(long)]
    pub rime_user_dir: PathBuf,
    /// Exit 10 when there are no changes of this kind; stdout is always JSON.
    #[arg(long)]
    pub pending: Option<String>,
    #[arg(long, conflicts_with = "pending")]
    pub notify_deploy: bool,
}
#[derive(clap::Args)]
pub struct ApplyArgs {
    #[arg(long)]
    pub rime_user_dir: PathBuf,
    #[arg(long, conflicts_with = "complete")]
    pub kind: Option<String>,
    #[arg(long)]
    pub complete: Option<String>,
    #[arg(long, requires = "complete")]
    pub outcome: Option<String>,
}
fn kind(value: &str) -> anyhow::Result<ApplyKind> {
    match value {
        "merge" => Ok(ApplyKind::Merge),
        "deploy" => Ok(ApplyKind::Deploy),
        _ => anyhow::bail!("Unknown apply kind"),
    }
}
pub fn state(args: &StateArgs) -> anyhow::Result<i32> {
    if args.notify_deploy {
        let count = changes::claim_deploy_notification(&args.rime_user_dir)?;
        println!("{}", serde_json::json!({"newFiles": count}));
        return Ok(if count > 0 { 0 } else { 10 });
    }
    let state = changes::state(&args.rime_user_dir)?;
    let empty = match args.pending.as_deref() {
        Some(value) => {
            if kind(value)? == ApplyKind::Merge {
                state.pending_merge.is_empty()
            } else {
                state.pending_deploy.is_empty()
            }
        }
        None => false,
    };
    println!("{}", serde_json::to_string(&state)?);
    Ok(if empty { 10 } else { 0 })
}
pub fn apply(args: &ApplyArgs) -> anyhow::Result<i32> {
    let _guard = qiwo_sync::operation::Guard::acquire(&args.rime_user_dir)?;
    if let Some(id) = &args.complete {
        changes::complete(
            &args.rime_user_dir,
            id,
            args.outcome
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing outcome"))?,
        )?;
    } else {
        let task = changes::begin(
            &args.rime_user_dir,
            kind(
                args.kind
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Missing kind"))?,
            )?,
        )?;
        println!("{}", task.id);
    }
    Ok(0)
}
