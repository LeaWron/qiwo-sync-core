//! Opaque CAS and MOVE publication. No unconditional PUT ever targets final data.
use super::{MAX_STATE_BYTES, Store, Validator, capability_error, reference};
use crate::lifecycle::{self, ManagedState, TransportProfile, local};
use anyhow::{Result, anyhow, ensure};
use reqwest::{Method, StatusCode, Url};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Upload {
    endpoint: String,
    target: String,
    sha256: String,
    size: u64,
    staging_id: String,
}

fn race_error(reason: &str, a: StatusCode, b: StatusCode) -> anyhow::Error {
    let detail = format!("{reason}（HTTP {} / HTTP {}）", a.as_u16(), b.as_u16());
    if a.is_success() && b.is_success() {
        capability_error(&detail)
    } else {
        anyhow!(
            "{detail}。本次能力检测未完成，不能据此确认兼容性；请检查服务器状态后再核对原任务。"
        )
    }
}

impl Store {
    fn record_probe(&self, prefix: &str, phase: &str, a: StatusCode, b: StatusCode) -> Result<()> {
        if let Some(root) = &self.journal_root {
            let report = serde_json::json!({"endpoint": self.endpoint_id(), "probe": prefix,
                "phase": phase, "statuses": [a.as_u16(), b.as_u16()], "checkedAt": chrono::Utc::now()});
            local::atomic_write(
                root,
                ".qiwo-sync/managed-capability-last-check.json",
                &serde_json::to_vec(&report)?,
            )?;
        }
        Ok(())
    }
    async fn move_absent(&self, source: Url, target: Url) -> Result<StatusCode> {
        ensure!(
            source.origin() == self.url.origin() && target.origin() == self.url.origin(),
            "不能跨服务器发布"
        );
        self.auth(self.client.request(Method::from_bytes(b"MOVE")?, source))
            .header("Destination", target.as_str())
            .header("Overwrite", "F")
            .send()
            .await
            .map(|r| r.status())
            .map_err(|_| anyhow!("发布结果未知，请使用原任务重试核对"))
    }

    async fn put_staging(&self, url: Url, bytes: &[u8]) -> Result<()> {
        let result = self
            .auth(self.client.put(url.clone()))
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| anyhow!("暂存上传结果未知，请使用原任务重试"))?;
        ensure!(
            matches!(
                result.status(),
                StatusCode::CREATED | StatusCode::OK | StatusCode::NO_CONTENT
            ),
            "暂存上传失败：HTTP {}",
            result.status().as_u16()
        );
        let (actual, _) = self.read_at(url, bytes.len()).await?;
        ensure!(actual == bytes, "暂存文件校验失败，停止发布");
        Ok(())
    }

    fn publication_matches(target: &str, expected: &[u8], actual: &[u8]) -> Result<()> {
        if target != "state.json" {
            ensure!(actual == expected, "已有对象内容不匹配，禁止覆盖");
            return Ok(());
        }
        let initial: ManagedState = serde_json::from_slice(expected)?;
        let observed: ManagedState = serde_json::from_slice(actual)
            .map_err(|_| anyhow!("已存在的管理状态无法校验，禁止覆盖"))?;
        observed.validate()?;
        ensure!(
            observed.profile() == initial.profile()
                && initial.receipts.iter().all(|(id, receipt)| observed
                    .receipts
                    .get(id)
                    .is_some_and(
                        |r| r.fingerprint == receipt.fingerprint && r.revision == receipt.revision
                    )),
            "另一迁移已建立管理空间，请关闭预览并使用加入流程"
        );
        Ok(())
    }

    /// Journal is durable before uploading. Retrying a target/content pair reuses
    /// the same random path; an unknown MOVE outcome is reconciled from the target.
    pub(super) async fn publish_absent(&self, target: &str, bytes: &[u8]) -> Result<()> {
        self.ensure_management_writable()?;
        let expected = reference(bytes);
        ensure!(
            target == "state.json" || target == format!("objects/{}", expected.sha256),
            "发布目标无效"
        );
        let final_url = self.managed_url(target)?;
        let limit = if target == "state.json" {
            MAX_STATE_BYTES
        } else {
            bytes.len()
        };
        if let Some((actual, _)) = self.read_optional_at(final_url.clone(), limit).await? {
            return Self::publication_matches(target, bytes, &actual);
        }
        let root = self
            .journal_root
            .as_ref()
            .ok_or_else(|| anyhow!("缺少本机上传日志目录"))?;
        let key = lifecycle::hash(&(self.endpoint_id(), target, &expected))?;
        let journal = format!(".qiwo-sync/managed-uploads/{key}.json");
        let upload: Upload = if local::safe_path(root, &journal)?.try_exists()? {
            serde_json::from_slice(&local::read(root, &journal, 4096)?)
                .map_err(|_| anyhow!("上传任务日志损坏"))?
        } else {
            let upload = Upload {
                endpoint: self.endpoint_id(),
                target: target.into(),
                sha256: expected.sha256.clone(),
                size: expected.size,
                staging_id: lifecycle::random_id()?,
            };
            local::atomic_write(root, &journal, &serde_json::to_vec(&upload)?)?;
            upload
        };
        ensure!(
            upload.endpoint == self.endpoint_id()
                && upload.target == target
                && upload.sha256 == expected.sha256
                && upload.size == expected.size
                && upload.staging_id.len() == 64
                && upload.staging_id.bytes().all(|b| b.is_ascii_hexdigit()),
            "上传任务与当前内容或端点不一致"
        );
        let source = self.managed_url(&format!("staging/{}", upload.staging_id))?;
        match self.read_optional_at(source.clone(), bytes.len()).await? {
            Some((actual, _)) => ensure!(actual == bytes, "已有暂存内容不匹配，停止发布"),
            None => self.put_staging(source.clone(), bytes).await?,
        }
        let result = self.move_absent(source, final_url.clone()).await;
        if let Ok(status) = result {
            ensure!(
                matches!(
                    status,
                    StatusCode::CREATED | StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED
                ),
                "发布结果未确认（HTTP {}），请使用原任务核对；禁止覆盖目标",
                status.as_u16()
            );
        }
        // A response code alone never proves publication, including 409/412.
        let (actual, _) = self
            .read_optional_at(final_url, limit)
            .await?
            .ok_or_else(|| anyhow!("目标尚未确认，请使用原任务重试；已保留暂存日志"))?;
        Self::publication_matches(target, bytes, &actual)
    }

    /// Explicit diagnostic API; never publishes user objects or state and never
    /// re-enables a quarantined provider after a successful sample.
    pub async fn diagnose_compatible(&self) -> Result<()> {
        self.prepare_compatible().await
    }

    /// Probe files are isolated under a CSPRNG directory and retained for diagnosis.
    /// This runs only during an explicit migration/join, never during ordinary sync.
    pub(super) async fn prepare_compatible(&self) -> Result<()> {
        for path in ["./", "objects/", "staging/", "probes/"] {
            self.collection(path).await?;
        }
        let prefix = format!("probes/compat-{}/", lifecycle::random_id()?);
        self.collection(&prefix).await?;
        let cas = self.managed_url(&format!("{prefix}cas"))?;
        self.put_staging(cas.clone(), b"initial").await?;
        let (_, first) = self.read_at(cas.clone(), 32).await?;
        Validator::parse(TransportProfile::OpaqueMove, &first)?;
        let send = |value: &'static [u8], token: String| {
            self.auth(self.client.put(cas.clone()))
                .header(reqwest::header::IF_MATCH, token)
                .body(value)
                .send()
        };
        let matched = send(b"updated", first.clone())
            .await
            .map_err(|_| anyhow!("条件更新检测中断"))?;
        let (updated, second) = self.read_at(cas.clone(), 32).await?;
        Validator::parse(TransportProfile::OpaqueMove, &second)?;
        ensure!(
            matches!(matched.status(), StatusCode::OK | StatusCode::NO_CONTENT)
                && updated == b"updated"
                && second != first,
            "{}",
            capability_error("原样条件标记不能可靠更新内容")
        );
        let stale = send(b"stale", first)
            .await
            .map_err(|_| anyhow!("过期条件检测中断"))?;
        let (unchanged, token) = self.read_at(cas.clone(), 32).await?;
        ensure!(
            stale.status() == StatusCode::PRECONDITION_FAILED
                && unchanged == b"updated"
                && token == second,
            "{}",
            capability_error("服务器未阻止过期条件覆盖")
        );
        let (a, b) =
            futures_util::future::join(send(b"race-a", second.clone()), send(b"race-b", second))
                .await;
        let a = a.map_err(|_| anyhow!("并发条件更新检测中断"))?.status();
        let b = b.map_err(|_| anyhow!("并发条件更新检测中断"))?.status();
        self.record_probe(&prefix, "concurrent-cas", a, b)?;
        let ok = |code| matches!(code, StatusCode::OK | StatusCode::NO_CONTENT);
        ensure!(
            (ok(a) && b == StatusCode::PRECONDITION_FAILED)
                || (ok(b) && a == StatusCode::PRECONDITION_FAILED),
            "{}",
            race_error("并发条件更新未产生唯一成功方", a, b)
        );
        let (winner, _) = self.read_at(cas, 32).await?;
        ensure!(
            winner == if ok(a) { b"race-a" } else { b"race-b" },
            "并发更新结果校验失败"
        );

        let a = self.managed_url(&format!("{prefix}move-a"))?;
        let b = self.managed_url(&format!("{prefix}move-b"))?;
        let target = self.managed_url(&format!("{prefix}winner"))?;
        self.put_staging(a.clone(), b"move-a").await?;
        self.put_staging(b.clone(), b"move-b").await?;
        let (ra, rb) = futures_util::future::join(
            self.move_absent(a.clone(), target.clone()),
            self.move_absent(b.clone(), target.clone()),
        )
        .await;
        let (ra, rb) = (ra?, rb?);
        self.record_probe(&prefix, "concurrent-move", ra, rb)?;
        let rejected =
            |code| matches!(code, StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED);
        ensure!(
            (ra == StatusCode::CREATED && rejected(rb))
                || (rb == StatusCode::CREATED && rejected(ra)),
            "{}",
            race_error("禁止覆盖发布未产生唯一成功方", ra, rb)
        );
        let a_won = ra == StatusCode::CREATED;
        let (winner_source, loser_source, win_bytes, lose_bytes) = if a_won {
            (a, b, b"move-a", b"move-b")
        } else {
            (b, a, b"move-b", b"move-a")
        };
        ensure!(
            self.read_at(target.clone(), 32).await?.0 == win_bytes
                && self.read_optional_at(winner_source, 32).await?.is_none()
                && self.read_at(loser_source.clone(), 32).await?.0 == lose_bytes,
            "发布竞争结果校验失败"
        );
        ensure!(
            rejected(
                self.move_absent(loser_source.clone(), target.clone())
                    .await?
            ) && self.read_at(target, 32).await?.0 == win_bytes
                && self.read_at(loser_source, 32).await?.0 == lose_bytes,
            "{}",
            capability_error("服务器未保护已存在的发布目标")
        );
        if let Some(root) = &self.journal_root {
            let proof = serde_json::json!({"endpoint": self.endpoint_id(), "transportProfile": TransportProfile::OpaqueMove,
                "checkedAt": chrono::Utc::now(), "probeVersion": 1, "probe": prefix});
            local::atomic_write(
                root,
                ".qiwo-sync/managed-capability.json",
                &serde_json::to_vec(&proof)?,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_distinguish_double_success_from_inconclusive_http_errors() {
        let rejected = race_error(
            "并发条件更新",
            StatusCode::NO_CONTENT,
            StatusCode::NO_CONTENT,
        )
        .to_string();
        assert!(rejected.contains("204 / HTTP 204") && rejected.contains("重复重试无效"));
        let throttled = race_error(
            "并发条件更新",
            StatusCode::NO_CONTENT,
            StatusCode::TOO_MANY_REQUESTS,
        )
        .to_string();
        assert!(throttled.contains("429") && throttled.contains("检测未完成"));
        assert!(!throttled.contains("重复重试无效"));
    }
}
