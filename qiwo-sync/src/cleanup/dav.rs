use anyhow::{Result, anyhow, ensure};
use reqwest::{Client, Method, StatusCode, Url};
use std::time::Duration;

pub(super) struct Read {
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
}
pub(super) struct Dav {
    client: Client,
    root: Url,
    username: String,
    password: String,
}
impl Dav {
    pub fn new(request: &crate::types::SyncRequest) -> Result<Self> {
        let url = request
            .remote_url
            .as_deref()
            .ok_or_else(|| anyhow!("缺少同步地址"))?;
        let mut root = Url::parse(url)?;
        ensure!(
            matches!(root.scheme(), "http" | "https")
                && root.host_str().is_some()
                && root.username().is_empty()
                && root.password().is_none()
                && root.query().is_none()
                && root.fragment().is_none(),
            "同步地址无效"
        );
        root.path_segments_mut()
            .map_err(|_| anyhow!("同步地址无效"))?
            .pop_if_empty()
            .push("");
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(20))
                .build()?,
            root,
            username: request.username.clone().unwrap_or_default(),
            password: request.password.clone().unwrap_or_default(),
        })
    }
    pub fn endpoint(&self) -> String {
        super::reference(format!("{}\n{}", self.root, self.username).as_bytes()).sha256
    }
    fn request(&self, method: Method, path: &str) -> Result<reqwest::RequestBuilder> {
        ensure!(
            crate::inventory::valid_path(path.trim_end_matches('/')),
            "清理路径无效"
        );
        let mut url = self.root.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow!("同步地址无效"))?;
            segments.pop_if_empty();
            for part in path.split('/') {
                segments.push(part);
            }
        }
        let mut request = self.client.request(method, url);
        if !self.username.is_empty() {
            request = request.basic_auth(&self.username, Some(&self.password));
        }
        Ok(request)
    }
    pub async fn get(&self, path: &str, limit: usize) -> Result<Option<Read>> {
        let mut response = self
            .request(Method::GET, path)?
            .send()
            .await
            .map_err(|_| anyhow!("读取清理对象失败，请检查网络"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status() == StatusCode::OK,
            "读取清理对象失败：HTTP {}",
            response.status().as_u16()
        );
        ensure!(
            response.content_length().is_none_or(|n| n <= limit as u64),
            "清理对象超过大小限制"
        );
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty() && !v.starts_with("W/"))
            .map(str::to_owned);
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("清理对象读取中断"))?
        {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= limit,
                "清理对象超过大小限制"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(Read { bytes, etag }))
    }
    // Conditions are defense in depth, not proof of server-side atomicity.
    // This maintenance flow explicitly requires all other writers to be paused.
    pub async fn manifest(&self, bytes: Vec<u8>, old: &Read) -> Result<()> {
        let mut request = self.request(Method::PUT, super::REMOTE_MANIFEST)?;
        if let Some(tag) = &old.etag {
            request = request.header(reqwest::header::IF_MATCH, tag);
        }
        request
            .body(bytes.clone())
            .send()
            .await
            .map_err(|_| anyhow!("清单提交结果未确认，请重新扫描；备份已保留"))?
            .error_for_status()
            .map_err(|_| anyhow!("清单更新失败或已变化，请重新扫描"))?;
        ensure!(
            self.get(super::REMOTE_MANIFEST, super::MAX_MANIFEST)
                .await?
                .is_some_and(|r| r.bytes == bytes),
            "清单更新后不一致，请暂停其他设备同步并重新扫描"
        );
        Ok(())
    }
    pub async fn delete_file(&self, path: &str, actor: &str, old: &Read) -> Result<()> {
        ensure!(super::allowed(path, actor), "只能删除其他设备的具体文件");
        let mut request = self.request(Method::DELETE, path)?;
        if let Some(tag) = &old.etag {
            request = request.header(reqwest::header::IF_MATCH, tag);
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow!("删除结果未确认，请重新扫描；备份已保留"))?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
            ),
            "删除失败或文件已变化：HTTP {}",
            response.status().as_u16()
        );
        ensure!(
            self.get(path, super::MAX_FILE).await?.is_none(),
            "文件删除后再次出现，请暂停其他设备同步并重新扫描"
        );
        Ok(())
    }

    // The caller has just obtained a complete Depth:1 listing with no children.
    // DAV collection DELETE is recursive; all other writers MUST stay paused
    // through this request (a directory ETag cannot make the emptiness check atomic).
    pub async fn delete_empty_directory(&self, path: &str, actor: &str) -> Result<()> {
        ensure!(
            super::allowed(&format!("{path}/_"), actor),
            "只能删除其他设备的空目录"
        );
        let response = self
            .request(Method::DELETE, &format!("{path}/"))?
            .send()
            .await
            .map_err(|_| anyhow!("目录删除结果未确认，请重新扫描"))?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
            ),
            "目录删除失败：HTTP {}",
            response.status().as_u16()
        );
        Ok(())
    }
}
