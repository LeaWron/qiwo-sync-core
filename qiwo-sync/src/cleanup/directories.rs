//! Only previewed foreign-device directories may be removed, deepest first.
use super::{Dav, Plan, Report, Selection, SyncRequest, allowed, local};
use crate::inventory::dav::Reader;
use anyhow::{Result, ensure};
use std::{collections::BTreeSet, io::ErrorKind, path::Path};

fn allowed_directory(path: &str, actor: &str) -> bool {
    allowed(&format!("{path}/_"), actor)
}

pub(super) fn ancestors<'a>(paths: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    for path in paths {
        let mut current = path;
        while let Some((parent, _)) = current.rsplit_once('/') {
            if parent == "sync" {
                break;
            }
            result.insert(parent.to_owned());
            current = parent;
        }
    }
    result
}

pub(super) fn deepest_first(dirs: &BTreeSet<String>) -> Vec<&str> {
    let mut dirs: Vec<_> = dirs.iter().map(String::as_str).collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.split('/').count()));
    dirs
}

pub(super) fn valid(plan: &Plan, actor: &str) -> bool {
    let parents = ancestors(plan.files.keys().map(String::as_str));
    plan.directories.len() <= 256
        && plan.directories.iter().all(|p| {
            allowed_directory(p, actor)
                && match &plan.selection {
                    Selection::Device { device_id } => {
                        p == &format!("sync/{device_id}")
                            || p.starts_with(&format!("sync/{device_id}/"))
                    }
                    Selection::Files { .. } => parents.contains(p),
                }
        })
}

pub(super) fn reader(request: &SyncRequest) -> Result<Reader> {
    Reader::new(
        request.remote_url.as_deref().unwrap_or(""),
        request.username.as_deref().unwrap_or(""),
        request.password.as_deref().unwrap_or(""),
    )
}

pub(super) async fn preview(
    request: &SyncRequest,
    selection: &Selection,
    paths: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let mut dirs = ancestors(paths.iter().map(String::as_str));
    if let Selection::Device { device_id } = selection {
        let prefix = format!("sync/{device_id}");
        ensure!(
            allowed_directory(&prefix, &request.device_id),
            "设备目录无效"
        );
        let path = local::safe_path(&request.rime_user_dir, &prefix)?;
        if path.try_exists()? {
            ensure!(path.is_dir(), "设备路径不是目录");
            for entry in walkdir::WalkDir::new(path).follow_links(false) {
                let entry = entry?;
                ensure!(!entry.file_type().is_symlink(), "设备目录含符号链接");
                if entry.file_type().is_dir() {
                    let relative = entry
                        .path()
                        .strip_prefix(&request.rime_user_dir)?
                        .to_string_lossy()
                        .replace('\\', "/");
                    if allowed_directory(&relative, &request.device_id) {
                        dirs.insert(relative);
                    }
                }
                ensure!(dirs.len() <= 256, "目录超过单次清理上限");
            }
        }
        let reader = reader(request)?;
        let mut pending = vec![prefix];
        let mut visited = BTreeSet::new();
        while let Some(dir) = pending.pop() {
            ensure!(
                visited.insert(dir.clone()) && visited.len() <= 256,
                "目录扫描超出限制"
            );
            if let Some(list) = reader.list(&dir).await? {
                ensure!(
                    list.complete && list.is_collection,
                    "目录状态无法确认，请重新扫描"
                );
                dirs.insert(dir);
                for entry in list.entries {
                    if entry.is_dir && allowed_directory(&entry.path, &request.device_id) {
                        pending.push(entry.path);
                    }
                }
            }
        }
    }
    ensure!(dirs.len() <= 256, "目录超过单次清理上限");
    Ok(dirs)
}

/// rmdir is atomic with respect to emptiness; never recursively delete local data.
/// None means already absent, false means retained because it is nonempty.
pub(super) fn remove_local(root: &Path, dir: &str) -> Result<Option<bool>> {
    let path = local::safe_path(root, dir)?;
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(Some(true)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == ErrorKind::DirectoryNotEmpty => Ok(Some(false)),
        Err(e) => Err(e.into()),
    }
}

pub(super) async fn execute(
    request: &SyncRequest,
    plan: &Plan,
    dav: &Dav,
    report: &mut Report,
) -> Result<()> {
    let reader = reader(request)?;
    for dir in deepest_first(&plan.directories) {
        if let Some(list) = reader.list(dir).await? {
            ensure!(
                list.complete && list.is_collection,
                "无法确认目录内容，停止删除：{dir}"
            );
            if list.entries.is_empty() {
                dav.delete_empty_directory(dir, &request.device_id).await?;
                ensure!(
                    reader.list(dir).await?.is_none(),
                    "目录删除结果未确认：{dir}"
                );
                report.remote_directories_deleted.push(dir.to_owned());
            } else {
                report.directories_kept.push(format!("云端 {dir}/"));
            }
        }
        match remove_local(&request.rime_user_dir, dir)? {
            Some(true) => report.local_directories_removed.push(dir.to_owned()),
            Some(false) => report.directories_kept.push(format!("本机 {dir}/")),
            None => {}
        }
        local::atomic_write(
            &request.rime_user_dir,
            &format!(".qiwo-sync/cleanup-backups/{}/result.json", plan.id),
            &serde_json::to_vec_pretty(report)?,
        )?;
    }
    Ok(())
}
