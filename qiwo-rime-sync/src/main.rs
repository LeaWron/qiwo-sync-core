use std::path::PathBuf;

use clap::{Parser, Subcommand};
use qiwo_sync::sync_engine::SyncEngine;
use qiwo_sync::types::{Frontend, SyncMode, SyncRequest};

/// Qiwo Rime Sync — WebDAV-based Rime configuration and user dictionary sync.
#[derive(Parser)]
#[command(name = "qiwo-rime-sync", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bidirectional sync with conflict detection
    Sync(SyncArgs),
    /// Push local files to remote
    Push(SyncArgs),
    /// Pull remote files to local
    Pull(SyncArgs),
    /// Initialize/update rime-frost schema
    InitFrost(InitFrostArgs),
    /// Sync only user dictionary (sync/ directory)
    SyncUserDict(SyncArgs),
}

#[derive(clap::Args)]
struct SyncArgs {
    #[arg(long)]
    frontend: String,
    #[arg(long)]
    rime_user_dir: PathBuf,
    #[arg(long)]
    remote_url: String,
    #[arg(long)]
    username: Option<String>,
    #[arg(long)]
    password: Option<String>,
    #[arg(long)]
    password_env: Option<String>,
    #[arg(long)]
    device_id: Option<String>,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct InitFrostArgs {
    #[arg(long)]
    frontend: String,
    #[arg(long)]
    rime_user_dir: PathBuf,
    #[arg(long)]
    frost_dir: PathBuf,
    #[arg(long)]
    device_id: Option<String>,
    #[arg(long)]
    dry_run: bool,
}

fn resolve_password(password: Option<String>, password_env: Option<String>) -> Option<String> {
    if password.is_some() {
        return password;
    }
    if let Some(env_var) = password_env {
        return std::env::var(&env_var).ok();
    }
    None
}

fn parse_frontend(value: &str) -> Result<Frontend, String> {
    match value.to_lowercase().as_str() {
        "weasel" => Ok(Frontend::Weasel),
        "squirrel" => Ok(Frontend::Squirrel),
        "ibus-rime" | "ibus" => Ok(Frontend::IbusRime),
        "trime" => Ok(Frontend::Trime),
        "yuyanime" | "yuyan" => Ok(Frontend::YuyanIme),
        _ => Err(format!("Unknown frontend: {}", value)),
    }
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let (mode, args, print_json) = match &cli.command {
        Command::Sync(a) => (SyncMode::Sync, a, a.json),
        Command::Push(a) => (SyncMode::Push, a, a.json),
        Command::Pull(a) => (SyncMode::Pull, a, a.json),
        Command::SyncUserDict(a) => (SyncMode::SyncUserDict, a, a.json),
        Command::InitFrost(a) => {
            let frontend = parse_frontend(&a.frontend)?;
            let device_id = a.device_id.clone().unwrap_or_else(|| hostname());

            let request = SyncRequest {
                frontend,
                rime_user_dir: a.rime_user_dir.clone(),
                remote_url: None,
                username: None,
                password: None,
                device_id,
                mode: SyncMode::InitFrost,
                frost_dir: Some(a.frost_dir.clone()),
                dry_run: a.dry_run,
            };

            let engine = SyncEngine::new();
            match engine.execute(request).await {
                Ok(s) => {
                    for msg in &s.messages {
                        println!("{}", msg);
                    }
                }
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(2);
                }
            }
            return Ok(());
        }
    };

    let frontend = parse_frontend(&args.frontend)?;
    let device_id = args.device_id.clone().unwrap_or_else(|| hostname());
    let password = resolve_password(args.password.clone(), args.password_env.clone());

    let request = SyncRequest {
        frontend,
        rime_user_dir: args.rime_user_dir.clone(),
        remote_url: Some(args.remote_url.clone()),
        username: args.username.clone(),
        password,
        device_id,
        mode,
        frost_dir: None,
        dry_run: args.dry_run,
    };

    let engine = SyncEngine::new();
    match engine.execute(request).await {
        Ok(summary) => {
            if print_json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                for msg in &summary.messages {
                    println!("{}", msg);
                }
                println!(
                    "mode={:?} uploaded={} downloaded={} conflicts={} skipped={}",
                    summary.mode,
                    summary.uploaded,
                    summary.downloaded,
                    summary.conflicts_backed_up,
                    summary.skipped
                );
            }
        }
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(2);
        }
    }

    Ok(())
}
