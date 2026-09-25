//! Requests executed by the native frontend while export/import is excluded.
use super::{
    Plan, local,
    migration::{self, MigrationPlan},
    transfer,
};
use crate::types::SyncRequest;
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Job {
    CleanupResiduals {
        plan: crate::cleanup::Plan,
    },
    Migrate {
        plan: MigrationPlan,
    },
    Cleanup {
        plan: Plan,
    },
    Join {
        migration_id: String,
        actor: String,
        endpoint: String,
        state_digest: String,
        files: std::collections::BTreeMap<String, super::ObjectRef>,
    },
}

pub async fn run(request: &SyncRequest, job: &Job) -> Result<()> {
    if let Job::CleanupResiduals { plan } = job {
        crate::cleanup::execute(request, plan).await?;
        return Ok(());
    }
    let store = transfer::store(request)?;
    store.ensure_management_writable()?;
    let root = &request.rime_user_dir;
    match job {
        Job::CleanupResiduals { .. } => unreachable!(),
        Job::Migrate { plan } => {
            ensure!(plan.actor == request.device_id, "迁移设备身份已变化");
            if let Some(enrollment) = local::enrollment(root)? {
                let (existing, _) = store.load().await?;
                enrollment.verify(&store, &existing, &request.device_id)?;
                ensure!(
                    enrollment.migration_id == plan.operation_id,
                    "本机已加入另一迁移，不能重新初始化"
                );
            }
            let state = migration::execute(&store, plan).await?;
            local::enroll(root, &store, &state, &request.device_id, &plan.operation_id)?;
            // Pull first: preserve divergent local files before allowing exports.
            let mut pull = request.clone();
            pull.mode = crate::types::SyncMode::Pull;
            transfer::execute(&pull).await?;
        }
        Job::Join {
            migration_id,
            actor,
            endpoint,
            state_digest,
            files,
        } => {
            ensure!(*actor == request.device_id, "加入预览的设备身份已变化");
            ensure!(
                *endpoint == store.endpoint_id(),
                "加入预览的账号或地址已变化"
            );
            let (state, _) = store.load().await?;
            ensure!(
                *endpoint == store.endpoint_id()
                    && *state_digest == super::hash(&state)?
                    && *files == state.files,
                "加入预览已变化，请重新预览"
            );
            ensure!(
                state
                    .receipts
                    .get(migration_id)
                    .is_some_and(|r| r.revision == 1),
                "初始迁移回执无效"
            );
            store.prepare_profile(state.profile()).await?;
            local::enroll(root, &store, &state, &request.device_id, migration_id)?;
            let mut pull = request.clone();
            pull.mode = crate::types::SyncMode::Pull;
            transfer::execute(&pull).await?;
        }
        Job::Cleanup { plan } => {
            ensure!(plan.actor == request.device_id, "清理设备身份已变化");
            let enrollment =
                local::enrollment(root)?.ok_or_else(|| anyhow!("本机尚未加入管理空间"))?;
            let (state, _) = store.load().await?;
            enrollment.verify(&store, &state, &request.device_id)?;
            let next = store.commit(plan).await?;
            local::quarantine(root, &next, &request.device_id)?;
            // Restore bytes through the same verified transfer path; no native
            // import is run by a management task itself.
            let mut pull = request.clone();
            pull.mode = crate::types::SyncMode::Pull;
            transfer::execute(&pull).await?;
        }
    }
    Ok(())
}
