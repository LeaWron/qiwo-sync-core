//! One-time maintenance of existing device snapshot files. No remote namespace,
//! enrollment, device retirement, tombstones, or protocol migration is created.
//! All other writers must be paused: the DAV server may not implement atomic CAS.
mod dav;
mod directories;

use crate::{
    inventory,
    lifecycle::{ObjectRef, local, store::reference},
    types::{SyncFileEntry, SyncManifest, SyncRequest},
};
use anyhow::{Result, anyhow, ensure};
use chrono::{DateTime, Utc};
use dav::Dav;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
};

const REMOTE_MANIFEST: &str = ".qiwo-sync-manifest.json";
const LOCAL_MANIFEST: &str = ".qiwo-sync/manifest.json";
const MAX_MANIFEST: usize = 4 * 1024 * 1024;
const MAX_FILE: usize = 64 * 1024 * 1024;
const MAX_TOTAL: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Selection {
    Device { device_id: String },
    Files { paths: BTreeSet<String> },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct File {
    pub local: Option<ObjectRef>,
    pub remote: Option<ObjectRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    pub id: String,
    pub actor: String,
    pub endpoint: String,
    pub created_at: DateTime<Utc>,
    pub selection: Selection,
    pub files: BTreeMap<String, File>,
    #[serde(default)]
    pub directories: BTreeSet<String>,
    pub remote_manifest: Option<ObjectRef>,
    pub local_manifest: Option<ObjectRef>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub complete: bool,
    pub remote_deleted: Vec<String>,
    pub local_removed: Vec<String>,
    #[serde(default)]
    pub remote_directories_deleted: Vec<String>,
    #[serde(default)]
    pub local_directories_removed: Vec<String>,
    #[serde(default)]
    pub directories_kept: Vec<String>,
    pub backup_path: String,
    pub message: String,
}

pub fn foreign(path: &str, actor: &str) -> bool {
    path.strip_prefix("sync/")
        .and_then(|p| p.split_once('/'))
        .is_some_and(|(id, _)| id != actor)
}
fn allowed(path: &str, actor: &str) -> bool {
    inventory::valid_path(path)
        && foreign(path, actor)
        && !path.split('/').any(|part| part.starts_with('.'))
}
fn local_bytes(root: &Path, path: &str, limit: usize) -> Result<Option<Vec<u8>>> {
    if !local::safe_path(root, path)?.try_exists()? {
        return Ok(None);
    }
    Ok(Some(local::read(root, path, limit)?))
}
fn object(value: Option<&[u8]>) -> Option<ObjectRef> {
    value.map(reference)
}
fn parse_manifest(bytes: &[u8]) -> Result<SyncManifest> {
    let value: SyncManifest =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("同步清单损坏，停止清理"))?;
    ensure!(
        value.version == 1
            && value.files.len() <= 10_000
            && value
                .files
                .iter()
                .all(|(p, e)| inventory::valid_path(p) && e.relative_path == *p),
        "同步清单无效，停止清理"
    );
    Ok(value)
}
fn pruned(bytes: &[u8], plan: &Plan) -> Result<Vec<u8>> {
    // Preserve unknown fields and all unselected entries for older/newer clients.
    parse_manifest(bytes)?;
    let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
    let files = value["files"]
        .as_object_mut()
        .ok_or_else(|| anyhow!("同步清单无效"))?;
    for path in plan.files.keys() {
        files.remove(path);
    }
    Ok(serde_json::to_vec_pretty(&value)?)
}
fn check_request(request: &SyncRequest) -> Result<()> {
    ensure!(!request.dry_run, "清理请使用独立预览");
    ensure!(
        !request.device_id.is_empty()
            && inventory::valid_path(&request.device_id)
            && request
                .device_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)),
        "本机设备身份无效"
    );
    ensure!(
        local::enrollment(&request.rime_user_dir)?.is_none(),
        "本机曾加入实验管理空间，不能直接清理原目录"
    );
    Ok(())
}

pub async fn preview(request: &SyncRequest, selection: Selection) -> Result<Plan> {
    check_request(request)?;
    let dav = Dav::new(request)?;
    let inventory = inventory::inspect(
        &request.rime_user_dir,
        request.remote_url.as_deref().unwrap(),
        request.username.as_deref().unwrap_or(""),
        request.password.as_deref().unwrap_or(""),
        &request.device_id,
    )
    .await?;
    ensure!(
        inventory.local_complete
            && inventory.remote_complete
            && matches!(inventory.remote_status.as_str(), "ok" | "missing"),
        "盘点不完整，请重新扫描后再清理"
    );
    ensure!(
        matches!(inventory.manifest_status.as_str(), "ok" | "missing"),
        "同步清单无法确认，不能清理"
    );
    let paths: BTreeSet<_> = match &selection {
        Selection::Device { device_id } => {
            ensure!(
                !device_id.is_empty()
                    && device_id != &request.device_id
                    && inventory::valid_path(device_id)
                    && !device_id.contains('/'),
                "不能清理本机或无效设备 ID"
            );
            inventory
                .files
                .iter()
                .filter(|f| f.device_id.as_deref() == Some(device_id))
                .map(|f| f.path.clone())
                .collect()
        }
        Selection::Files { paths } => paths.clone(),
    };
    let directories = directories::preview(request, &selection, &paths).await?;
    ensure!(
        (!paths.is_empty() || !directories.is_empty()) && paths.len() <= 512,
        "请选择设备残留文件或空目录（最多 512 个文件）"
    );
    let remote_manifest = dav.get(REMOTE_MANIFEST, MAX_MANIFEST).await?;
    let local_manifest = local_bytes(&request.rime_user_dir, LOCAL_MANIFEST, MAX_MANIFEST)?;
    if let Some(value) = &remote_manifest {
        parse_manifest(&value.bytes)?;
    }
    if let Some(value) = &local_manifest {
        parse_manifest(value)?;
    }
    let mut files = BTreeMap::new();
    let mut total = 0;
    for path in paths {
        ensure!(
            allowed(&path, &request.device_id),
            "只能清理其他设备的残留文件，不能清理公共配置或本机快照"
        );
        ensure!(
            inventory.files.iter().any(|f| f.path == path),
            "所选文件已不在盘点中，请刷新"
        );
        let remote = dav.get(&path, MAX_FILE).await?;
        let local = local_bytes(&request.rime_user_dir, &path, MAX_FILE)?;
        let file = File {
            local: object(local.as_deref()),
            remote: object(remote.as_ref().map(|r| r.bytes.as_slice())),
        };
        total +=
            file.local.as_ref().map_or(0, |v| v.size) + file.remote.as_ref().map_or(0, |v| v.size);
        ensure!(total <= MAX_TOTAL, "单次备份超过 256 MiB，请分批清理");
        files.insert(path, file);
    }
    ensure!(
        object(
            dav.get(REMOTE_MANIFEST, MAX_MANIFEST)
                .await?
                .as_ref()
                .map(|r| r.bytes.as_slice())
        ) == object(remote_manifest.as_ref().map(|r| r.bytes.as_slice())),
        "同步清单在预览时变化，请暂停其他设备同步后重新预览"
    );
    Ok(Plan {
        id: crate::lifecycle::random_id()?,
        actor: request.device_id.clone(),
        endpoint: dav.endpoint(),
        created_at: Utc::now(),
        selection,
        files,
        directories,
        remote_manifest: object(remote_manifest.as_ref().map(|r| r.bytes.as_slice())),
        local_manifest: object(local_manifest.as_deref()),
    })
}

/// Called only inside the native frontend's serialized task gate. A task that
/// has begun mutations is never replayed: a new preview must inspect remaining files.
pub async fn execute(request: &SyncRequest, plan: &Plan) -> Result<Report> {
    check_request(request)?;
    let _guard = crate::operation::Guard::acquire(&request.rime_user_dir)?;
    let dav = Dav::new(request)?;
    ensure!(
        plan.id.len() == 64 && plan.id.bytes().all(|b| b.is_ascii_hexdigit()),
        "清理任务 ID 无效"
    );
    ensure!(
        plan.actor == request.device_id && plan.endpoint == dav.endpoint(),
        "设备或连接配置已变化，请重新预览"
    );
    ensure!(
        plan.created_at <= Utc::now()
            && Utc::now()
                .signed_duration_since(plan.created_at)
                .num_minutes()
                < 30,
        "预览已过期，请重新扫描"
    );
    ensure!(
        (!plan.files.is_empty() || !plan.directories.is_empty())
            && plan.files.len() <= 512
            && plan.files.keys().all(|p| allowed(p, &request.device_id))
            && directories::valid(plan, &request.device_id),
        "清理范围无效"
    );
    let root = &request.rime_user_dir;
    let folder = format!(".qiwo-sync/cleanup-backups/{}", plan.id);
    ensure!(
        !local::safe_path(root, &format!("{folder}/started.json"))?.try_exists()?,
        "此任务已开始执行，不能重复删除；请重新扫描生成预览，原备份已保留"
    );
    let mut report = Report {
        complete: false,
        remote_deleted: Vec::new(),
        local_removed: Vec::new(),
        remote_directories_deleted: Vec::new(),
        local_directories_removed: Vec::new(),
        directories_kept: Vec::new(),
        backup_path: local::safe_path(root, &folder)?.display().to_string(),
        message: String::new(),
    };
    let result = execute_inner(request, plan, &dav, &folder, &mut report).await;
    report.complete = result.is_ok();
    report.message = match &result {
        Ok(()) if report.directories_kept.is_empty() => {
            "清理完成，已移除范围内的空目录。设备 ID 未被停用。".into()
        }
        Ok(()) => "文件清理完成；仍有内容的目录已保留。设备 ID 未被停用。".into(),
        Err(e) => format!("清理未完成：{e}。请重新扫描，勿重复执行旧任务。"),
    };
    local::atomic_write(
        root,
        &format!("{folder}/result.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    result.map_err(|e| {
        anyhow!(
            "{e}；可能已部分清理，请重新扫描。备份：{}",
            report.backup_path
        )
    })?;
    Ok(report)
}

async fn execute_inner(
    request: &SyncRequest,
    plan: &Plan,
    dav: &Dav,
    folder: &str,
    report: &mut Report,
) -> Result<()> {
    let root = &request.rime_user_dir;
    let remote_manifest = dav.get(REMOTE_MANIFEST, MAX_MANIFEST).await?;
    let local_manifest = local_bytes(root, LOCAL_MANIFEST, MAX_MANIFEST)?;
    ensure!(
        object(remote_manifest.as_ref().map(|r| r.bytes.as_slice())) == plan.remote_manifest
            && object(local_manifest.as_deref()) == plan.local_manifest,
        "同步清单已变化，请重新预览"
    );
    // Back up every side before the first deletion or manifest write.
    local::atomic_write(
        root,
        &format!("{folder}/plan.json"),
        &serde_json::to_vec_pretty(plan)?,
    )?;
    if let Some(value) = &remote_manifest {
        parse_manifest(&value.bytes)?;
        local::atomic_write(
            root,
            &format!("{folder}/remote-manifest.json"),
            &value.bytes,
        )?;
    }
    if let Some(value) = &local_manifest {
        parse_manifest(value)?;
        local::atomic_write(root, &format!("{folder}/local-manifest.json"), value)?;
    }
    let mut total = 0;
    for (path, file) in &plan.files {
        let remote = dav.get(path, MAX_FILE).await?;
        let local = local_bytes(root, path, MAX_FILE)?;
        ensure!(
            object(remote.as_ref().map(|r| r.bytes.as_slice())) == file.remote
                && object(local.as_deref()) == file.local,
            "文件已变化：{path}，请重新预览"
        );
        total += remote.as_ref().map_or(0, |v| v.bytes.len() as u64)
            + local.as_ref().map_or(0, |v| v.len() as u64);
        ensure!(total <= MAX_TOTAL, "备份超过单次限制");
        if let Some(value) = remote {
            local::atomic_write(root, &format!("{folder}/remote/{path}"), &value.bytes)?;
        }
        if let Some(value) = local {
            local::atomic_write(root, &format!("{folder}/local/{path}"), &value)?;
        }
    }
    let fresh = dav.get(REMOTE_MANIFEST, MAX_MANIFEST).await?;
    ensure!(
        object(fresh.as_ref().map(|r| r.bytes.as_slice())) == plan.remote_manifest,
        "同步清单在备份期间变化，停止清理"
    );
    ensure!(
        object(local_bytes(root, LOCAL_MANIFEST, MAX_MANIFEST)?.as_deref()) == plan.local_manifest,
        "本机同步清单已变化"
    );
    local::atomic_write(root, &format!("{folder}/started.json"), b"{}")?;
    // Prune only previewed keys; a partial failure leaves visible untracked
    // residuals that can be selected again, instead of broken manifest links.
    if let Some(old) = fresh {
        dav.manifest(pruned(&old.bytes, plan)?, &old).await?;
    }
    if let Some(old) = &local_manifest {
        local::atomic_write(root, LOCAL_MANIFEST, &pruned(old, plan)?)?;
    }
    for (path, file) in &plan.files {
        let remote = dav.get(path, MAX_FILE).await?;
        ensure!(
            object(remote.as_ref().map(|r| r.bytes.as_slice())) == file.remote,
            "文件在清理期间变化：{path}"
        );
        if let Some(old) = remote {
            dav.delete_file(path, &request.device_id, &old).await?;
            report.remote_deleted.push(path.clone());
        }
        let local = local_bytes(root, path, MAX_FILE)?;
        ensure!(
            object(local.as_deref()) == file.local,
            "本机文件在清理期间变化：{path}"
        );
        if local.is_some() {
            std::fs::remove_file(local::safe_path(root, path)?)?;
            report.local_removed.push(path.clone());
        }
        local::atomic_write(
            root,
            &format!("{folder}/result.json"),
            &serde_json::to_vec_pretty(report)?,
        )?;
    }
    directories::execute(request, plan, dav, report).await?;
    Ok(())
}

/// Remove a confirmed missing foreign snapshot from the *cache*, after backup.
/// Own snapshots and user databases are never touched, so an old device may
/// publish again under its original ID when it returns. No retirement records.
pub async fn prune_foreign_cache(request: &SyncRequest) -> Result<()> {
    let dav = Dav::new(request)?;
    let Some(remote) = dav.get(REMOTE_MANIFEST, MAX_MANIFEST).await? else {
        return Ok(());
    };
    let manifest = parse_manifest(&remote.bytes)?;
    let root = &request.rime_user_dir;
    let sync = local::safe_path(root, "sync")?;
    if !sync.exists() {
        return Ok(());
    }
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(sync).follow_links(false) {
        let entry = entry?;
        ensure!(
            !entry.file_type().is_symlink(),
            "快照缓存含符号链接，停止同步"
        );
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .strip_prefix(root)?
            .to_str()
            .ok_or_else(|| anyhow!("快照路径无效"))?
            .replace('\\', "/");
        if allowed(&path, &request.device_id) && !manifest.files.contains_key(&path) {
            paths.push(path);
        }
    }
    let batch = crate::lifecycle::random_id()?;
    let directories = directories::ancestors(paths.iter().map(String::as_str));
    for path in paths {
        if dav.get(&path, MAX_FILE).await?.is_some() {
            continue;
        }
        let bytes = local::read(root, &path, MAX_FILE)?;
        local::atomic_write(
            root,
            &format!(".qiwo-sync/cleanup-backups/cache-{batch}/{path}"),
            &bytes,
        )?;
        ensure!(
            local::read(root, &path, MAX_FILE)? == bytes,
            "快照缓存发生变化"
        );
        std::fs::remove_file(local::safe_path(root, &path)?)?;
    }
    for dir in directories::deepest_first(&directories) {
        directories::remove_local(root, dir)?;
    }
    Ok(())
}

/// Only a snapshot's owner publishes it; downloaded copies are caches.
pub fn published_files(
    local: &HashMap<String, SyncFileEntry>,
    remote: &HashMap<String, SyncFileEntry>,
    actor: &str,
) -> HashMap<String, SyncFileEntry> {
    local
        .iter()
        .filter(|(p, _)| !foreign(p, actor))
        .chain(remote.iter().filter(|(p, _)| foreign(p, actor)))
        .map(|(p, e)| (p.clone(), e.clone()))
        .collect()
}
