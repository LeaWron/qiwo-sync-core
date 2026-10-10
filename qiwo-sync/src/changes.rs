//! Applied local changes and durable, generation-scoped native acknowledgements.
//! Immutable events are written before replacement; a crash between intent and
//! commit is recovered from the target hash. Uploads never create local events.
use crate::lifecycle::{local, random_id};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, io::Read, path::Path};

const EVENTS: &str = ".qiwo-sync/applied-changes";
const TASKS: &str = ".qiwo-sync/apply-tasks";
const FILE_RESULT: &str = ".qiwo-sync/file-sync-result.json";
const MAX_STATE: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Purpose {
    LearningSnapshot,
    Configuration,
    Schema,
    Dictionary,
    Internal,
    Unknown,
}

/// Recognize named formats, not "anything outside sync/" or any *.txt file.
pub fn classify(path: &str, actor: &str) -> Purpose {
    let parts: Vec<_> = path.split('/').collect();
    let lower = path.to_ascii_lowercase();
    let name = parts.last().copied().unwrap_or("").to_ascii_lowercase();
    if parts.iter().any(|p| p.starts_with('.'))
        || lower.starts_with("build/")
        || matches!(name.as_str(), "installation.yaml" | "user.yaml")
    {
        return Purpose::Internal;
    }
    if parts.len() == 3 && parts[0] == "sync" && parts[1] != actor && name.ends_with(".userdb.txt")
    {
        return Purpose::LearningSnapshot;
    }
    if name.ends_with(".schema.yaml") {
        return Purpose::Schema;
    }
    if name.ends_with(".dict.yaml") || name == "custom_phrase.txt" {
        return Purpose::Dictionary;
    }
    if name.ends_with(".custom.yaml")
        || matches!(
            name.as_str(),
            "default.yaml" | "weasel.yaml" | "squirrel.yaml" | "ibus_rime.yaml" | "fcitx5.yaml"
        )
    {
        return Purpose::Configuration;
    }
    Purpose::Unknown
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedChange {
    pub id: String,
    pub path: String,
    pub purpose: Purpose,
    pub operation: String,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub committed: bool,
}

fn hash(path: &Path) -> Result<Option<String>> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        file.metadata()?.is_file(),
        "Sync target is not a regular file"
    );
    let mut digest = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(Some(format!("{:x}", digest.finalize())))
}

/// Called in a blocking task with the sync guard held. No await between the
/// durable intent, atomic file replacement and committed event.
pub fn apply(
    root: &Path,
    path: &str,
    staged: Option<&Path>,
    actor: &str,
) -> Result<Option<AppliedChange>> {
    let target = local::safe_path(root, path)?;
    let before = hash(&target)?;
    let after = staged.map(hash).transpose()?.flatten();
    if before == after {
        if let Some(p) = staged {
            fs::remove_file(p)?;
        }
        return Ok(None);
    }
    let purpose = classify(path, actor);
    let mut change = AppliedChange {
        id: random_id()?,
        path: path.into(),
        purpose,
        operation: if before.is_none() {
            "added"
        } else if after.is_none() {
            "deleted"
        } else {
            "modified"
        }
        .into(),
        before_hash: before,
        after_hash: after,
        committed: false,
    };
    let event = format!("{EVENTS}/{}.json", change.id);
    if purpose != Purpose::Internal {
        local::atomic_write(root, &event, &serde_json::to_vec(&change)?)?;
    }
    if let Some(staged) = staged {
        fs::create_dir_all(target.parent().unwrap())?;
        fs::rename(staged, &target)?;
    } else {
        fs::remove_file(&target)?;
    }
    change.committed = true;
    if purpose == Purpose::Internal {
        return Ok(None);
    }
    local::atomic_write(root, &event, &serde_json::to_vec(&change)?)?;
    Ok(Some(change))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyKind {
    Merge,
    Deploy,
}
impl ApplyKind {
    pub fn accepts(self, p: Purpose) -> bool {
        match self {
            Self::Merge => p == Purpose::LearningSnapshot,
            Self::Deploy => matches!(
                p,
                Purpose::Configuration | Purpose::Schema | Purpose::Dictionary
            ),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyTask {
    pub id: String,
    pub kind: ApplyKind,
    pub changes: Vec<String>,
    pub outcome: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileResult {
    pub id: String,
    pub outcome: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingState {
    pub native_task: Option<NativeTask>,
    pub pending_merge: Vec<AppliedChange>,
    pub pending_deploy: Vec<AppliedChange>,
    pub unknown: Vec<AppliedChange>,
    pub apply_tasks: Vec<ApplyTask>,
    pub file_sync: Option<FileResult>,
}

fn records<T: serde::de::DeserializeOwned>(root: &Path, directory: &str) -> Result<Vec<T>> {
    let path = local::safe_path(root, directory)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Invalid state filename"))?;
        if name.ends_with(".json") {
            ensure!(
                valid_id(name.trim_end_matches(".json")),
                "Invalid state filename"
            );
            names.push(name);
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|name| {
            Ok(serde_json::from_slice(&local::read(
                root,
                &format!("{directory}/{name}"),
                MAX_STATE,
            )?)?)
        })
        .collect()
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn state(root: &Path) -> Result<PendingState> {
    let tasks: Vec<ApplyTask> = records(root, TASKS)?;
    let acknowledged: BTreeSet<_> = tasks
        .iter()
        .filter(|t| t.outcome == "succeeded")
        .flat_map(|t| t.changes.iter().cloned())
        .collect();
    let mut result = PendingState {
        apply_tasks: tasks,
        ..Default::default()
    };
    result.native_task = native_state(root)?;
    for mut change in records::<AppliedChange>(root, EVENTS)? {
        ensure!(
            valid_id(&change.id) && crate::inventory::valid_path(&change.path),
            "Invalid change record"
        );
        if acknowledged.contains(&change.id) {
            continue;
        }
        if !change.committed {
            if hash(&local::safe_path(root, &change.path)?)? != change.after_hash {
                continue;
            }
            change.committed = true;
        }
        match change.purpose {
            Purpose::LearningSnapshot => result.pending_merge.push(change),
            Purpose::Configuration | Purpose::Schema | Purpose::Dictionary => {
                result.pending_deploy.push(change)
            }
            Purpose::Unknown => result.unknown.push(change),
            Purpose::Internal => (),
        }
    }
    let path = local::safe_path(root, FILE_RESULT)?;
    if path.exists() {
        result.file_sync = Some(serde_json::from_slice(&local::read(
            root,
            FILE_RESULT,
            MAX_STATE,
        )?)?);
    }
    Ok(result)
}

pub fn begin(root: &Path, kind: ApplyKind) -> Result<ApplyTask> {
    let state = state(root)?;
    let changes = if kind == ApplyKind::Merge {
        state.pending_merge
    } else {
        state.pending_deploy
    };
    let task = ApplyTask {
        id: random_id()?,
        kind,
        changes: changes.into_iter().map(|c| c.id).collect(),
        outcome: "running".into(),
    };
    local::atomic_write(
        root,
        &format!("{TASKS}/{}.json", task.id),
        &serde_json::to_vec(&task)?,
    )?;
    Ok(task)
}
pub fn complete(root: &Path, id: &str, outcome: &str) -> Result<()> {
    ensure!(valid_id(id), "Invalid apply task ID");
    ensure!(
        matches!(outcome, "succeeded" | "failed" | "cancelled"),
        "Invalid apply outcome"
    );
    let path = format!("{TASKS}/{id}.json");
    let mut task: ApplyTask = serde_json::from_slice(&local::read(root, &path, MAX_STATE)?)?;
    ensure!(
        task.id == id && task.outcome == "running",
        "Apply task already completed"
    );
    task.outcome = outcome.into();
    local::atomic_write(root, &path, &serde_json::to_vec(&task)?)
}
pub fn file_result(root: &Path, id: &str, outcome: &str) -> Result<()> {
    ensure!(
        valid_id(id) && matches!(outcome, "running" | "succeeded" | "failed" | "cancelled"),
        "Invalid sync outcome"
    );
    local::atomic_write(
        root,
        FILE_RESULT,
        &serde_json::to_vec(&FileResult {
            id: id.into(),
            outcome: outcome.into(),
        })?,
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTask {
    pub id: String,
    pub phase: String,
    pub cancel_requested: bool,
}
const NATIVE: &str = ".qiwo-sync/native-task.json";
pub fn native_state(root: &Path) -> Result<Option<NativeTask>> {
    if !local::safe_path(root, NATIVE)?.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&local::read(
        root, NATIVE, MAX_STATE,
    )?)?))
}
pub fn native_task(
    root: &Path,
    id: Option<&str>,
    phase: Option<&str>,
    cancel: bool,
) -> Result<NativeTask> {
    // Separate from the file-operation lock: cancellation must work while the
    // network process owns that lock. Only short metadata writes are serialized.
    let path = local::safe_path(root, ".qiwo-sync/native-state.lock")?;
    fs::create_dir_all(path.parent().unwrap())?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock()?;
    let result = (|| {
        let mut task = if let Some(id) = id {
            ensure!(valid_id(id), "Invalid native task ID");
            let task = native_state(root)?.ok_or_else(|| anyhow::anyhow!("No native task"))?;
            ensure!(task.id == id, "Native task changed");
            task
        } else {
            NativeTask {
                id: random_id()?,
                phase: "waiting-export".into(),
                cancel_requested: false,
            }
        };
        if let Some(phase) = phase {
            ensure!(
                matches!(
                    phase,
                    "waiting-export"
                        | "exporting"
                        | "network"
                        | "waiting-merge"
                        | "merging"
                        | "waiting-deploy"
                        | "deploying"
                        | "succeeded"
                        | "failed"
                        | "cancelled"
                ),
                "Invalid native phase"
            );
            task.phase = phase.into();
        }
        if cancel {
            ensure!(
                !matches!(task.phase.as_str(), "succeeded" | "failed" | "cancelled"),
                "Native task already finished"
            );
            task.cancel_requested = true;
            local::atomic_write(
                root,
                &format!(".qiwo-sync/native-cancel/{}", task.id),
                b"cancel\n",
            )?;
        }
        local::atomic_write(root, NATIVE, &serde_json::to_vec(&task)?)?;
        Ok(task)
    })();
    file.unlock()?;
    result
}
