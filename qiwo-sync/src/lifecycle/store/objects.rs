//! Immutable objects and explicit capability-checked initialization.
use super::{MAX_STATE_BYTES, Store, capability_error, require_strong_etag};
use crate::lifecycle::{ManagedState, ObjectRef, TransportProfile, check_files, managed_path};
use anyhow::{Result, anyhow, ensure};
use reqwest::{Method, StatusCode, Url};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;

pub fn reference(bytes: &[u8]) -> ObjectRef {
    ObjectRef {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        size: bytes.len() as u64,
    }
}

impl Store {
    pub(super) fn managed_url(&self, path: &str) -> Result<Url> {
        Ok(self.url.join(path)?)
    }
    pub(super) async fn read_at(&self, url: Url, limit: usize) -> Result<(Vec<u8>, String)> {
        self.read_optional_at(url, limit)
            .await?
            .ok_or_else(|| anyhow!("对象不存在"))
    }
    pub(super) async fn read_optional_at(
        &self,
        url: Url,
        limit: usize,
    ) -> Result<Option<(Vec<u8>, String)>> {
        let mut response = self
            .auth(self.client.get(url))
            .send()
            .await
            .map_err(|_| anyhow!("读取对象失败"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status() == StatusCode::OK,
            "对象读取失败：HTTP {}",
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
            .map_err(|_| anyhow!("对象读取中断"))?
        {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= limit,
                "对象超过大小限制"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some((bytes, etag)))
    }
    pub async fn read_object(&self, object: &ObjectRef) -> Result<Vec<u8>> {
        check_files(&BTreeMap::from([(
            "custom_phrase.txt".into(),
            object.clone(),
        )]))?;
        ensure!(object.size <= MAX_OBJECT_BYTES as u64, "对象超过大小限制");
        let (bytes, _) = self
            .read_at(
                self.managed_url(&format!("objects/{}", object.sha256))?,
                object.size as usize,
            )
            .await?;
        ensure!(reference(&bytes) == *object, "对象哈希或大小校验失败");
        Ok(bytes)
    }
    pub async fn put_object(&self, bytes: Vec<u8>) -> Result<ObjectRef> {
        self.put_object_for(bytes, TransportProfile::Standard).await
    }
    pub async fn put_object_for(
        &self,
        bytes: Vec<u8>,
        profile: TransportProfile,
    ) -> Result<ObjectRef> {
        self.ensure_management_writable()?;
        ensure!(bytes.len() <= MAX_OBJECT_BYTES, "对象超过大小限制");
        let object = reference(&bytes);
        if profile == TransportProfile::OpaqueMove {
            self.publish_absent(&format!("objects/{}", object.sha256), &bytes)
                .await?;
            self.read_object(&object).await?;
            return Ok(object);
        }
        let response = self
            .auth(
                self.client
                    .put(self.managed_url(&format!("objects/{}", object.sha256))?),
            )
            .header(reqwest::header::IF_NONE_MATCH, "*")
            .body(bytes)
            .send()
            .await
            .map_err(|_| anyhow!("上传对象结果未知，可重新预览后重试"))?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::CREATED
                    | StatusCode::OK
                    | StatusCode::NO_CONTENT
                    | StatusCode::PRECONDITION_FAILED
            ),
            "上传对象失败：HTTP {}",
            response.status().as_u16()
        );
        // Also validate pre-existing objects. Never overwrite a corrupt hash path.
        self.read_object(&object).await?;
        Ok(object)
    }
    pub async fn read_legacy(&self, path: &str) -> Result<Vec<u8>> {
        ensure!(managed_path(path), "旧文件路径不允许迁移");
        let mut url = self.url.clone();
        let mut segments = url.path_segments_mut().map_err(|_| anyhow!("路径无效"))?;
        segments.pop().pop();
        for segment in path.split('/') {
            segments.push(segment);
        }
        drop(segments);
        self.read_at(url, MAX_OBJECT_BYTES).await.map(|v| v.0)
    }
    pub(super) async fn collection(&self, path: &str) -> Result<()> {
        let response = self
            .auth(
                self.client
                    .request(Method::from_bytes(b"MKCOL")?, self.managed_url(path)?),
            )
            .send()
            .await
            .map_err(|_| anyhow!("创建管理目录失败"))?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::CREATED | StatusCode::METHOD_NOT_ALLOWED
            ),
            "管理目录不可用：HTTP {}",
            response.status().as_u16()
        );
        Ok(())
    }
    /// Only explicit migration calls this. Probe files are retained for diagnosis;
    /// there is deliberately no DELETE or physical purge in the management API.
    pub async fn prepare_namespace(&self, probe_id: &str) -> Result<()> {
        ensure!(super::super::identifier(probe_id), "探测 ID 无效");
        self.collection("./").await?;
        self.collection("objects/").await?;
        self.collection("probes/").await?;
        let url = self.managed_url(&format!("probes/{probe_id}"))?;
        let send = |body: &'static str, header, value: String| {
            self.auth(self.client.put(url.clone()))
                .header(header, value)
                .body(body)
                .send()
        };
        let response = send("initial", reqwest::header::IF_NONE_MATCH, "*".into()).await?;
        ensure!(
            response.status() == StatusCode::CREATED,
            "服务器未确认独占创建探测文件"
        );
        let (bytes, first) = self.read_at(url.clone(), 32).await?;
        ensure!(
            bytes == b"initial",
            "探测文件内容校验失败，请关闭预览并检查服务器返回内容"
        );
        let response = send("overwrite", reqwest::header::IF_NONE_MATCH, "*".into()).await?;
        if response.status() != StatusCode::PRECONDITION_FAILED {
            if response.status().is_success() {
                return Err(capability_error(
                    "服务器未拒绝已存在文件的独占创建请求（If-None-Match）",
                ));
            }
            anyhow::bail!("独占创建检测未完成：HTTP {}", response.status().as_u16());
        }
        require_strong_etag(&first)?;
        let response = send("updated", reqwest::header::IF_MATCH, first.clone()).await?;
        ensure!(
            matches!(response.status(), StatusCode::OK | StatusCode::NO_CONTENT),
            "服务器不支持条件更新"
        );
        let (bytes, second) = self.read_at(url.clone(), 32).await?;
        require_strong_etag(&second)?;
        if bytes != b"updated" || first == second {
            return Err(capability_error("服务器未正确更新探测文件或 ETag"));
        }
        let response = send("stale", reqwest::header::IF_MATCH, first).await?;
        if response.status() != StatusCode::PRECONDITION_FAILED {
            if response.status().is_success() {
                return Err(capability_error("服务器未拒绝过期写入条件（If-Match）"));
            }
            anyhow::bail!("过期写入检测未完成：HTTP {}", response.status().as_u16());
        }
        let (bytes, final_etag) = self.read_at(url, 32).await?;
        ensure!(
            bytes == b"updated" && final_etag == second,
            "服务器错误地修改了冲突文件"
        );
        Ok(())
    }
    pub async fn prepare_profile(&self, profile: TransportProfile) -> Result<()> {
        self.ensure_management_writable()?;
        match profile {
            TransportProfile::Standard => self.prepare_namespace(&super::super::random_id()?).await,
            TransportProfile::OpaqueMove => self.prepare_compatible().await,
        }
    }
    /// All references must have been uploaded and verified before publication.
    pub(crate) async fn initialize(&self, state: &ManagedState) -> Result<()> {
        self.ensure_management_writable()?;
        state.validate()?;
        let bytes = serde_json::to_vec(state)?;
        ensure!(bytes.len() <= MAX_STATE_BYTES, "管理状态超过大小限制");
        if state.profile() == TransportProfile::OpaqueMove {
            // A competing/previous initialization is reconciled by its receipt below.
            self.publish_absent("state.json", &bytes).await?;
            return Ok(());
        }
        let response = self
            .auth(self.client.put(self.url.clone()))
            .header(reqwest::header::IF_NONE_MATCH, "*")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(|_| anyhow!("初始化结果未知，请使用同一迁移计划重试"))?;
        ensure!(
            response.status() == StatusCode::CREATED,
            "初始化未确认或空间已存在，请使用同一迁移计划核对结果"
        );
        Ok(())
    }
}
