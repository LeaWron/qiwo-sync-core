use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 同步模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMode {
    Sync,
    Push,
    Pull,
    InitFrost,
    SyncUserDict,
}

/// 前端标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Frontend {
    Weasel,
    Squirrel,
    IbusRime,
    Trime,
    #[serde(
        rename = "qiwo-yuyan",
        alias = "qiwoime",
        alias = "qiwo",
        alias = "qiwo-ime",
        alias = "yuyanime",
        alias = "yuyan",
        alias = "yuyan-ime"
    )]
    QiwoIme,
}

impl Frontend {
    pub fn as_str(&self) -> &'static str {
        match self {
            Frontend::Weasel => "Weasel",
            Frontend::Squirrel => "Squirrel",
            Frontend::IbusRime => "IbusRime",
            Frontend::Trime => "Trime",
            Frontend::QiwoIme => "qiwo-yuyan",
        }
    }
}

/// 同步请求
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRequest {
    pub frontend: Frontend,
    pub rime_user_dir: PathBuf,
    pub remote_url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub device_id: String,
    pub mode: SyncMode,
    pub frost_dir: Option<PathBuf>,
    pub dry_run: bool,
}

/// 同步结果摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSummary {
    pub mode: SyncMode,
    pub frontend: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    pub uploaded: u32,
    pub downloaded: u32,
    #[serde(rename = "conflictsBackedUp")]
    pub conflicts_backed_up: u32,
    pub skipped: u32,
    pub messages: Vec<String>,
}

impl SyncSummary {
    pub fn new(mode: SyncMode, frontend: Frontend, device_id: &str) -> Self {
        Self {
            mode,
            frontend: frontend.as_str().to_string(),
            device_id: device_id.to_string(),
            uploaded: 0,
            downloaded: 0,
            conflicts_backed_up: 0,
            skipped: 0,
            messages: Vec::new(),
        }
    }
}

/// 清单中的文件条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncFileEntry {
    #[serde(rename = "relativePath")]
    pub relative_path: String,
    pub size: u64,
    pub sha256: String,
    #[serde(rename = "lastWriteUtc")]
    pub last_write_utc: DateTime<Utc>,
    #[serde(rename = "eTag", default, skip_serializing_if = "Option::is_none")]
    pub e_tag: Option<String>,
}

/// 同步清单
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncManifest {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(default)]
    pub frontend: String,
    #[serde(rename = "updatedAtUtc")]
    pub updated_at_utc: DateTime<Utc>,
    pub files: HashMap<String, SyncFileEntry>,
}

fn default_version() -> u32 {
    1
}

impl SyncManifest {
    pub fn empty() -> Self {
        Self {
            version: 1,
            device_id: String::new(),
            frontend: String::new(),
            updated_at_utc: Utc::now(),
            files: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_deserializes_qiwo_android_identity() {
        assert_eq!(
            serde_json::from_str::<Frontend>("\"qiwo-yuyan\"").unwrap(),
            Frontend::QiwoIme
        );
        assert_eq!(
            serde_json::from_str::<Frontend>("\"qiwoime\"").unwrap(),
            Frontend::QiwoIme
        );
        assert_eq!(
            serde_json::from_str::<Frontend>("\"qiwo\"").unwrap(),
            Frontend::QiwoIme
        );
        assert_eq!(
            serde_json::from_str::<Frontend>("\"qiwo-ime\"").unwrap(),
            Frontend::QiwoIme
        );
    }

    #[test]
    fn frontend_deserializes_legacy_yuyan_aliases_without_serializing_them() {
        assert_eq!(
            serde_json::from_str::<Frontend>("\"yuyanime\"").unwrap(),
            Frontend::QiwoIme
        );
        assert_eq!(
            serde_json::from_str::<Frontend>("\"yuyan\"").unwrap(),
            Frontend::QiwoIme
        );
        assert_eq!(
            serde_json::to_string(&Frontend::QiwoIme).unwrap(),
            "\"qiwo-yuyan\""
        );
    }
}
