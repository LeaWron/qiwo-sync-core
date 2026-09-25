//! Managed-space lifecycle protocol. No legacy sync entry point calls this yet.
//! Data objects are immutable; a single conditional state write moves references
//! into/out of trash. Old v1 manifests and physical paths are never modified.
use crate::inventory::{Inventory, valid_path};
use anyhow::{Result, anyhow, bail, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub mod job;
pub mod local;
pub mod migration;
mod selector;
pub mod store;
pub mod transfer;

/// Explicit transport contract, never inferred from a permissive ETag parser.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransportProfile {
    #[default]
    #[serde(rename = "standard-cas-v2")]
    Standard,
    #[serde(rename = "jianguoyun-opaque-cas-move-v1")]
    OpaqueMove,
}
impl TransportProfile {
    pub fn protocol_version(self) -> u32 {
        match self {
            Self::Standard => 2,
            Self::OpaqueMove => 3,
        }
    }
}

pub fn random_id() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow!("无法生成安全的上传 ID"))?;
    Ok(bytes.iter().map(|v| format!("{v:02x}")).collect())
}

/// Collision-safe through exclusive server creation; contains no credentials.
pub fn operation_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    hash(&(
        Utc::now().to_rfc3339(),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ))
    .expect("serializable operation id")
}
const MAX_ENTRIES: usize = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Selection {
    RetireDevice { device_id: String },
    DeleteFiles { paths: BTreeSet<String> },
    Restore { batch_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectRef {
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrashBatch {
    pub actor: String,
    pub created_at: DateTime<Utc>,
    pub selection: Selection,
    pub files: BTreeMap<String, ObjectRef>,
    pub restored_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    pub fingerprint: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedState {
    pub protocol_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_profile: Option<TransportProfile>,
    pub revision: u64,
    pub files: BTreeMap<String, ObjectRef>,
    pub retired_devices: BTreeMap<String, u64>,
    pub deleted_paths: BTreeMap<String, u64>,
    pub trash: BTreeMap<String, TrashBatch>,
    pub receipts: BTreeMap<String, Receipt>,
}

impl Default for ManagedState {
    fn default() -> Self {
        Self {
            protocol_version: 2,
            transport_profile: None,
            revision: 0,
            files: BTreeMap::new(),
            retired_devices: BTreeMap::new(),
            deleted_paths: BTreeMap::new(),
            trash: BTreeMap::new(),
            receipts: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub operation_id: String,
    pub actor: String,
    pub selection: Selection,
    pub state_digest: String,
    pub files: BTreeMap<String, ObjectRef>,
}

fn hash<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn managed_path(path: &str) -> bool {
    valid_path(path)
        && path.len() <= 4096
        && !path.split('/').any(|s| s.starts_with('.'))
        && !matches!(path, "installation.yaml" | "user.yaml")
}
fn owner(path: &str) -> Option<&str> {
    path.strip_prefix("sync/")
        .and_then(|p| p.split_once('/'))
        .map(|(id, _)| id)
}
fn owned_by(path: &str, device: &str) -> bool {
    owner(path) == Some(device)
}
fn check_files(files: &BTreeMap<String, ObjectRef>) -> Result<()> {
    ensure!(files.len() <= MAX_ENTRIES, "文件数量超出协议限制");
    for (path, file) in files {
        ensure!(managed_path(path), "文件路径不允许管理");
        ensure!(
            file.sha256.len() == 64
                && file
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "对象摘要无效"
        );
    }
    Ok(())
}

impl ManagedState {
    pub fn profile(&self) -> TransportProfile {
        self.transport_profile.unwrap_or_default()
    }
    pub fn digest(&self) -> Result<String> {
        hash(self)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(
                (self.protocol_version, self.transport_profile),
                (2, None) | (3, Some(TransportProfile::OpaqueMove))
            ),
            "管理协议版本或传输配置不受支持"
        );
        check_files(&self.files)?;
        ensure!(
            self.trash.len() <= MAX_ENTRIES
                && self.receipts.len() <= MAX_ENTRIES
                && self.retired_devices.len() <= MAX_ENTRIES
                && self.deleted_paths.len() <= MAX_ENTRIES,
            "管理记录超过协议限制"
        );
        for (id, revision) in &self.retired_devices {
            ensure!(
                identifier(id) && *revision > 0 && *revision <= self.revision,
                "停用记录无效"
            );
        }
        for (path, revision) in &self.deleted_paths {
            ensure!(
                managed_path(path) && *revision > 0 && *revision <= self.revision,
                "删除记录无效"
            );
        }
        for (id, batch) in &self.trash {
            ensure!(identifier(id) && identifier(&batch.actor), "回收站记录无效");
            check_files(&batch.files)?;
            match &batch.selection {
                Selection::RetireDevice { device_id } => {
                    ensure!(
                        identifier(device_id)
                            && device_id != &batch.actor
                            && batch.files.keys().all(|p| owned_by(p, device_id)),
                        "回收站设备范围无效"
                    );
                    if batch.restored_at.is_none() {
                        ensure!(
                            self.retired_devices.contains_key(device_id),
                            "回收站缺少停用记录"
                        );
                    }
                }
                Selection::DeleteFiles { paths } => {
                    ensure!(
                        !paths.is_empty()
                            && paths.iter().eq(batch.files.keys())
                            && paths.iter().all(|p| !owned_by(p, &batch.actor)),
                        "回收站文件范围无效"
                    );
                }
                Selection::Restore { .. } => bail!("回收站操作无效"),
            }
            ensure!(self.receipts.contains_key(id), "回收站缺少操作回执");
            if batch.restored_at.is_none() {
                ensure!(
                    batch
                        .files
                        .keys()
                        .all(|p| self.deleted_paths.contains_key(p) && !self.files.contains_key(p)),
                    "回收站缺少删除记录或原路径已占用"
                );
            }
        }
        for (id, receipt) in &self.receipts {
            ensure!(
                identifier(id)
                    && receipt.revision > 0
                    && receipt.revision <= self.revision
                    && receipt.fingerprint.len() == 64,
                "操作回执无效"
            );
        }
        for path in self.files.keys() {
            ensure!(
                !self.deleted_paths.contains_key(path)
                    && !owner(path).is_some_and(|id| self.retired_devices.contains_key(id)),
                "有效文件与停用或删除记录冲突"
            );
        }
        Ok(())
    }

    /// Must also be used by v2 upload/download planning before any Rime import.
    /// There is deliberately no age-based expiry for these records.
    pub fn blocks_path(&self, path: &str) -> bool {
        self.deleted_paths.contains_key(path)
            || owner(path).is_some_and(|id| self.retired_devices.contains_key(id))
    }

    pub fn plan(&self, actor: &str, operation_id: &str, selection: Selection) -> Result<Plan> {
        self.validate()?;
        ensure!(
            identifier(actor) && identifier(operation_id),
            "设备或操作 ID 无效"
        );
        ensure!(!self.retired_devices.contains_key(actor), "本机设备已停用");
        let files = match &selection {
            Selection::RetireDevice { device_id } => {
                ensure!(
                    identifier(device_id) && device_id != actor,
                    "不能停用本机或无效的设备 ID"
                );
                ensure!(
                    !self.retired_devices.contains_key(device_id),
                    "设备已经停用"
                );
                self.files
                    .iter()
                    .filter(|(path, _)| owned_by(path, device_id))
                    .map(|(p, f)| (p.clone(), f.clone()))
                    .collect()
            }
            Selection::DeleteFiles { paths } => {
                ensure!(
                    !paths.is_empty() && paths.len() <= MAX_ENTRIES,
                    "请选择文件"
                );
                let mut files = BTreeMap::new();
                for path in paths {
                    ensure!(
                        managed_path(path) && !owned_by(path, actor),
                        "不能清理本机正在使用的快照或内部文件"
                    );
                    let file = self
                        .files
                        .get(path)
                        .ok_or_else(|| anyhow::anyhow!("所选文件已变化，请刷新预览"))?;
                    files.insert(path.clone(), file.clone());
                }
                files
            }
            Selection::Restore { batch_id } => {
                let batch = self
                    .trash
                    .get(batch_id)
                    .ok_or_else(|| anyhow::anyhow!("回收站记录不存在"))?;
                ensure!(batch.restored_at.is_none(), "该批次已恢复");
                for path in batch.files.keys() {
                    ensure!(
                        !self.files.contains_key(path),
                        "原路径已有文件，不能覆盖恢复"
                    );
                    for id in self.retired_devices.keys().filter(|id| owned_by(path, id)) {
                        ensure!(
                            matches!(&batch.selection, Selection::RetireDevice { device_id } if device_id == id),
                            "请先恢复文件所属的停用设备"
                        );
                    }
                }
                batch.files.clone()
            }
        };
        Ok(Plan {
            operation_id: operation_id.into(),
            actor: actor.into(),
            selection,
            state_digest: hash(self)?,
            files,
        })
    }

    /// Atomically derive the next snapshot. The caller must publish it with CAS.
    /// Retry the same plan/id after an ambiguous network result; do not invent a new id.
    pub fn apply(&self, plan: &Plan, now: DateTime<Utc>) -> Result<Self> {
        self.validate()?;
        let fingerprint = hash(plan)?;
        if let Some(receipt) = self.receipts.get(&plan.operation_id) {
            ensure!(
                receipt.fingerprint == fingerprint,
                "操作 ID 已用于不同的请求"
            );
            return Ok(self.clone());
        }
        ensure!(hash(self)? == plan.state_digest, "内容已变化，请重新预览");
        let expected = self.plan(&plan.actor, &plan.operation_id, plan.selection.clone())?;
        ensure!(expected.files == plan.files, "预览内容与当前状态不一致");
        let mut next = self.clone();
        next.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("修订号溢出"))?;
        match &plan.selection {
            Selection::RetireDevice { device_id } => {
                next.retired_devices
                    .insert(device_id.clone(), next.revision);
            }
            Selection::DeleteFiles { .. } => (),
            Selection::Restore { batch_id } => {
                let batch = next.trash.get_mut(batch_id).unwrap();
                batch.restored_at = Some(now);
                if let Selection::RetireDevice { device_id } = &batch.selection {
                    next.retired_devices.remove(device_id);
                }
            }
        }
        if matches!(plan.selection, Selection::Restore { .. }) {
            for (path, file) in &plan.files {
                next.files.insert(path.clone(), file.clone());
                next.deleted_paths.remove(path);
            }
        } else {
            ensure!(
                !next.trash.contains_key(&plan.operation_id),
                "回收站批次 ID 已存在"
            );
            for path in plan.files.keys() {
                next.files.remove(path);
                next.deleted_paths.insert(path.clone(), next.revision);
            }
            next.trash.insert(
                plan.operation_id.clone(),
                TrashBatch {
                    actor: plan.actor.clone(),
                    created_at: now,
                    selection: plan.selection.clone(),
                    files: plan.files.clone(),
                    restored_at: None,
                },
            );
        }
        next.receipts.insert(
            plan.operation_id.clone(),
            Receipt {
                fingerprint,
                revision: next.revision,
            },
        );
        next.validate()?;
        Ok(next)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub selection: Selection,
    pub paths: Vec<String>,
    pub known_remote_bytes: u64,
    pub unknown_sizes: usize,
    pub blockers: Vec<String>,
    pub notes: Vec<String>,
    pub executable: bool,
}

/// Legacy inventory can preview impact but can never authorize a v2 write.
/// A v2 mutation must be planned from a validated managed state, not these facts.
pub fn preview_legacy(inventory: &Inventory, selection: Selection) -> Result<Preview> {
    let mut blockers = vec!["此同步目录尚未迁移到管理协议；完成各端接入与迁移后才能执行。".into()];
    let paths: BTreeSet<String> = match &selection {
        Selection::RetireDevice { device_id } => {
            ensure!(
                device_id != &inventory.current_device_id,
                "不能停用当前设备"
            );
            ensure!(identifier(device_id), "设备 ID 无效");
            ensure!(
                inventory.devices.iter().any(|d| d.id == *device_id),
                "设备已不在盘点结果中"
            );
            inventory
                .files
                .iter()
                .filter(|f| owned_by(&f.path, device_id))
                .map(|f| f.path.clone())
                .collect()
        }
        Selection::DeleteFiles { paths } => {
            ensure!(
                !paths.is_empty() && paths.len() <= MAX_ENTRIES,
                "请选择文件"
            );
            ensure!(
                paths
                    .iter()
                    .all(|p| managed_path(p) && !owned_by(p, &inventory.current_device_id)),
                "不能清理本机快照或内部文件"
            );
            paths.clone()
        }
        Selection::Restore { .. } => bail!("旧协议目录尚无管理回收站"),
    };
    let mut known_remote_bytes = 0u64;
    let mut unknown_sizes = 0;
    for path in &paths {
        let file = inventory
            .files
            .iter()
            .find(|f| f.path == *path)
            .ok_or_else(|| anyhow::anyhow!("所选文件已变化，请刷新"))?;
        if let Some(remote) = &file.remote {
            if let Some(size) = remote.size {
                known_remote_bytes = known_remote_bytes.saturating_add(size);
            } else {
                unknown_sizes += 1;
            }
        } else if !inventory.remote_complete {
            unknown_sizes += 1;
        }
    }
    if !inventory.remote_complete || !inventory.local_complete {
        blockers.push("盘点不完整，当前列表不能作为最终清理范围。".into());
    }
    if !matches!(inventory.manifest_status.as_str(), "ok" | "missing") {
        blockers.push("同步清单无法确认，需要先解决清单读取问题。".into());
    }
    Ok(Preview {
        selection,
        paths: paths.into_iter().collect(),
        known_remote_bytes,
        unknown_sizes,
        blockers,
        notes: vec![
            "此处仅预览，不会改变任何文件。".into(),
            "将来执行时先移入回收站；回收站仍占空间，不等于立即释放容量。".into(),
            "停用设备或删除词库快照不会撤销已合并到词库的词条。".into(),
        ],
        executable: false,
    })
}
