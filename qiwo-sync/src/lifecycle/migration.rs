//! Explicit, resumable snapshot migration. The legacy namespace is read-only.
use super::selector::FileSelector;
use super::{
    ManagedState, ObjectRef, Receipt, TransportProfile, check_files, hash, identifier,
    managed_path,
    store::{Store, reference},
};
use crate::inventory::Inventory;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_MIGRATION_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_profile: Option<TransportProfile>,
    pub operation_id: String,
    pub actor: String,
    pub endpoint: String,
    pub files: BTreeMap<String, ObjectRef>,
    pub excluded: Vec<String>,
}
impl MigrationPlan {
    fn validate(&self, store: &Store) -> Result<()> {
        ensure!(
            self.transport_profile.is_some(),
            "此预览来自旧版本，请关闭后重新预览迁移"
        );
        ensure!(
            identifier(&self.operation_id) && identifier(&self.actor),
            "迁移操作或设备 ID 无效"
        );
        ensure!(
            self.endpoint == store.endpoint_id(),
            "迁移计划与服务器不匹配"
        );
        check_files(&self.files)?;
        ensure!(
            self.files.keys().all(|p| FileSelector.should_sync(p)),
            "迁移包含不受支持的文件"
        );
        let bytes = self
            .files
            .values()
            .try_fold(0u64, |n, f| n.checked_add(f.size));
        ensure!(
            bytes.is_some_and(|n| n <= MAX_MIGRATION_BYTES),
            "迁移内容超过 512 MiB 限制"
        );
        Ok(())
    }
    pub fn state(&self, store: &Store) -> Result<ManagedState> {
        self.validate(store)?;
        let mut state = ManagedState {
            protocol_version: self.transport_profile.unwrap().protocol_version(),
            transport_profile: self
                .transport_profile
                .filter(|p| *p != TransportProfile::Standard),
            revision: 1,
            files: self.files.clone(),
            ..Default::default()
        };
        state.receipts.insert(
            self.operation_id.clone(),
            Receipt {
                fingerprint: hash(self)?,
                revision: 1,
            },
        );
        state.validate()?;
        Ok(state)
    }
}

/// Reads actual bytes for the displayed snapshot; file times are not content identity.
/// Local files join via the v2 transfer engine after explicit enrollment.
pub async fn preview(
    store: &Store,
    inventory: &Inventory,
    actor: &str,
    operation_id: &str,
) -> Result<MigrationPlan> {
    preview_with_profile(
        store,
        inventory,
        actor,
        operation_id,
        store.proposed_profile(),
    )
    .await
}

pub async fn preview_with_profile(
    store: &Store,
    inventory: &Inventory,
    actor: &str,
    operation_id: &str,
    profile: TransportProfile,
) -> Result<MigrationPlan> {
    store.ensure_management_writable()?;
    ensure!(
        inventory.remote_complete && inventory.remote_status == "ok",
        "云端盘点不完整，不能迁移"
    );
    ensure!(
        matches!(inventory.manifest_status.as_str(), "ok" | "missing"),
        "请先修复无法读取的旧同步清单"
    );
    ensure!(
        store.load_optional().await?.is_none(),
        "管理空间已存在，请使用加入流程"
    );
    let mut plan = MigrationPlan {
        transport_profile: Some(profile),
        operation_id: operation_id.into(),
        actor: actor.into(),
        endpoint: store.endpoint_id(),
        files: BTreeMap::new(),
        excluded: Vec::new(),
    };
    let mut total = 0u64;
    for file in &inventory.files {
        if file.remote.is_none() {
            continue;
        }
        if !managed_path(&file.path) || !FileSelector.should_sync(&file.path) {
            plan.excluded.push(file.path.clone());
            continue;
        }
        let bytes = store.read_legacy(&file.path).await?;
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("迁移大小溢出"))?;
        ensure!(total <= MAX_MIGRATION_BYTES, "迁移内容超过 512 MiB 限制");
        plan.files.insert(file.path.clone(), reference(&bytes));
    }
    plan.excluded.sort();
    plan.validate(store)?;
    Ok(plan)
}

/// Persist this exact plan before calling. Lost final responses are reconciled
/// through the durable receipt; retries never replace an existing managed space.
pub async fn execute(store: &Store, plan: &MigrationPlan) -> Result<ManagedState> {
    store.ensure_management_writable()?;
    let state = plan.state(store)?;
    if let Some((existing, _)) = store.load_optional().await? {
        ensure!(
            existing.profile() == state.profile(),
            "管理空间传输配置发生变化，不能继续原迁移"
        );
        ensure!(
            existing
                .receipts
                .get(&plan.operation_id)
                .is_some_and(|r| r.fingerprint == hash(plan).unwrap_or_default()),
            "另一迁移已建立管理空间，不能覆盖"
        );
        return Ok(existing);
    }
    // A new probe ID on each retry avoids reusing an old conditional-write result.
    store.prepare_profile(state.profile()).await?;
    for (path, expected) in &plan.files {
        let bytes = store.read_legacy(path).await?;
        ensure!(
            reference(&bytes) == *expected,
            "旧文件已变化，请重新预览迁移"
        );
        ensure!(
            store.put_object_for(bytes, state.profile()).await? == *expected,
            "迁移对象验证失败"
        );
    }
    store.initialize(&state).await?;
    let (published, _) = store.load().await?;
    ensure!(
        published
            .receipts
            .get(&plan.operation_id)
            .is_some_and(|r| r.fingerprint == hash(plan).unwrap_or_default()),
        "迁移发布结果不匹配"
    );
    Ok(published)
}
