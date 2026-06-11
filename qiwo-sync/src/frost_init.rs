use std::path::Path;

use anyhow::Result;
use tokio::fs;

use crate::file_selector::FileSelector;
use crate::types::{SyncMode, SyncRequest, SyncSummary};

const DEFAULT_CUSTOM_YAML: &str = "default.custom.yaml";
const FROST_SCHEMA_FILE: &str = "rime_frost.schema.yaml";
const DEFAULT_CUSTOM_CONTENT: &str = concat!(
    "patch:\n",
    "  schema_list:\n",
    "    - schema: rime_frost\n",
    "  switcher/hotkeys/@next: F4\n",
    "  switcher/save_options/@next: auto_commit_spacing\n",
);
const SCHEMA_CUSTOM_CONTENT: &str = concat!(
    "patch:\n",
    "  switches/@next:\n",
    "    name: auto_commit_spacing\n",
    "    states: [ 关闭中英数字自动空格, 开启中英数字自动空格 ]\n",
);

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
        let schema_custom_files =
            ensure_schema_custom_yamls(frost_dir, &request.rime_user_dir, request.dry_run).await?;
        if schema_custom_files > 0 {
            messages.push(format!(
                "Qiwo auto spacing switcher patches ensured for {schema_custom_files} schema(s)."
            ));
        }

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

    let lower = relative_path
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_lowercase();

    lower.ends_with(".yaml")
        || lower == "installation.yaml"
        || lower.starts_with("cn_dicts/")
        || lower.starts_with("en_dicts/")
        || lower.starts_with("others/")
}

async fn ensure_default_custom_yaml(rime_user_dir: &Path, dry_run: bool) -> Result<()> {
    ensure_custom_yaml_file(
        rime_user_dir,
        DEFAULT_CUSTOM_YAML,
        DEFAULT_CUSTOM_CONTENT,
        dry_run,
    )
    .await
    .map(|_| ())
}

async fn ensure_schema_custom_yamls(
    frost_dir: &Path,
    rime_user_dir: &Path,
    dry_run: bool,
) -> Result<u32> {
    let mut ensured = 0u32;

    for entry in walkdir::WalkDir::new(frost_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let file_name = entry.file_name().to_string_lossy();
        let Some(schema_id) = file_name.strip_suffix(".schema.yaml") else {
            continue;
        };
        if !schema_id.starts_with("rime_frost") {
            continue;
        }

        let custom_file = format!("{schema_id}.custom.yaml");
        if ensure_custom_yaml_file(rime_user_dir, &custom_file, SCHEMA_CUSTOM_CONTENT, dry_run)
            .await?
        {
            ensured += 1;
        }
    }

    Ok(ensured)
}

async fn ensure_custom_yaml_file(
    rime_user_dir: &Path,
    file_name: &str,
    content: &str,
    dry_run: bool,
) -> Result<bool> {
    let file = rime_user_dir.join(file_name);

    if file.exists() {
        if let Ok(meta) = std::fs::metadata(&file) {
            if meta.len() > 0 {
                return Ok(false);
            }
        }
    }

    if dry_run {
        return Ok(true);
    }

    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).await?;
    }

    fs::write(&file, content).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use tokio::runtime::Runtime;

    use super::*;
    use crate::types::{Frontend, SyncMode, SyncRequest};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("qiwo-sync-core-{name}-{nanos}"))
    }

    #[test]
    fn init_frost_creates_qiwo_custom_patches() {
        let rt = Runtime::new().unwrap();
        let frost_dir = temp_dir("frost");
        let user_dir = temp_dir("user");
        std_fs::create_dir_all(&frost_dir).unwrap();
        std_fs::write(frost_dir.join("rime_frost.schema.yaml"), "schema\n").unwrap();
        std_fs::write(
            frost_dir.join("rime_frost_double_pinyin.schema.yaml"),
            "schema\n",
        )
        .unwrap();
        std_fs::write(frost_dir.join("luna_pinyin.schema.yaml"), "schema\n").unwrap();

        let request = SyncRequest {
            frontend: Frontend::IbusRime,
            rime_user_dir: user_dir.clone(),
            remote_url: None,
            username: None,
            password: None,
            device_id: "test".into(),
            mode: SyncMode::InitFrost,
            frost_dir: Some(frost_dir.clone()),
            dry_run: false,
        };

        rt.block_on(FrostInitializer::initialize(&request)).unwrap();

        let default_custom = std_fs::read_to_string(user_dir.join("default.custom.yaml")).unwrap();
        assert!(default_custom.contains("schema: rime_frost"));
        assert!(default_custom.contains("switcher/hotkeys/@next: F4"));
        assert!(default_custom.contains("switcher/save_options/@next: auto_commit_spacing"));

        let schema_custom =
            std_fs::read_to_string(user_dir.join("rime_frost.custom.yaml")).unwrap();
        assert!(schema_custom.contains("switches/@next"));
        assert!(schema_custom.contains("auto_commit_spacing"));
        assert!(schema_custom.contains("关闭中英数字自动空格"));
        assert!(schema_custom.contains("开启中英数字自动空格"));
        assert!(
            user_dir
                .join("rime_frost_double_pinyin.custom.yaml")
                .exists()
        );
        assert!(!user_dir.join("luna_pinyin.custom.yaml").exists());

        let _ = std_fs::remove_dir_all(frost_dir);
        let _ = std_fs::remove_dir_all(user_dir);
    }
}
