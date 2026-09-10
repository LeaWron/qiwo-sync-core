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
    /// Check the shared data directory and move stale distribution copies out of the user directory
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
    /// Name of the environment variable holding the WebDAV password.
    ///
    /// There is deliberately no `--password`: a command line is readable by any
    /// process running as the same user, and lands in shell history.
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
    /// Rime's shared data directory, where the installer staged rime-frost.
    /// Read only: checked for completeness, and its file list is what tells a
    /// stale copy in the user directory apart from the user's own files.
    /// Nothing is copied out of it.
    #[arg(long)]
    frost_dir: PathBuf,
    #[arg(long)]
    device_id: Option<String>,
    #[arg(long)]
    dry_run: bool,
}

fn resolve_password(password_env: Option<&str>) -> Option<String> {
    std::env::var(password_env?).ok()
}

fn parse_frontend(value: &str) -> Result<Frontend, String> {
    match value.to_lowercase().as_str() {
        "weasel" => Ok(Frontend::Weasel),
        "squirrel" => Ok(Frontend::Squirrel),
        "ibus-rime" | "ibus" => Ok(Frontend::IbusRime),
        "trime" => Ok(Frontend::Trime),
        "qiwo-android" | "qiwo-yuyan" | "qiwoime" | "qiwo" | "qiwo-ime" => Ok(Frontend::QiwoIme),
        "yuyanime" | "yuyan" | "yuyan-ime" => Ok(Frontend::QiwoIme),
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
            let device_id = a.device_id.clone().unwrap_or_else(hostname);

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
    let device_id = args.device_id.clone().unwrap_or_else(hostname);
    let password = resolve_password(args.password_env.as_deref());

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontend_accepts_qiwo_android_identity() {
        assert_eq!(parse_frontend("qiwo-yuyan").unwrap(), Frontend::QiwoIme);
        assert_eq!(parse_frontend("qiwoime").unwrap(), Frontend::QiwoIme);
        assert_eq!(parse_frontend("qiwo").unwrap(), Frontend::QiwoIme);
        assert_eq!(parse_frontend("qiwo-ime").unwrap(), Frontend::QiwoIme);
    }

    #[test]
    fn parse_frontend_keeps_legacy_yuyan_aliases_as_inputs_only() {
        assert_eq!(parse_frontend("yuyanime").unwrap(), Frontend::QiwoIme);
        assert_eq!(parse_frontend("yuyan").unwrap(), Frontend::QiwoIme);
    }
}
