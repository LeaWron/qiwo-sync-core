//! The management store is disjoint from all v1 paths. Initialization and writes
//! require validated server capabilities; physical DELETE is never used.
use super::{ManagedState, Plan, TransportProfile};
use anyhow::{Result, anyhow, bail, ensure};
use chrono::Utc;
use reqwest::{Client, StatusCode, Url};
use std::time::Duration;

mod compatible;
mod objects;
mod validator;
pub use objects::{MAX_OBJECT_BYTES, reference};
pub use validator::Validator;

pub const MAX_STATE_BYTES: usize = 4 * 1024 * 1024;

pub struct Store {
    journal_root: Option<std::path::PathBuf>,
    client: Client,
    url: Url,
    credentials: Option<(String, String)>,
}
impl Store {
    pub fn new(sync_root: &str, username: &str, password: &str) -> Result<Self> {
        let mut url = Url::parse(sync_root).map_err(|_| anyhow!("管理目录地址无效"))?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "管理目录地址无效"
        );
        url.path_segments_mut()
            .map_err(|_| anyhow!("管理目录地址无效"))?
            .pop_if_empty()
            .push(".qiwo-managed-v2")
            .push("state.json");
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            journal_root: None,
            client,
            url,
            credentials: (!username.is_empty()).then(|| (username.into(), password.into())),
        })
    }
    pub fn with_journal(mut self, root: &std::path::Path) -> Self {
        self.journal_root = Some(root.to_owned());
        self
    }
    /// This is only a preview proposal. Execution must prove every capability.
    pub fn proposed_profile(&self) -> TransportProfile {
        if self.url.host_str() == Some("dav.jianguoyun.com") {
            TransportProfile::OpaqueMove
        } else {
            TransportProfile::Standard
        }
    }
    /// Empirical counterexample: identical validators were both accepted (204/204).
    /// A later successful sample cannot certify this endpoint's atomicity.
    pub fn management_block_reason(&self) -> Option<&'static str> {
        (self.url.host_str() == Some("dav.jianguoyun.com")).then_some(
            "当前坚果云接口曾同时接受两个并发条件写入，管理迁移和清理已暂停。请关闭预览；普通同步和只读查看仍可使用。"
        )
    }
    pub fn ensure_management_writable(&self) -> Result<()> {
        if let Some(reason) = self.management_block_reason() {
            bail!("{reason}");
        }
        Ok(())
    }
    pub fn endpoint_id(&self) -> String {
        super::hash(&(
            self.url.as_str(),
            self.credentials
                .as_ref()
                .map(|(user, _)| user.as_str())
                .unwrap_or(""),
        ))
        .expect("serializable endpoint")
    }
    fn auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some((user, password)) = &self.credentials {
            request.basic_auth(user, Some(password))
        } else {
            request
        }
    }
    pub async fn load(&self) -> Result<(ManagedState, Validator)> {
        self.load_optional()
            .await?
            .ok_or_else(|| anyhow!("尚未初始化管理空间，不能执行清理"))
    }

    pub async fn load_optional(&self) -> Result<Option<(ManagedState, Validator)>> {
        let mut response = self
            .auth(self.client.get(self.url.clone()))
            .send()
            .await
            .map_err(|_| anyhow!("读取管理状态失败"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status() == StatusCode::OK,
            "管理状态读取失败：HTTP {}",
            response.status().as_u16()
        );
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("管理状态读取中断"))?
        {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= MAX_STATE_BYTES,
                "管理状态超出大小限制"
            );
            bytes.extend_from_slice(&chunk);
        }
        let state: ManagedState =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("管理状态损坏或版本不支持"))?;
        state.validate()?;
        let validator = Validator::parse(state.profile(), &etag)?;
        Ok(Some((state, validator)))
    }
    /// Publish one all-or-nothing state transition. After an ambiguous response,
    /// retry this exact plan: its persistent receipt makes retries idempotent.
    pub async fn commit(&self, plan: &Plan) -> Result<ManagedState> {
        self.ensure_management_writable()?;
        let (current, etag) = self.load().await?;
        let next = current.apply(plan, Utc::now())?;
        if next.revision == current.revision {
            return Ok(current);
        }
        self.replace(&next, &etag).await?;
        Ok(next)
    }

    /// Caller must derive `next` from the state loaded with this exact ETag.
    /// Used by both lifecycle and transfer writers; never falls back to an unconditional PUT.
    pub(crate) async fn replace(&self, next: &ManagedState, etag: &Validator) -> Result<()> {
        self.ensure_management_writable()?;
        ensure!(etag.profile == next.profile(), "条件标记与传输配置不一致");
        next.validate()?;
        let bytes = serde_json::to_vec(next)?;
        ensure!(
            bytes.len() <= MAX_STATE_BYTES,
            "管理记录空间已满，请先升级协议容量"
        );
        let response = self
            .auth(self.client.put(self.url.clone()))
            .header(reqwest::header::IF_MATCH, &etag.value)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(|_| anyhow!("提交结果尚未确认，请使用同一操作 ID 重试"))?;
        if response.status() == StatusCode::PRECONDITION_FAILED {
            bail!("其他设备已更新数据，请刷新后重新预览");
        }
        ensure!(
            matches!(response.status(), StatusCode::OK | StatusCode::NO_CONTENT),
            "提交结果尚未确认（HTTP {}），请使用同一操作 ID 重试",
            response.status().as_u16()
        );
        Ok(())
    }
}

fn strong_etag(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0] == b'"'
        && bytes[bytes.len() - 1] == b'"'
        && bytes[1..bytes.len() - 1]
            .iter()
            .all(|b| *b == 0x21 || (0x23..=0x7e).contains(b) || *b >= 0x80)
}

fn capability_error(reason: &str) -> anyhow::Error {
    anyhow!("{reason}。当前服务器暂不兼容安全管理，重复重试无效；请关闭预览并检查服务器兼容性。")
}

fn require_strong_etag(value: &str) -> Result<()> {
    if strong_etag(value) {
        return Ok(());
    }
    let reason = if value.is_empty() {
        "服务器未返回 ETag"
    } else if value.starts_with("W/") {
        "服务器仅返回弱 ETag，不能用于防止并发覆盖"
    } else {
        "服务器返回的 ETag 格式不符合标准（强 ETag 必须带双引号）"
    };
    Err(capability_error(reason))
}

#[cfg(test)]
mod quarantine_tests {
    use super::*;

    #[tokio::test]
    async fn known_non_atomic_endpoint_is_blocked_before_network_or_publication() {
        let mut store = Store::new("https://dav.jianguoyun.com/dav/fixture", "", "").unwrap();
        // Any accidental network path fails immediately through this dead proxy.
        store.client = Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
            .build()
            .unwrap();
        assert!(store.management_block_reason().is_some());
        for profile in [TransportProfile::Standard, TransportProfile::OpaqueMove] {
            let error = store
                .prepare_profile(profile)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("管理迁移和清理已暂停"), "{error}");
            let error = store
                .put_object_for(b"fixture".to_vec(), profile)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("管理迁移和清理已暂停"), "{error}");
        }
        let other = Store::new("http://127.0.0.1:1/dav/fixture", "", "").unwrap();
        assert!(other.ensure_management_writable().is_ok());
    }
}
