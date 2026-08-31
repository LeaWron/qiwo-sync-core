use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Method;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(60);

pub struct WebDavClient {
    client: reqwest::Client,
    base_url: String,
    known_collections: Arc<Mutex<HashSet<String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectionProbe {
    Exists,
    Missing,
}

impl WebDavClient {
    pub fn new(base_url: &str, username: Option<&str>, password: Option<&str>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        if let (Some(u), Some(p)) = (username, password) {
            let auth = format!("Basic {}", base64_encode(format!("{}:{}", u, p).as_bytes()));
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&auth)?);
        }

        // Validate base URL
        let base = base_url.trim_end_matches('/').to_string();
        reqwest::Url::parse(&base).context("Invalid remote URL")?;

        // Without these a WebDAV host that accepts the connection and then stops
        // talking wedges the whole sync forever. Values match the Android Kotlin
        // client (WebDavClient.kt) so both frontends give up at the same point.
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent("QiwoSync/1.0")
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()?;

        Ok(Self {
            client,
            base_url: format!("{}/", base),
            known_collections: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    pub async fn ensure_root(&self) -> Result<()> {
        self.ensure_collection("").await
    }

    /// Upload a local file.
    pub async fn put_file(&self, relative_path: &str, local_path: &Path) -> Result<()> {
        if let Some(parent) = Path::new(relative_path).parent()
            && let Some(parent_str) = parent.to_str()
            && !parent_str.is_empty()
            && parent_str != "."
        {
            self.ensure_collection(parent_str).await?;
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
    ///
    /// The body lands in a sibling temp file that is renamed into place only
    /// after it is fully written and flushed. A dropped connection mid-download
    /// would otherwise leave a truncated `default.custom.yaml` — or a truncated
    /// user dictionary snapshot — in the live Rime directory.
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
        let staging = staging_path(target);

        match write_then_rename(&staging, target, &data).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&staging).await;
                Err(e).with_context(|| format!("writing {}", target.display()))
            }
        }
    }

    // ---- internal helpers ----

    fn build_url(&self, relative_path: &str) -> String {
        let path = normalize_path(relative_path);
        let encoded = path
            .split('/')
            .map(urlencoding)
            .collect::<Vec<_>>()
            .join("/");
        format!("{}{}", self.base_url, encoded)
    }

    async fn ensure_collection(&self, relative_path: &str) -> Result<()> {
        let normalized = normalize_path(relative_path);
        let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();

        if segments.is_empty() {
            self.ensure_collection_exists("").await?;
            return Ok(());
        }

        let mut current = String::new();
        for seg in &segments {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(seg);
            self.ensure_collection_exists(&current).await?;
        }

        Ok(())
    }

    async fn ensure_collection_exists(&self, relative_path: &str) -> Result<()> {
        let normalized = normalize_path(relative_path);
        {
            let known = self.known_collections.lock().await;
            if known.contains(&normalized) {
                return Ok(());
            }
        }

        match self.probe_collection(&normalized).await? {
            CollectionProbe::Exists => {}
            CollectionProbe::Missing => self.mkcol(&normalized).await?,
        }

        self.known_collections.lock().await.insert(normalized);
        Ok(())
    }

    async fn probe_collection(&self, relative_path: &str) -> Result<CollectionProbe> {
        let url = if relative_path.is_empty() {
            self.base_url.clone()
        } else {
            self.build_url(relative_path)
        };

        let resp = self
            .client
            .request(Method::from_bytes(b"PROPFIND").unwrap(), &url)
            .header("Depth", "0")
            .send()
            .await?;

        let status_code = resp.status();
        match status_code {
            StatusCode::OK | StatusCode::MULTI_STATUS => Ok(CollectionProbe::Exists),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::CONFLICT => {
                Ok(CollectionProbe::Missing)
            }
            _ => {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!(
                    "PROPFIND {} failed: HTTP {} {}",
                    url,
                    status_code.as_u16(),
                    body
                )
            }
        }
    }

    async fn mkcol(&self, relative_path: &str) -> Result<()> {
        let url = if relative_path.is_empty() {
            self.base_url.clone()
        } else {
            self.build_url(relative_path)
        };

        let resp = self
            .client
            .request(Method::from_bytes(b"MKCOL").unwrap(), &url)
            .send()
            .await?;

        let status_code = resp.status();
        match status_code {
            StatusCode::CREATED | StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::CONFLICT => Ok(()),
            _ => {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!(
                    "MKCOL {} failed: HTTP {} {}",
                    url,
                    status_code.as_u16(),
                    body
                )
            }
        }
    }
}

/// Sibling staging path used by [`WebDavClient::download_file`].
///
/// Kept next to the target so the rename stays on one filesystem; the leading
/// dot keeps it out of Rime's way if a crash leaves one behind, and the suffix
/// is excluded by [`crate::file_selector::FileSelector`].
fn staging_path(target: &Path) -> std::path::PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());

    match target.parent() {
        Some(parent) => parent.join(format!(".{name}.qiwo-part")),
        None => std::path::PathBuf::from(format!(".{name}.qiwo-part")),
    }
}

async fn write_then_rename(staging: &Path, target: &Path, data: &[u8]) -> Result<()> {
    let mut file = fs::File::create(staging).await?;
    file.write_all(data).await?;
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    fs::rename(staging, target).await?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::thread;

    #[derive(Debug)]
    struct RecordedRequest {
        method: String,
        path: String,
    }

    // Current-thread on purpose: the library must not need `rt-multi-thread`, and
    // a workspace build would otherwise hide that via feature unification.
    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn start_server<F>(
        expected_requests: usize,
        responder: F,
    ) -> (
        String,
        Arc<StdMutex<Vec<RecordedRequest>>>,
        thread::JoinHandle<()>,
    )
    where
        F: Fn(&RecordedRequest) -> u16 + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base_url = format!("http://{}/dav", address);
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let responder = Arc::new(responder);

        let handle = thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).unwrap();
                let request_text = String::from_utf8_lossy(&buffer[..read]);
                let request_line = request_text.lines().next().unwrap_or_default();
                let mut parts = request_line.split_whitespace();
                let request = RecordedRequest {
                    method: parts.next().unwrap_or_default().to_string(),
                    path: parts.next().unwrap_or_default().to_string(),
                };
                let status = responder(&request);
                recorded.lock().unwrap().push(request);

                let reason = match status {
                    200 => "OK",
                    201 => "Created",
                    204 => "No Content",
                    207 => "Multi-Status",
                    404 => "Not Found",
                    405 => "Method Not Allowed",
                    409 => "Conflict",
                    _ => "Error",
                };
                let body = "";
                write!(
                    stream,
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    reason,
                    body.len(),
                    body
                )
                .unwrap();
            }
        });

        (base_url, requests, handle)
    }

    #[test]
    fn ensure_root_probes_existing_collection_once() {
        let (base_url, requests, server) = start_server(1, |request| {
            assert_eq!(request.method, "PROPFIND");
            207
        });
        let client = WebDavClient::new(&base_url, None, None).unwrap();

        block_on(async {
            client.ensure_root().await.unwrap();
            client.ensure_root().await.unwrap();
        });
        server.join().unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "PROPFIND");
        assert_eq!(requests[0].path, "/dav/");
    }

    #[test]
    fn ensure_collection_creates_missing_segments_once() {
        let (base_url, requests, server) =
            start_server(4, |request| match request.method.as_str() {
                "PROPFIND" => 404,
                "MKCOL" => 201,
                _ => 500,
            });
        let client = WebDavClient::new(&base_url, None, None).unwrap();

        block_on(async {
            client.ensure_collection("sync/android").await.unwrap();
            client.ensure_collection("sync/android").await.unwrap();
        });
        server.join().unwrap();

        let requests = requests.lock().unwrap();
        let observed = requests
            .iter()
            .map(|request| (request.method.as_str(), request.path.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                ("PROPFIND", "/dav/sync"),
                ("MKCOL", "/dav/sync"),
                ("PROPFIND", "/dav/sync/android"),
                ("MKCOL", "/dav/sync/android"),
            ]
        );
    }
}
