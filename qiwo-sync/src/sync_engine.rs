use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::file_selector::FileSelector;
use crate::installation::InstallationHelper;
use crate::types::{SyncFileEntry, SyncManifest, SyncMode, SyncRequest, SyncSummary};
use crate::webdav_client::WebDavClient;

const STATE_DIR: &str = ".qiwo-sync";
const BACKUP_DIR: &str = "backups";
const LOCAL_MANIFEST: &str = "manifest.json";
const REMOTE_MANIFEST: &str = ".qiwo-sync-manifest.json";

pub struct SyncEngine {
    selector: FileSelector,
}

impl SyncEngine {
    pub fn new() -> Self {
        Self {
            selector: FileSelector,
        }
    }

    pub async fn execute(&self, request: SyncRequest) -> Result<SyncSummary> {
        if request.mode == SyncMode::InitFrost {
            return crate::frost_init::FrostInitializer::initialize(&request).await;
        }

        let remote_url = request
            .remote_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("RemoteUrl is required for WebDAV sync."))?;

        if !request.dry_run {
            fs::create_dir_all(&request.rime_user_dir).await?;
            InstallationHelper::ensure(&request.rime_user_dir, &request.device_id).await?;
            InstallationHelper::ensure_sync_export_dir(&request.rime_user_dir, &request.device_id)?;
        }

        let webdav = WebDavClient::new(
            remote_url,
            request.username.as_deref(),
            request.password.as_deref(),
        )?;

        if !request.dry_run {
            webdav.ensure_root().await?;
        }

        match request.mode {
            SyncMode::Push => self.push(&request, &webdav).await,
            SyncMode::Pull => self.pull(&request, &webdav).await,
            SyncMode::Sync => self.sync(&request, &webdav).await,
            SyncMode::SyncUserDict => self.sync_user_dict(&request, &webdav).await,
            SyncMode::InitFrost => unreachable!(),
        }
    }

    // ---- Push ----

    async fn push(&self, request: &SyncRequest, webdav: &WebDavClient) -> Result<SyncSummary> {
        let local_files = scan_local_files(&request.rime_user_dir, &self.selector)?;
        let mut uploaded = 0u32;
        let mut messages = Vec::new();

        let mut sorted: Vec<_> = local_files.iter().collect();
        sorted.sort_by_key(|(k, _)| k.to_lowercase());

        for (path, _entry) in &sorted {
            if !request.dry_run {
                let local_path = request.rime_user_dir.join(path);
                webdav.put_file(path, &local_path).await?;
            }
            uploaded += 1;
        }

        let manifest = create_manifest(request, &local_files);
        write_manifests(request, webdav, &manifest).await?;

        messages.push(format!("Pushed {} file(s).", uploaded));
        let mut summary = SyncSummary::new(SyncMode::Push, request.frontend, &request.device_id);
        summary.uploaded = uploaded;
        summary.messages = messages;
        Ok(summary)
    }

    // ---- Pull ----

    async fn pull(&self, request: &SyncRequest, webdav: &WebDavClient) -> Result<SyncSummary> {
        let remote_manifest = read_remote_manifest(webdav).await?;
        let mut downloaded = 0u32;
        let mut skipped = 0u32;
        let mut messages = Vec::new();

        let mut sorted: Vec<_> = remote_manifest.files.iter().collect();
        sorted.sort_by_key(|(k, _)| k.to_lowercase());

        for (path, _entry) in &sorted {
            if !self.selector.should_sync(path) {
                skipped += 1;
                continue;
            }

            if !request.dry_run {
                let target = request.rime_user_dir.join(path);
                webdav.download_file(path, &target).await?;
            }
            downloaded += 1;
        }

        let local_files = if request.dry_run {
            remote_manifest.files.clone()
        } else {
            scan_local_files(&request.rime_user_dir, &self.selector)?
        };
        let local_manifest = create_manifest(request, &local_files);
        if !request.dry_run {
            write_local_manifest(request, &local_manifest).await?;
        }

        messages.push(format!("Pulled {} file(s).", downloaded));
        let mut summary = SyncSummary::new(SyncMode::Pull, request.frontend, &request.device_id);
        summary.downloaded = downloaded;
        summary.skipped = skipped;
        summary.messages = messages;
        Ok(summary)
    }

    // ---- Sync (双向三路合并) ----

    async fn sync(&self, request: &SyncRequest, webdav: &WebDavClient) -> Result<SyncSummary> {
        let previous = read_local_manifest(request).await?;
        let remote = read_remote_manifest(webdav).await?;
        let local_files = scan_local_files(&request.rime_user_dir, &self.selector)?;

        self.do_three_way_merge(request, webdav, &local_files, &remote, &previous)
            .await
    }

    // ---- SyncUserDict ----

    async fn sync_user_dict(
        &self,
        request: &SyncRequest,
        webdav: &WebDavClient,
    ) -> Result<SyncSummary> {
        let previous = read_local_manifest(request).await?;
        let remote = read_remote_manifest(webdav).await?;
        let local_files = scan_local_files(&request.rime_user_dir, &self.selector)?;

        // Filter: only sync/ directory files
        let local_dict: HashMap<String, SyncFileEntry> = local_files
            .into_iter()
            .filter(|(k, _)| k.starts_with("sync/"))
            .collect();

        let remote_dict: HashMap<String, SyncFileEntry> = remote
            .files
            .iter()
            .filter(|(k, _)| k.starts_with("sync/"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let filtered_remote = SyncManifest {
            device_id: remote.device_id.clone(),
            files: remote_dict,
            ..SyncManifest::empty()
        };

        self.do_three_way_merge(request, webdav, &local_dict, &filtered_remote, &previous)
            .await
    }

    async fn do_three_way_merge(
        &self,
        request: &SyncRequest,
        webdav: &WebDavClient,
        local_files: &HashMap<String, SyncFileEntry>,
        remote_manifest: &SyncManifest,
        previous_manifest: &SyncManifest,
    ) -> Result<SyncSummary> {
        let mut uploaded = 0u32;
        let mut downloaded = 0u32;
        let mut skipped = 0u32;
        let mut conflicts = 0u32;
        let mut messages = Vec::new();

        let mut all_paths: Vec<&String> = local_files
            .keys()
            .chain(remote_manifest.files.keys())
            .collect();
        all_paths.sort_by_key(|k| k.to_lowercase());
        all_paths.dedup();

        for path in &all_paths {
            if !self.selector.should_sync(path) {
                skipped += 1;
                continue;
            }

            let local_entry = local_files.get(*path);
            let remote_entry = remote_manifest.files.get(*path);
            let previous_entry = previous_manifest.files.get(*path);

            // Same hash on both sides → skip
            if let (Some(l), Some(r)) = (local_entry, remote_entry) {
                if l.sha256.eq_ignore_ascii_case(&r.sha256) {
                    skipped += 1;
                    continue;
                }
            }

            let local_changed = local_entry.map_or(false, |l| {
                previous_entry.map_or(true, |p| !l.sha256.eq_ignore_ascii_case(&p.sha256))
            });
            let remote_changed = remote_entry.map_or(false, |r| {
                previous_entry.map_or(true, |p| !r.sha256.eq_ignore_ascii_case(&p.sha256))
            });

            match (local_entry, remote_entry) {
                // Local only → upload
                (Some(l), None) => {
                    if !request.dry_run {
                        let lp = request.rime_user_dir.join(&l.relative_path);
                        webdav.put_file(&l.relative_path, &lp).await?;
                    }
                    uploaded += 1;
                }
                // Remote only → download
                (None, Some(r)) => {
                    if !request.dry_run {
                        let target = request.rime_user_dir.join(&r.relative_path);
                        webdav.download_file(&r.relative_path, &target).await?;
                    }
                    downloaded += 1;
                }
                (Some(l), Some(r)) => {
                    match (local_changed, remote_changed) {
                        // Local changed only → upload
                        (true, false) => {
                            if !request.dry_run {
                                let lp = request.rime_user_dir.join(&l.relative_path);
                                webdav.put_file(&l.relative_path, &lp).await?;
                            }
                            uploaded += 1;
                        }
                        // Remote changed only → download
                        (false, true) => {
                            if !request.dry_run {
                                let target = request.rime_user_dir.join(&r.relative_path);
                                webdav.download_file(&r.relative_path, &target).await?;
                            }
                            downloaded += 1;
                        }
                        // Both changed → conflict: backup local, remote wins
                        (true, true) => {
                            if !request.dry_run {
                                backup_local_file(&request.rime_user_dir, &l.relative_path)?;
                                let target = request.rime_user_dir.join(&r.relative_path);
                                webdav.download_file(&r.relative_path, &target).await?;
                            }
                            downloaded += 1;
                            conflicts += 1;
                            messages.push(format!(
                                "Conflict backed up, remote kept: {}",
                                l.relative_path
                            ));
                        }
                        // Neither changed → timestamp tiebreaker
                        (false, false) => {
                            if l.last_write_utc >= r.last_write_utc {
                                if !request.dry_run {
                                    let lp = request.rime_user_dir.join(&l.relative_path);
                                    webdav.put_file(&l.relative_path, &lp).await?;
                                }
                                uploaded += 1;
                            } else {
                                if !request.dry_run {
                                    let target = request.rime_user_dir.join(&r.relative_path);
                                    webdav.download_file(&r.relative_path, &target).await?;
                                }
                                downloaded += 1;
                            }
                        }
                    }
                }
                (None, None) => {
                    skipped += 1;
                }
            }
        }

        // Update both manifests
        let final_files = if request.dry_run {
            local_files.clone()
        } else {
            scan_local_files(&request.rime_user_dir, &self.selector)?
        };
        let final_manifest = create_manifest(request, &final_files);
        write_manifests(request, webdav, &final_manifest).await?;

        let label = if request.mode == SyncMode::SyncUserDict {
            "Dict sync"
        } else {
            "Sync"
        };
        messages.push(format!(
            "{} — uploaded {}, downloaded {}, conflicts {}.",
            label, uploaded, downloaded, conflicts
        ));

        let mut summary = SyncSummary::new(request.mode, request.frontend, &request.device_id);
        summary.uploaded = uploaded;
        summary.downloaded = downloaded;
        summary.conflicts_backed_up = conflicts;
        summary.skipped = skipped;
        summary.messages = messages;
        Ok(summary)
    }
}

// ---- File scanning ----

fn scan_local_files(
    rime_user_dir: &Path,
    selector: &FileSelector,
) -> Result<HashMap<String, SyncFileEntry>> {
    let mut entries = HashMap::new();
    if !rime_user_dir.exists() {
        return Ok(entries);
    }
    scan_dir(rime_user_dir, rime_user_dir, selector, &mut entries)?;
    Ok(entries)
}

fn scan_dir(
    base: &Path,
    current: &Path,
    selector: &FileSelector,
    entries: &mut HashMap<String, SyncFileEntry>,
) -> Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        if path.is_dir() {
            scan_dir(base, &path, selector, entries)?;
        } else if selector.should_sync(&relative) {
            let meta = entry.metadata()?;
            let sha256 = sha256_file(&path)?;
            let last_write_utc = chrono::DateTime::from(
                meta.modified()
                    .ok()
                    .unwrap_or_else(|| std::time::SystemTime::now()),
            );

            entries.insert(
                relative.clone(),
                SyncFileEntry {
                    relative_path: relative,
                    size: meta.len(),
                    sha256,
                    last_write_utc,
                    e_tag: None,
                },
            );
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// ---- Manifest I/O ----

fn local_manifest_path(rime_user_dir: &Path) -> PathBuf {
    rime_user_dir.join(STATE_DIR).join(LOCAL_MANIFEST)
}

async fn read_local_manifest(request: &SyncRequest) -> Result<SyncManifest> {
    let path = local_manifest_path(&request.rime_user_dir);
    if !path.exists() {
        return Ok(SyncManifest::empty());
    }

    let data = fs::read(&path).await?;
    Ok(serde_json::from_slice(&data).unwrap_or_else(|_| SyncManifest::empty()))
}

async fn write_local_manifest(request: &SyncRequest, manifest: &SyncManifest) -> Result<()> {
    let path = local_manifest_path(&request.rime_user_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let json = serde_json::to_vec_pretty(manifest)?;
    fs::write(&path, json).await?;
    Ok(())
}

async fn read_remote_manifest(webdav: &WebDavClient) -> Result<SyncManifest> {
    let bytes = webdav.get_bytes(REMOTE_MANIFEST).await?;
    match bytes {
        Some(data) => Ok(serde_json::from_slice(&data).unwrap_or_else(|_| SyncManifest::empty())),
        None => Ok(SyncManifest::empty()),
    }
}

async fn write_manifests(
    request: &SyncRequest,
    webdav: &WebDavClient,
    manifest: &SyncManifest,
) -> Result<()> {
    if request.dry_run {
        return Ok(());
    }

    write_local_manifest(request, manifest).await?;
    let json = serde_json::to_vec_pretty(manifest)?;
    webdav.put_bytes(REMOTE_MANIFEST, json).await?;
    Ok(())
}

fn create_manifest(request: &SyncRequest, files: &HashMap<String, SyncFileEntry>) -> SyncManifest {
    SyncManifest {
        version: 1,
        device_id: request.device_id.clone(),
        frontend: request.frontend.as_str().to_string(),
        updated_at_utc: Utc::now(),
        files: files.clone(),
    }
}

// ---- Backup ----

fn backup_local_file(rime_user_dir: &Path, relative_path: &str) -> Result<()> {
    let src = rime_user_dir.join(relative_path);
    if !src.exists() {
        return Ok(());
    }

    let timestamp = Utc::now().format("%Y%m%d%H%M%S").to_string();
    let backup_path = rime_user_dir
        .join(STATE_DIR)
        .join(BACKUP_DIR)
        .join(&timestamp)
        .join(relative_path);

    if let Some(parent) = backup_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::copy(&src, &backup_path)?;
    Ok(())
}
