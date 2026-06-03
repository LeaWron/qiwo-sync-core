use std::path::Path;

use anyhow::Result;
use tokio::fs;

use crate::file_selector::FileSelector;
use crate::types::{SyncMode, SyncRequest, SyncSummary};

const DEFAULT_CUSTOM_YAML: &str = "default.custom.yaml";
const FROST_SCHEMA_FILE: &str = "rime_frost.schema.yaml";
const DEFAULT_CUSTOM_CONTENT: &str = "patch:\n  schema_list:\n    - schema: rime_frost\n";

pub struct FrostInitializer;

impl FrostInitializer {
    pub async fn initialize(request: &SyncRequest) -> Result<SyncSummary> {
        let frost_dir = request
            .frost_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("FrostDir is required for init-frost."))?;

        if !frost_dir.exists() {
            anyhow::bail!(
                "rime-frost directory does not exist: {}",
                frost_dir.display()
            );
        }

        if !request.dry_run {
            fs::create_dir_all(&request.rime_user_dir).await?;
        }

        let mut messages = Vec::new();
        let mut copied = 0u32;
        let mut skipped_files = 0u32;

        let schema = request.rime_user_dir.join(FROST_SCHEMA_FILE);
        if !schema.exists() {
            copied = copy_frost_resources(
                frost_dir,
                &request.rime_user_dir,
                request.dry_run,
                &mut skipped_files,
            )?;
        } else {
            messages.push("rime-frost schema already exists; resource copy skipped.".into());
        }

        ensure_default_custom_yaml(&request.rime_user_dir, request.dry_run).await?;

        let mut summary =
            SyncSummary::new(SyncMode::InitFrost, request.frontend, &request.device_id);
        summary.downloaded = copied;
        summary.skipped = skipped_files;
        summary.messages = messages;
        Ok(summary)
    }
}

fn copy_frost_resources(
    frost_dir: &Path,
    rime_user_dir: &Path,
    dry_run: bool,
    skipped: &mut u32,
) -> Result<u32> {
    let selector = FileSelector;
    let mut copied = 0u32;

    for entry in walkdir::WalkDir::new(frost_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let source = entry.path();
        let relative = source
            .strip_prefix(frost_dir)
            .unwrap_or(source)
            .to_string_lossy()
            .replace('\\', "/");

        // Skip .git/
        if relative.starts_with(".git/") {
            continue;
        }

        if !is_frost_resource(&relative, &selector) {
            continue;
        }

        let target = rime_user_dir.join(&relative);

        if target.exists() {
            *skipped += 1;
            continue;
        }

        if !dry_run {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(source, &target)?;
        }

        copied += 1;
    }

    Ok(copied)
}

fn is_frost_resource(relative_path: &str, selector: &FileSelector) -> bool {
    if selector.should_sync(relative_path) {
        return true;
    }

    let lower = relative_path.replace('\\', "/").trim_start_matches('/').to_lowercase();

    lower.ends_with(".yaml")
        || lower == "installation.yaml"
        || lower.starts_with("cn_dicts/")
        || lower.starts_with("en_dicts/")
        || lower.starts_with("others/")
}

async fn ensure_default_custom_yaml(rime_user_dir: &Path, dry_run: bool) -> Result<()> {
    let file = rime_user_dir.join(DEFAULT_CUSTOM_YAML);

    if file.exists() {
        if let Ok(meta) = std::fs::metadata(&file) {
            if meta.len() > 0 {
                return Ok(());
            }
        }
    }

    if dry_run {
        return Ok(());
    }

    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).await?;
    }

    fs::write(&file, DEFAULT_CUSTOM_CONTENT).await?;
    Ok(())
}
