use std::path::Path;

use anyhow::Result;
use tokio::fs;

/// 确保 Rime 的 installation.yaml 包含正确的同步配置。
/// installation_id 使用设备标识，sync_dir 指向 "sync/"。
pub struct InstallationHelper;

impl InstallationHelper {
    const SYNC_DIR: &'static str = "sync";

    /// 确保 installation.yaml 存在且包含 installation_id 和 sync_dir。
    /// 如果文件已存在，会把 installation_id 对齐到 WebDAV 设置里的 device_id。
    pub async fn ensure(rime_user_dir: &Path, device_id: &str) -> Result<()> {
        let file = rime_user_dir.join("installation.yaml");
        let safe_id = make_safe_id(device_id);

        if file.exists() {
            let content = fs::read_to_string(&file).await?;
            let mut has_sync_dir = false;
            let mut has_installation_id = false;
            let mut old_installation_id = None;
            let mut lines = Vec::new();

            for line in content.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("installation_id:") {
                    has_installation_id = true;
                    old_installation_id = parse_yaml_string_value(trimmed);
                    lines.push(format!("installation_id: \"{}\"", safe_id));
                } else if trimmed.starts_with("sync_dir:") {
                    has_sync_dir = true;
                    lines.push(format!("sync_dir: \"{}\"", Self::SYNC_DIR));
                } else {
                    lines.push(line.to_string());
                }
            }

            if !has_installation_id {
                lines.push(format!("installation_id: \"{}\"", safe_id));
            }

            if !has_sync_dir {
                lines.push(format!("sync_dir: \"{}\"", Self::SYNC_DIR));
            }

            let updated = format!("{}\n", lines.join("\n"));
            if updated != content {
                fs::write(&file, updated).await?;
            }

            Self::migrate_sync_data(rime_user_dir, old_installation_id.as_deref(), &safe_id)?;
            Self::ensure_sync_export_dir(rime_user_dir, &safe_id)?;
            return Ok(());
        }

        // Create new file
        let yaml = format!(
            "distribution: \"Qiwo\"\ndistribution_version: \"1.0\"\ninstallation_id: \"{}\"\nsync_dir: \"{}\"\n",
            safe_id,
            Self::SYNC_DIR
        );

        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&file, yaml).await?;
        Self::ensure_sync_export_dir(rime_user_dir, &safe_id)?;
        Ok(())
    }

    /// 确保 sync/{device_id}/ 目录存在,供 Rime sync_user_data() 导出使用。
    pub fn ensure_sync_export_dir(
        rime_user_dir: &Path,
        device_id: &str,
    ) -> Result<std::path::PathBuf> {
        let dir = rime_user_dir
            .join(Self::SYNC_DIR)
            .join(make_safe_id(device_id));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn migrate_sync_data(rime_user_dir: &Path, old_id: Option<&str>, new_id: &str) -> Result<()> {
        let Some(old_id) = old_id else {
            return Ok(());
        };

        let old_safe_id = make_safe_id(old_id);
        if old_safe_id == new_id {
            return Ok(());
        }

        let sync_dir = rime_user_dir.join(Self::SYNC_DIR);
        let old_dir = sync_dir.join(old_safe_id);
        let new_dir = sync_dir.join(new_id);
        if !old_dir.exists() {
            return Ok(());
        }

        std::fs::create_dir_all(&new_dir)?;
        move_dir_contents(&old_dir, &new_dir)?;
        let _ = std::fs::remove_dir(&old_dir);
        Ok(())
    }
}

fn make_safe_id(device_id: &str) -> String {
    let safe = device_id
        .replace(' ', "-")
        .replace(':', "-")
        .replace('\\', "-")
        .replace('/', "-")
        .to_lowercase();
    if safe.trim().is_empty() {
        "unknown".to_string()
    } else {
        safe
    }
}

fn parse_yaml_string_value(line: &str) -> Option<String> {
    let (_, value) = line.split_once(':')?;
    let trimmed = value.trim();
    if let Some(quoted) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Some(quoted.to_string());
    }
    if let Some(quoted) = trimmed
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        return Some(quoted.to_string());
    }
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn move_dir_contents(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            std::fs::create_dir_all(&target)?;
            move_dir_contents(&source, &target)?;
            let _ = std::fs::remove_dir(&source);
        } else if !target.exists() {
            std::fs::rename(&source, &target)?;
        }
    }
    Ok(())
}
