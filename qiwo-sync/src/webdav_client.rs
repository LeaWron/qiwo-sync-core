use std::path::Path;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::StatusCode;
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub struct WebDavClient {
    client: reqwest::Client,
    base_url: String,
}

impl WebDavClient {
    pub fn new(base_url: &str, username: Option<&str>, password: Option<&str>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        if let (Some(u), Some(p)) = (username, password) {
            let auth = format!(
                "Basic {}",
                base64_encode(format!("{}:{}", u, p).as_bytes())
            );
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&auth)?);
        }

        // Validate base URL
        let base = base_url.trim_end_matches('/').to_string();
        reqwest::Url::parse(&base).context("Invalid remote URL")?;

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent("QiwoSync/1.0")
            .build()?;

        Ok(Self {
            client,
            base_url: format!("{}/", base),
        })
    }

    pub async fn ensure_root(&self) -> Result<()> {
        self.mkcol("").await
    }

    /// Upload a local file.
    pub async fn put_file(&self, relative_path: &str, local_path: &Path) -> Result<()> {
        if let Some(parent) = Path::new(relative_path).parent() {
            if let Some(parent_str) = parent.to_str() {
                if !parent_str.is_empty() && parent_str != "." {
                    self.ensure_collection(parent_str).await?;
                }
            }
        }

        let url = self.build_url(relative_path);
        let data = fs::read(local_path).await?;
        let resp = self.client.put(&url).body(data).send().await?;

        ensure_success(resp, &url)
    }

    /// Upload bytes.
    pub async fn put_bytes(&self, relative_path: &str, bytes: Vec<u8>) -> Result<()> {
        let url = self.build_url(relative_path);
        let resp = self.client.put(&url).body(bytes).send().await?;

        ensure_success(resp, &url)
    }

    /// Download bytes. Returns None if 404.
    pub async fn get_bytes(&self, relative_path: &str) -> Result<Option<Vec<u8>>> {
        let url = self.build_url(relative_path);
        let resp = self.client.get(&url).send().await?;

        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {} failed: HTTP {} {}", url, status.as_u16(), body);
        }

        Ok(Some(resp.bytes().await?.to_vec()))
    }

    /// Download a remote file to local path.
    pub async fn download_file(&self, relative_path: &str, target: &Path) -> Result<()> {
        let url = self.build_url(relative_path);
        let resp = self.client.get(&url).send().await?;

        if !resp.status().is_success() {
            anyhow::bail!("GET {} failed: HTTP {}", url, resp.status().as_u16());
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).await?;
        }

        let data = resp.bytes().await?;
        let mut file = fs::File::create(target).await?;
        file.write_all(&data).await?;
        Ok(())
    }

    // ---- internal helpers ----

    fn build_url(&self, relative_path: &str) -> String {
        let path = normalize_path(relative_path);
        let encoded = path
            .split('/')
            .map(|seg| urlencoding(seg))
            .collect::<Vec<_>>()
            .join("/");
        format!("{}{}", self.base_url, encoded)
    }

    async fn ensure_collection(&self, relative_path: &str) -> Result<()> {
        let normalized = normalize_path(relative_path);
        let segments: Vec<&str> = normalized
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let mut current = String::new();
        for seg in &segments {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(seg);
            self.mkcol(&current).await?;
        }

        if segments.is_empty() {
            self.mkcol("").await?;
        }

        Ok(())
    }

    async fn mkcol(&self, relative_path: &str) -> Result<()> {
        let url = if relative_path.is_empty() {
            self.base_url.clone()
        } else {
            self.build_url(relative_path)
        };

        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"MKCOL").unwrap(), &url)
            .send()
            .await?;

        let status_code = resp.status();
        match status_code {
            StatusCode::CREATED | StatusCode::OK => Ok(()),
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::CONFLICT => {
                Ok(())
            }
            _ => {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("MKCOL {} failed: HTTP {} {}", url, status_code.as_u16(), body)
            }
        }
    }
}

fn ensure_success(resp: reqwest::Response, url: &str) -> Result<()> {
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        anyhow::bail!("Request to {} failed: HTTP {}", url, status.as_u16())
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_string()
}

fn urlencoding(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}
