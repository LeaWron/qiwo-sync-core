//! Local enrollment, atomic baselines and reversible quarantine. The native
//! frontend must serialize all calls with export/import and identity changes.
use super::{
    ManagedState, ObjectRef, TransportProfile, check_files, identifier, operation_id, owner,
    store::{MAX_OBJECT_BYTES, Store, reference},
};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MARKER: &str = ".qiwo-sync/managed-v2.json";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Enrollment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_profile: Option<TransportProfile>,
    pub endpoint: String,
    pub migration_id: String,
    pub migration_fingerprint: String,
    pub device_id: String,
    pub revision: u64,
    pub files: BTreeMap<String, ObjectRef>,
}

/// Reject symlinks in every existing component; caller holds the frontend gate.
/// This is not a defense against an adversarial same-user process replacing paths.
pub fn safe_path(root: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(
        root.is_absolute() && root.is_dir(),
        "用户目录必须是已有绝对目录"
    );
    ensure!(crate::inventory::valid_path(relative), "本机路径无效");
    let mut current = root.to_owned();
    ensure!(
        !fs::symlink_metadata(&current)?.file_type().is_symlink(),
        "用户目录不能是符号链接"
    );
    for part in relative.split('/') {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) => ensure!(!meta.file_type().is_symlink(), "不能通过符号链接管理文件"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(current)
}

pub fn read(root: &Path, relative: &str, limit: usize) -> Result<Vec<u8>> {
    let path = safe_path(root, relative)?;
    let file = fs::File::open(path)?;
    ensure!(file.metadata()?.is_file(), "管理路径不是普通文件");
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "本机文件超过大小限制");
    Ok(bytes)
}

pub fn atomic_write(root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let path = safe_path(root, relative)?;
    let parent = path.parent().ok_or_else(|| anyhow!("路径无效"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.qiwo-part", operation_id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, &path)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn enrollment(root: &Path) -> Result<Option<Enrollment>> {
    let path = safe_path(root, MARKER)?;
    if !path.try_exists()? {
        return Ok(None);
    }
    let result: Enrollment =
        serde_json::from_slice(&read(root, MARKER, super::store::MAX_STATE_BYTES)?)?;
    ensure!(
        identifier(&result.device_id) && identifier(&result.migration_id),
        "本机管理身份损坏"
    );
    ensure!(
        matches!(
            (result.protocol_version, result.transport_profile),
            (None, None) | (Some(3), Some(TransportProfile::OpaqueMove))
        ),
        "本机管理协议版本不受支持"
    );
    check_files(&result.files)?;
    Ok(Some(result))
}

impl Enrollment {
    pub fn verify(&self, store: &Store, state: &ManagedState, actor: &str) -> Result<()> {
        state.validate()?;
        ensure!(
            self.protocol_version.unwrap_or(2) == state.protocol_version
                && self.transport_profile == state.transport_profile,
            "云端管理协议已改变，禁止自动切换或回退同步"
        );
        ensure!(
            self.endpoint == store.endpoint_id(),
            "连接地址已变更；禁止回退到旧同步或自动加入其他空间"
        );
        ensure!(
            self.device_id == actor,
            "本机管理身份不一致；请通过加入流程更新身份"
        );
        ensure!(state.revision >= self.revision, "云端管理状态发生回退");
        ensure!(
            state
                .receipts
                .get(&self.migration_id)
                .is_some_and(|r| r.fingerprint == self.migration_fingerprint),
            "管理空间已被替换或迁移回执丢失"
        );
        Ok(())
    }
    pub fn save(&self, root: &Path) -> Result<()> {
        atomic_write(root, MARKER, &serde_json::to_vec(self)?)
    }
}

pub fn enroll(
    root: &Path,
    store: &Store,
    state: &ManagedState,
    actor: &str,
    migration_id: &str,
) -> Result<()> {
    ensure!(
        identifier(actor) && !state.retired_devices.contains_key(actor),
        "本机身份无效或已停用"
    );
    let receipt = state
        .receipts
        .get(migration_id)
        .ok_or_else(|| anyhow!("迁移回执不存在"))?;
    if let Some(existing) = enrollment(root)? {
        existing.verify(store, state, actor)?;
        ensure!(existing.migration_id == migration_id, "本机已加入其他空间");
        return Ok(());
    }
    // Persist before touching snapshots: crashes must never fall back to v1.
    state.validate()?;
    Enrollment {
        protocol_version: (state.protocol_version == 3).then_some(3),
        transport_profile: state.transport_profile,
        endpoint: store.endpoint_id(),
        migration_id: migration_id.into(),
        migration_fingerprint: receipt.fingerprint.clone(),
        device_id: actor.into(),
        revision: state.revision,
        files: BTreeMap::new(),
    }
    .save(root)?;
    quarantine(root, state, actor)?;
    Ok(())
}

pub fn backup(root: &Path, path: &str) -> Result<()> {
    let source = safe_path(root, path)?;
    if !source.try_exists()? {
        return Ok(());
    }
    let target = safe_path(
        root,
        &format!(".qiwo-sync/managed-recovery/{}/{path}", operation_id()),
    )?;
    fs::create_dir_all(target.parent().unwrap())?;
    fs::rename(source, &target)?;
    #[cfg(unix)]
    fs::File::open(target.parent().unwrap())?.sync_all()?;
    Ok(())
}

/// Call before *each* native sync_user_data, which both exports and imports.
/// Unmanaged foreign snapshots are quarantined too; v1 clients cannot reintroduce them.
pub fn quarantine(root: &Path, state: &ManagedState, actor: &str) -> Result<usize> {
    state.validate()?;
    let mut candidates: Vec<String> = state.deleted_paths.keys().cloned().collect();
    let sync = safe_path(root, "sync")?;
    if sync.exists() {
        for entry in walkdir::WalkDir::new(sync).follow_links(false) {
            let entry = entry?;
            ensure!(
                !entry.file_type().is_symlink(),
                "快照目录含符号链接，停止导入"
            );
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(root)?
                .to_str()
                .ok_or_else(|| anyhow!("快照路径不是 UTF-8"))?
                .replace('\\', "/");
            if state.blocks_path(&relative)
                || (owner(&relative) != Some(actor) && !state.files.contains_key(&relative))
            {
                candidates.push(relative);
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    let mut moved = 0;
    for path in candidates {
        if safe_path(root, &path)?.try_exists()? {
            backup(root, &path)?;
            moved += 1;
        }
    }
    ensure!(
        !state.retired_devices.contains_key(actor),
        "本机设备已停用，已隔离快照并停止同步"
    );
    Ok(moved)
}

pub fn scan(root: &Path) -> Result<BTreeMap<String, ObjectRef>> {
    let mut files = BTreeMap::new();
    let selector = super::selector::FileSelector;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !e.file_type().is_dir()
                || e.path()
                    .strip_prefix(root)
                    .ok()
                    .and_then(|p| p.to_str())
                    .is_some_and(|p| selector.should_descend(p))
        })
    {
        let entry = entry?;
        ensure!(!entry.file_type().is_symlink(), "同步目录含符号链接");
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .strip_prefix(root)?
            .to_str()
            .ok_or_else(|| anyhow!("文件路径不是 UTF-8"))?
            .replace('\\', "/");
        if selector.should_sync(&path) {
            ensure!(super::managed_path(&path), "文件路径不支持管理");
            files.insert(
                path.clone(),
                reference(&read(root, &path, MAX_OBJECT_BYTES)?),
            );
            ensure!(files.len() <= super::MAX_ENTRIES, "本机文件数量超过限制");
        }
    }
    Ok(files)
}
