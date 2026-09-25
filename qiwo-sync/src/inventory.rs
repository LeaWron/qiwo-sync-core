//! Read-only inventory. This path must never initialize Rime, save settings,
//! create WebDAV collections, or execute the sync engine.
pub(crate) mod dav;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{file_selector::FileSelector, installation::safe_device_id, types::SyncManifest};

const MANIFEST: &str = ".qiwo-sync-manifest.json";
const MAX_FILES: usize = 10_000;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFacts {
    pub size: Option<u64>,
    pub modified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryFile {
    pub path: String,
    pub category: String,
    pub device_id: Option<String>,
    pub local: Option<FileFacts>,
    pub remote: Option<FileFacts>,
    /// None means the manifest could not be read, not "untracked".
    pub tracked: Option<bool>,
    pub sync_eligible: Option<bool>,
    pub issues: Vec<String>,
}

impl InventoryFile {
    fn new(path: &str) -> Self {
        let device = path.strip_prefix("sync/").and_then(|p| p.split_once('/'));
        let category = if path == MANIFEST || path.starts_with(".qiwo-sync/") {
            "metadata"
        } else if path.to_lowercase().ends_with(".custom.yaml") {
            "config"
        } else if path.rsplit('/').next() == Some("custom_phrase.txt") {
            "phrase"
        } else if device.is_some() && path.ends_with(".userdb.txt") {
            "snapshot"
        } else {
            "other"
        };
        Self {
            path: path.into(),
            category: category.into(),
            device_id: device.map(|(id, _)| id.to_owned()),
            local: None,
            remote: None,
            tracked: None,
            sync_eligible: Some(FileSelector.should_sync(path)),
            issues: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryDevice {
    pub id: String,
    pub is_current: bool,
    pub file_count: usize,
    pub local_bytes: u64,
    pub remote_bytes: u64,
    pub unknown_remote_sizes: usize,
    pub latest_file_modified_at: Option<DateTime<Utc>>,
    // File timestamps and manifest publisher are not device heartbeats.
    pub last_sync_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub scanned_at: DateTime<Utc>,
    pub current_device_id: String,
    pub remote_status: String,
    pub manifest_status: String,
    pub local_complete: bool,
    pub remote_complete: bool,
    pub local_bytes: u64,
    pub remote_bytes: u64,
    pub unknown_remote_sizes: usize,
    pub warnings: Vec<String>,
    pub devices: Vec<InventoryDevice>,
    pub files: Vec<InventoryFile>,
}

/// Reads metadata only. Credentials are used solely for the configured origin;
/// neither errors nor the result contain the URL, credentials, or file contents.
pub async fn inspect(
    local_root: &Path,
    remote_url: &str,
    username: &str,
    password: &str,
    device_id: &str,
) -> Result<Inventory> {
    let reader = dav::Reader::new(remote_url, username, password)?;
    let mut inventory = Inventory {
        scanned_at: Utc::now(),
        current_device_id: safe_device_id(device_id),
        remote_status: "unavailable".into(),
        manifest_status: "unavailable".into(),
        local_complete: true,
        remote_complete: false,
        local_bytes: 0,
        remote_bytes: 0,
        unknown_remote_sizes: 0,
        warnings: Vec::new(),
        devices: Vec::new(),
        files: Vec::new(),
    };
    let mut files = BTreeMap::<String, InventoryFile>::new();
    let mut manifest_paths = BTreeSet::new();
    let mut device_ids = BTreeSet::new();
    match reader.get_manifest().await {
        Ok(None) => inventory.manifest_status = "missing".into(),
        Ok(Some(bytes)) => match serde_json::from_slice::<SyncManifest>(&bytes) {
            Ok(manifest)
                if manifest.version == 1
                    && manifest
                        .files
                        .iter()
                        .all(|(p, e)| valid_path(p) && e.relative_path == *p)
                    && manifest.files.len() <= MAX_FILES =>
            {
                inventory.manifest_status = "ok".into();
                for path in manifest.files.keys() {
                    if path.starts_with(".qiwo-sync/")
                        || path.starts_with(".git/")
                        || path.starts_with(".qiwo-managed-v2/")
                    {
                        continue;
                    }
                    manifest_paths.insert(path.clone());
                    files.insert(path.clone(), InventoryFile::new(path));
                }
            }
            _ => {
                inventory.manifest_status = "invalid".into();
                inventory
                    .warnings
                    .push("同步清单损坏、版本不支持或超出限制；无法判断文件是否被记录。".into());
            }
        },
        Err(error) => inventory
            .warnings
            .push(format!("无法读取同步清单：{error}")),
    }

    let scan = async {
        let mut queue = std::collections::VecDeque::from([String::new()]);
        let mut directories = 0;
        let mut complete = true;
        while let Some(dir) = queue.pop_front() {
            directories += 1;
            if directories > 256 {
                inventory
                    .warnings
                    .push("目录数量超过扫描上限，结果不完整。".into());
                return false;
            }
            match reader.list(&dir).await {
                Ok(None) if dir.is_empty() => {
                    inventory.remote_status = "missing".into();
                    return true;
                }
                Ok(Some(listing)) => {
                    if dir.is_empty() {
                        inventory.remote_status = "ok".into();
                    }
                    complete &= listing.complete;
                    for entry in listing.entries {
                        if entry.is_dir {
                            if let Some(id) = entry
                                .path
                                .strip_prefix("sync/")
                                .filter(|id| !id.contains('/'))
                            {
                                device_ids.insert(id.to_owned());
                            }
                            // These are private/internal trees, not personal sync content.
                            if matches!(
                                entry.path.as_str(),
                                ".qiwo-sync" | ".git" | ".qiwo-managed-v2"
                            ) {
                                continue;
                            }
                            if entry.path.split('/').count() >= 16 {
                                complete = false;
                            } else {
                                queue.push_back(entry.path);
                            }
                        } else {
                            if files.len() >= MAX_FILES && !files.contains_key(&entry.path) {
                                inventory
                                    .warnings
                                    .push("文件数量超过扫描上限，结果不完整。".into());
                                return false;
                            }
                            files
                                .entry(entry.path.clone())
                                .or_insert_with(|| InventoryFile::new(&entry.path))
                                .remote = Some(entry.facts);
                        }
                    }
                }
                Ok(None) => {
                    complete = false;
                }
                Err(error) => {
                    inventory
                        .warnings
                        .push(format!("部分远端目录无法读取：{error}"));
                    return false;
                }
            }
        }
        complete
    };
    match tokio::time::timeout(Duration::from_secs(60), scan).await {
        Ok(complete) => inventory.remote_complete = complete,
        Err(_) => inventory
            .warnings
            .push("远端扫描超时，已显示读取到的部分结果。".into()),
    }
    if !inventory.remote_complete {
        inventory
            .warnings
            .push("远端结果不完整；未列出的文件不能视为不存在。".into());
    }
    let root: std::path::PathBuf = local_root.components().collect();
    let (mut files, local_complete, local_devices) =
        tokio::task::spawn_blocking(move || scan_local(&root, files)).await?;
    device_ids.extend(local_devices);
    inventory.local_complete = local_complete;
    if !local_complete {
        inventory
            .warnings
            .push("部分本地文件无法读取或超过扫描上限；未读取的文件状态未知。".into());
    }
    let manifest_known = matches!(inventory.manifest_status.as_str(), "ok" | "missing");
    for file in files.values_mut() {
        file.tracked = manifest_known.then(|| manifest_paths.contains(&file.path));
        if file.category != "metadata" {
            if file.remote.is_some() && file.tracked == Some(false) {
                file.issues.push("untracked".into());
            }
            if file.remote.is_none() && file.tracked == Some(true) && inventory.remote_complete {
                file.issues.push("missing-remote".into());
            }
            if file.sync_eligible == Some(false)
                && (file.remote.is_some() || file.tracked == Some(true))
            {
                file.issues.push("excluded".into());
            }
        }
    }
    inventory.files = files.into_values().collect();
    inventory.aggregate(device_ids);
    inventory.scanned_at = Utc::now();
    Ok(inventory)
}

fn scan_local(
    root: &Path,
    mut files: BTreeMap<String, InventoryFile>,
) -> (BTreeMap<String, InventoryFile>, bool, BTreeSet<String>) {
    let mut complete = true;
    let mut devices = BTreeSet::new();
    // Include locally eligible root files and sync exports, without reading
    // dictionary contents, binary user databases, build data or credentials.
    let walk_root = match std::fs::symlink_metadata(root) {
        Ok(meta) if meta.is_dir() => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        _ => {
            complete = false;
            false
        }
    };
    if walk_root {
        for (count, item) in walkdir::WalkDir::new(root)
            .follow_links(false)
            .follow_root_links(false)
            .max_depth(18)
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0
                    || !e.file_type().is_dir()
                    || e.path().strip_prefix(root).is_ok_and(|p| {
                        FileSelector.should_sync(&format!("{}/", p.to_string_lossy()))
                    }) && !e.file_name().to_string_lossy().ends_with(".userdb")
            })
            .enumerate()
        {
            if count >= 20_000 {
                complete = false;
                break;
            }
            let Ok(entry) = item else {
                complete = false;
                continue;
            };
            if entry.file_type().is_symlink() {
                complete = false;
                continue;
            }
            if entry.file_type().is_dir() {
                if entry.depth() == 2
                    && entry
                        .path()
                        .strip_prefix(root)
                        .is_ok_and(|p| p.starts_with("sync"))
                    && let Some(id) = entry.file_name().to_str().filter(|id| valid_path(id))
                {
                    devices.insert(id.to_owned());
                }
                if entry.depth() == 18 {
                    complete = false;
                }
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(root) else {
                continue;
            };
            let Some(path) = relative
                .to_str()
                .map(|s| s.replace(std::path::MAIN_SEPARATOR, "/"))
            else {
                complete = false;
                continue;
            };
            if !valid_path(&path) {
                complete = false;
                continue;
            }
            if !FileSelector.should_sync(&path) && !path.starts_with("sync/") {
                continue;
            }
            if files.len() >= MAX_FILES && !files.contains_key(&path) {
                complete = false;
                break;
            }
            files
                .entry(path.clone())
                .or_insert_with(|| InventoryFile::new(&path));
        }
    }
    // Also inspect known remote/manifest paths, including legacy distributed files.
    for file in files.values_mut() {
        match local_facts(root, &file.path) {
            Ok(facts) => file.local = facts,
            Err(_) => {
                file.issues.push("local-unknown".into());
                complete = false;
            }
        }
    }
    (files, complete, devices)
}

fn local_facts(root: &Path, relative: &str) -> Result<Option<FileFacts>> {
    anyhow::ensure!(valid_path(relative), "Unsafe path");
    let mut path = root.to_owned();
    // Never follow a remote-provided path through a symlink, including the root.
    for segment in std::iter::once("").chain(relative.split('/')) {
        if !segment.is_empty() {
            path.push(segment);
        }
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => anyhow::ensure!(!meta.file_type().is_symlink(), "Symlink skipped"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    }
    let meta = std::fs::metadata(path)?;
    anyhow::ensure!(meta.is_file(), "Not a regular file");
    Ok(Some(FileFacts {
        size: Some(meta.len()),
        modified_at: meta.modified().ok().map(DateTime::from),
    }))
}

pub(super) fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|s| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && !s.chars().any(|c| c == '\\' || c == ':' || c.is_control())
        })
}

impl Inventory {
    fn aggregate(&mut self, device_ids: BTreeSet<String>) {
        let mut devices = BTreeMap::new();
        let current = &self.current_device_id;
        let make_device = |id: &str| InventoryDevice {
            id: id.into(),
            is_current: id == current,
            file_count: 0,
            local_bytes: 0,
            remote_bytes: 0,
            unknown_remote_sizes: 0,
            latest_file_modified_at: None,
            last_sync_at: None,
        };
        for id in device_ids {
            devices.insert(id.clone(), make_device(&id));
        }
        devices.insert(current.clone(), make_device(current));
        for file in &self.files {
            let local = file.local.as_ref().and_then(|f| f.size).unwrap_or(0);
            let remote = file.remote.as_ref().and_then(|f| f.size).unwrap_or(0);
            let unknown = usize::from(file.remote.as_ref().is_some_and(|f| f.size.is_none()));
            self.local_bytes = self.local_bytes.saturating_add(local);
            self.remote_bytes = self.remote_bytes.saturating_add(remote);
            self.unknown_remote_sizes += unknown;
            if let Some(id) = &file.device_id {
                let device = devices.entry(id.clone()).or_insert_with(|| make_device(id));
                device.file_count += 1;
                device.local_bytes = device.local_bytes.saturating_add(local);
                device.remote_bytes = device.remote_bytes.saturating_add(remote);
                device.unknown_remote_sizes += unknown;
                let modified = file
                    .local
                    .iter()
                    .chain(file.remote.iter())
                    .filter_map(|f| f.modified_at)
                    .max();
                device.latest_file_modified_at = device.latest_file_modified_at.max(modified);
            }
        }
        self.devices = devices.into_values().collect();
        self.devices.sort_by_key(|d| !d.is_current);
    }
}
