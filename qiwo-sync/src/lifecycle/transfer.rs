//! V2 transfer path. Lifecycle records are preserved by every writer.
//! Call only inside a native frontend's exclusive export/import session.
use super::{
    hash, local, operation_id, owner,
    store::{MAX_OBJECT_BYTES, Store, reference},
};
use crate::types::{SyncMode, SyncRequest, SyncSummary};
use anyhow::{Result, anyhow, ensure};
use std::collections::BTreeSet;

pub fn store(request: &SyncRequest) -> Result<Store> {
    Ok(Store::new(
        request
            .remote_url
            .as_deref()
            .ok_or_else(|| anyhow!("缺少同步地址"))?,
        request.username.as_deref().unwrap_or(""),
        request.password.as_deref().unwrap_or(""),
    )?
    .with_journal(&request.rime_user_dir))
}

/// Preflight before export and again immediately before import. Errors stop the
/// native operation; lack of connectivity never permits use of a cached state.
pub async fn prepare(request: &SyncRequest) -> Result<()> {
    let store = store(request)?;
    store.ensure_management_writable()?;
    let enrollment =
        local::enrollment(&request.rime_user_dir)?.ok_or_else(|| anyhow!("尚未加入管理空间"))?;
    let (state, _) = store.load().await?;
    enrollment.verify(&store, &state, &request.device_id)?;
    local::quarantine(&request.rime_user_dir, &state, &request.device_id)?;
    Ok(())
}

pub async fn execute(request: &SyncRequest) -> Result<SyncSummary> {
    ensure!(!request.dry_run, "管理同步请使用独立预览接口");
    ensure!(
        request.mode != SyncMode::InitFrost,
        "管理同步不负责初始化方案"
    );
    let root = &request.rime_user_dir;
    let store = store(request)?;
    store.ensure_management_writable()?;
    let mut baseline = local::enrollment(root)?.ok_or_else(|| anyhow!("尚未加入管理空间"))?;
    let (state, etag) = store.load().await?;
    baseline.verify(&store, &state, &request.device_id)?;
    local::quarantine(root, &state, &request.device_id)?;
    let local_files = local::scan(root)?;
    let paths: BTreeSet<_> = local_files.keys().chain(state.files.keys()).collect();
    let mut next = state.clone();
    let mut downloads = Vec::new();
    let mut summary = SyncSummary::new(request.mode, request.frontend, &request.device_id);
    let scope = |p: &str| request.mode != SyncMode::SyncUserDict || p.starts_with("sync/");
    for path in paths {
        if !scope(path)
            || state.blocks_path(path)
            || !super::selector::FileSelector.should_sync(path)
        {
            continue;
        }
        let local = local_files.get(path);
        let remote = state.files.get(path);
        if local == remote {
            summary.skipped += 1;
            continue;
        }
        let foreign = owner(path).is_some_and(|id| id != request.device_id);
        let upload = !foreign
            && local.is_some()
            && request.mode != SyncMode::Pull
            && (request.mode == SyncMode::Push
                || remote.is_none()
                || (local != baseline.files.get(path) && remote == baseline.files.get(path)));
        if upload {
            let bytes = local::read(root, path, MAX_OBJECT_BYTES)?;
            ensure!(
                Some(&reference(&bytes)) == local,
                "本机文件已变化，请重试同步"
            );
            next.files.insert(
                path.clone(),
                store.put_object_for(bytes, state.profile()).await?,
            );
            summary.uploaded += 1;
        } else if let Some(remote) = remote {
            downloads.push((path.clone(), remote.clone()));
        }
    }
    if summary.uploaded > 0 {
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("修订号溢出"))?;
        store.replace(&next, &etag).await?;
    }
    let staging = format!(".qiwo-sync/managed-staging/{}", operation_id());
    // Stage and verify every download before replacing any local file.
    for (path, object) in &downloads {
        let bytes = store.read_object(object).await?;
        local::atomic_write(root, &format!("{staging}/{path}"), &bytes)?;
    }
    let (fresh, _) = store.load().await?;
    ensure!(
        hash(&fresh)? == hash(&next)?,
        "云端状态在同步中改变，请重试；未导入旧快照"
    );
    for (path, object) in downloads {
        let bytes = local::read(root, &format!("{staging}/{path}"), MAX_OBJECT_BYTES)?;
        ensure!(reference(&bytes) == object, "暂存文件校验失败");
        if local::safe_path(root, &path)?.try_exists()? {
            local::backup(root, &path)?;
            summary.conflicts_backed_up += 1;
        }
        local::atomic_write(root, &path, &bytes)?;
        summary.downloaded += 1;
    }
    local::quarantine(root, &fresh, &request.device_id)?;
    baseline.files = fresh.files;
    baseline.revision = fresh.revision;
    baseline.save(root)?;
    summary
        .messages
        .push("管理同步完成；已保留回收记录和本机恢复副本".into());
    Ok(summary)
}
