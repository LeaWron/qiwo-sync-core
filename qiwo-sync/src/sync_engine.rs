use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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
const USER_DICT_PREFIX: &str = "sync/";

/// Which slice of the Rime directory a merge run is allowed to touch.
///
/// A run only ever rewrites manifest entries inside its own scope; everything
/// outside is carried over untouched from the manifest it started from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeScope {
    All,
    UserDict,
}

impl MergeScope {
    fn contains(&self, path: &str) -> bool {
        match self {
            MergeScope::All => true,
            MergeScope::UserDict => path.starts_with(USER_DICT_PREFIX),
        }
    }
}

/// Returns `base` with its in-scope entries replaced by those from `updates`.
fn merge_scoped(
    base: &HashMap<String, SyncFileEntry>,
    updates: &HashMap<String, SyncFileEntry>,
    scope: MergeScope,
) -> HashMap<String, SyncFileEntry> {
    let mut merged: HashMap<String, SyncFileEntry> = base
        .iter()
        .filter(|(path, _)| !scope.contains(path))
        .map(|(path, entry)| (path.clone(), entry.clone()))
        .collect();

    merged.extend(
        updates
            .iter()
            .filter(|(path, _)| scope.contains(path))
            .map(|(path, entry)| (path.clone(), entry.clone())),
    );

    merged
}

pub struct SyncEngine {
    selector: FileSelector,
}

impl Default for SyncEngine {
    fn default() -> Self {
        Self::new()
    }
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
        let local_files = scan_local_files(&request.rime_user_dir, &self.selector).await?;
        let mut uploaded = 0u32;
        let mut messages = Vec::new();

        for path in local_files.keys().collect::<BTreeSet<_>>() {
            if !request.dry_run {
                let local_path = request.rime_user_dir.join(path);
                webdav.put_file(path, &local_path).await?;
            }
            uploaded += 1;
        }

        // Push is "make the remote match this device", so both manifests become
        // the local view.
        let manifest = create_manifest(request, local_files);
        write_manifests(request, webdav, &manifest, &manifest).await?;

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

        for path in remote_manifest.files.keys().collect::<BTreeSet<_>>() {
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
            scan_local_files(&request.rime_user_dir, &self.selector).await?
        };
        let local_manifest = create_manifest(request, local_files);
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
        let local_files = scan_local_files(&request.rime_user_dir, &self.selector).await?;

        self.do_three_way_merge(
            request,
            webdav,
            &local_files,
            &remote,
            &previous,
            MergeScope::All,
        )
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
        let local_files = scan_local_files(&request.rime_user_dir, &self.selector).await?;

        // The remote manifest is passed through whole. `MergeScope::UserDict`
        // narrows what gets *compared*, and the manifest writers below narrow
        // what gets *rewritten* — pre-filtering here used to drop every
        // out-of-scope remote entry from the manifest we published.
        self.do_three_way_merge(
            request,
            webdav,
            &local_files,
            &remote,
            &previous,
            MergeScope::UserDict,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn do_three_way_merge(
        &self,
        request: &SyncRequest,
        webdav: &WebDavClient,
        local_files: &HashMap<String, SyncFileEntry>,
        remote_manifest: &SyncManifest,
        previous_manifest: &SyncManifest,
        scope: MergeScope,
    ) -> Result<SyncSummary> {
        let mut uploaded = 0u32;
        let mut downloaded = 0u32;
        let mut skipped = 0u32;
        let mut conflicts = 0u32;
        let mut messages = Vec::new();

        // BTreeSet both dedups and orders; the old sort-by-lowercase + dedup pair
        // allocated a String per comparison and failed to dedup paths that
        // differed only in case.
        let all_paths: BTreeSet<&String> = local_files
            .keys()
            .chain(remote_manifest.files.keys())
            .filter(|path| scope.contains(path))
            .collect();

        for path in &all_paths {
            if !self.selector.should_sync(path) {
                skipped += 1;
                continue;
            }

            let local_entry = local_files.get(*path);
            let remote_entry = remote_manifest.files.get(*path);
            let previous_entry = previous_manifest.files.get(*path);

            // Same hash on both sides → skip
            if let (Some(l), Some(r)) = (local_entry, remote_entry)
                && l.sha256.eq_ignore_ascii_case(&r.sha256)
            {
                skipped += 1;
                continue;
            }

            let local_changed = local_entry.is_some_and(|l| {
                previous_entry.is_none_or(|p| !l.sha256.eq_ignore_ascii_case(&p.sha256))
            });
            let remote_changed = remote_entry.is_some_and(|r| {
                previous_entry.is_none_or(|p| !r.sha256.eq_ignore_ascii_case(&p.sha256))
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
                                backup_local_file(&request.rime_user_dir, &l.relative_path).await?;
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

        // Both manifests keep every entry this run did not look at. Rewriting
        // them wholesale from a full local scan is what made `sync-user-dict`
        // erase remote-only files from the published manifest.
        //
        // The rescan is skipped when nothing moved — which is the common case for
        // a periodic sync — because then the opening scan still describes the
        // directory exactly. It is deliberately *not* replaced by patching in the
        // remote manifest's entries for downloaded paths: a stale remote manifest
        // would then silently become our local baseline, and the rescan is the
        // only thing that keeps the baseline authoritative.
        let transferred = uploaded + downloaded;
        let final_files = if request.dry_run || transferred == 0 {
            local_files.clone()
        } else {
            scan_local_files(&request.rime_user_dir, &self.selector).await?
        };
        let local_update = create_manifest(
            request,
            merge_scoped(&previous_manifest.files, &final_files, scope),
        );
        let remote_update = create_manifest(
            request,
            merge_scoped(&remote_manifest.files, &final_files, scope),
        );
        write_manifests(request, webdav, &local_update, &remote_update).await?;

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

/// Walks the Rime user directory on a blocking thread.
///
/// Directory traversal plus SHA-256 of every candidate is genuinely blocking
/// work; running it inline on the runtime stalled every other task on the same
/// worker, which on Android is the whole sync.
async fn scan_local_files(
    rime_user_dir: &Path,
    selector: &FileSelector,
) -> Result<HashMap<String, SyncFileEntry>> {
    let dir = rime_user_dir.to_path_buf();
    let selector = *selector;
    tokio::task::spawn_blocking(move || scan_local_files_blocking(&dir, &selector)).await?
}

fn scan_local_files_blocking(
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
            // Pruning matters: `build/` holds the compiled prism and table files,
            // `*.userdb/` the LevelDB store, and a directory left over from the
            // old init-frost behaviour can still hold 153 MB. None of it can ever
            // contain a syncable file, so descending was pure stat traffic.
            if selector.should_descend(&relative) {
                scan_dir(base, &path, selector, entries)?;
            }
        } else if selector.should_sync(&relative) {
            let meta = entry.metadata()?;
            let sha256 = sha256_file(&path)?;
            let last_write_utc = chrono::DateTime::from(
                meta.modified()
                    .ok()
                    .unwrap_or_else(std::time::SystemTime::now),
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
        // First run on this device: an empty baseline is the correct starting point.
        return Ok(SyncManifest::empty());
    }

    let data = fs::read(&path).await?;

    // A corrupt baseline must not be quietly treated as "nothing was ever
    // synced": that makes every differing file look changed on both sides, so
    // the merge takes the conflict branch and the remote copy wins across the
    // board. Fail loudly and let the user decide to re-baseline.
    serde_json::from_slice(&data).with_context(|| {
        format!(
            "local sync manifest is corrupt: {} — delete it to re-baseline this device",
            path.display()
        )
    })
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
    let Some(data) = webdav.get_bytes(REMOTE_MANIFEST).await? else {
        // No manifest published yet: this remote has never been synced to.
        return Ok(SyncManifest::empty());
    };

    // Same reasoning as the local baseline — treating a truncated or half-written
    // remote manifest as empty would republish it with only this device's view,
    // dropping every file another device had uploaded.
    serde_json::from_slice(&data).with_context(|| {
        format!("remote sync manifest is corrupt: {REMOTE_MANIFEST} — delete it on the server to re-publish")
    })
}

async fn write_manifests(
    request: &SyncRequest,
    webdav: &WebDavClient,
    local: &SyncManifest,
    remote: &SyncManifest,
) -> Result<()> {
    if request.dry_run {
        return Ok(());
    }

    write_local_manifest(request, local).await?;
    let json = serde_json::to_vec_pretty(remote)?;
    webdav.put_bytes(REMOTE_MANIFEST, json).await?;
    Ok(())
}

fn create_manifest(request: &SyncRequest, files: HashMap<String, SyncFileEntry>) -> SyncManifest {
    SyncManifest {
        version: 1,
        device_id: request.device_id.clone(),
        frontend: request.frontend.as_str().to_string(),
        updated_at_utc: Utc::now(),
        files,
    }
}

/// Copies a file into `.qiwo-sync/backups/<timestamp>/` before it is overwritten.
///
/// On a blocking thread for the same reason as the scan: a conflicting user
/// dictionary snapshot can be several megabytes.
async fn backup_local_file(rime_user_dir: &Path, relative_path: &str) -> Result<()> {
    let dir = rime_user_dir.to_path_buf();
    let relative = relative_path.to_owned();
    tokio::task::spawn_blocking(move || backup_local_file_blocking(&dir, &relative)).await?
}

fn backup_local_file_blocking(rime_user_dir: &Path, relative_path: &str) -> Result<()> {
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

// ---- Backup ----

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, sha: &str) -> (String, SyncFileEntry) {
        (
            path.to_string(),
            SyncFileEntry {
                relative_path: path.to_string(),
                size: 1,
                sha256: sha.to_string(),
                last_write_utc: Utc::now(),
                e_tag: None,
            },
        )
    }

    fn manifest_of(entries: &[(&str, &str)]) -> HashMap<String, SyncFileEntry> {
        entries.iter().map(|(p, s)| entry(p, s)).collect()
    }

    #[test]
    fn user_dict_scope_only_covers_the_sync_directory() {
        assert!(MergeScope::UserDict.contains("sync/android/rime_frost.userdb.txt"));
        assert!(!MergeScope::UserDict.contains("lua/corrector.lua"));
        assert!(!MergeScope::UserDict.contains("default.custom.yaml"));
        assert!(MergeScope::All.contains("default.custom.yaml"));
    }

    /// The `sync-user-dict` regression: a dictionary-only run used to republish a
    /// manifest built from a full local scan, so files another device had
    /// uploaded but this one had never pulled vanished from the remote manifest.
    #[test]
    fn user_dict_merge_preserves_remote_only_entries_outside_the_scope() {
        let remote = manifest_of(&[
            ("lua/from-other-device.lua", "aaa"),
            ("sync/android/dict.txt", "old"),
        ]);
        let local_scan = manifest_of(&[("sync/android/dict.txt", "new")]);

        let merged = merge_scoped(&remote, &local_scan, MergeScope::UserDict);

        assert_eq!(
            merged["lua/from-other-device.lua"].sha256, "aaa",
            "an out-of-scope remote entry must survive a user-dict run"
        );
        assert_eq!(merged["sync/android/dict.txt"].sha256, "new");
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn user_dict_merge_drops_scoped_entries_that_no_longer_exist_locally() {
        let base = manifest_of(&[
            ("default.custom.yaml", "keep"),
            ("sync/retired-device/dict.txt", "gone"),
        ]);
        let local_scan = manifest_of(&[("sync/android/dict.txt", "new")]);

        let merged = merge_scoped(&base, &local_scan, MergeScope::UserDict);

        assert!(merged.contains_key("default.custom.yaml"));
        assert!(!merged.contains_key("sync/retired-device/dict.txt"));
        assert!(merged.contains_key("sync/android/dict.txt"));
    }

    #[test]
    fn full_scope_merge_replaces_everything() {
        let base = manifest_of(&[("a.custom.yaml", "old"), ("b.custom.yaml", "old")]);
        let local_scan = manifest_of(&[("a.custom.yaml", "new")]);

        let merged = merge_scoped(&base, &local_scan, MergeScope::All);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged["a.custom.yaml"].sha256, "new");
    }
}
