use std::path::Path;

use anyhow::Result;
use tokio::fs;

/// 确保 Rime 的 installation.yaml 包含正确的同步配置。
/// installation_id 使用设备标识，sync_dir 指向 "sync/"。
pub struct InstallationHelper;

impl InstallationHelper {
    const SYNC_DIR: &'static str = "sync";

    /// 确保 installation.yaml 存在且包含 installation_id 和 sync_dir。
    /// 如果文件已存在，仅补充缺失的字段。
    pub async fn ensure(rime_user_dir: &Path, device_id: &str) -> Result<()> {
        let file = rime_user_dir.join("installation.yaml");

        if file.exists() {
            let content = fs::read_to_string(&file).await?;
            let mut updated = content.trim_end().to_string();
            let mut needs_update = false;

            if !updated.contains("sync_dir:") {
                updated.push('\n');
                updated.push_str(&format!("sync_dir: \"{}\"\n", Self::SYNC_DIR));
                needs_update = true;
            }

            if !updated.contains("installation_id:") {
                updated.push('\n');
                updated.push_str(&format!(
                    "installation_id: \"{}\"\n",
                    make_safe_id(device_id)
                ));
                needs_update = true;
            }

            if needs_update {
                fs::write(&file, updated).await?;
            }

            return Ok(());
        }

        // Create new file
        let yaml = format!(
            "distribution: \"Qiwo\"\ndistribution_version: \"1.0\"\ninstallation_id: \"{}\"\nsync_dir: \"{}\"\n",
            make_safe_id(device_id),
            Self::SYNC_DIR
        );

        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&file, yaml).await?;
        Ok(())
    }

    /// 确保 sync/{device_id}/ 目录存在,供 Rime sync_user_data() 导出使用。
    pub fn ensure_sync_export_dir(rime_user_dir: &Path, device_id: &str) -> Result<std::path::PathBuf> {
        let dir = rime_user_dir
            .join(Self::SYNC_DIR)
            .join(make_safe_id(device_id));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

fn make_safe_id(device_id: &str) -> String {
    device_id
        .replace(' ', "-")
        .replace(':', "-")
        .replace('\\', "-")
        .replace('/', "-")
        .to_lowercase()
}
