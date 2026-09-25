//! Bounded, read-only WebDAV traversal. Keep it separate from the writing client
//! so inventory cannot accidentally create collections or initialize a manifest.
use std::time::Duration;

use anyhow::{Result, anyhow, bail, ensure};
use chrono::{DateTime, Utc};
use quick_xml::{NsReader, events::Event, name::ResolveResult};
use reqwest::{Method, StatusCode, Url};

use super::{FileFacts, MANIFEST, valid_path};

pub(crate) struct Reader {
    client: reqwest::Client,
    root: Url,
    credentials: Option<(String, String)>,
}

pub(crate) struct Entry {
    pub path: String,
    pub is_dir: bool,
    pub facts: FileFacts,
}

pub(crate) struct Listing {
    pub entries: Vec<Entry>,
    pub complete: bool,
    pub is_collection: bool,
}

impl Reader {
    pub fn new(url: &str, username: &str, password: &str) -> Result<Self> {
        let mut root = Url::parse(url).map_err(|_| anyhow!("WebDAV 地址无效"))?;
        ensure!(
            matches!(root.scheme(), "http" | "https")
                && root.host_str().is_some()
                && root.username().is_empty()
                && root.password().is_none()
                && root.query().is_none()
                && root.fragment().is_none(),
            "WebDAV 地址须为 HTTP(S)，不能包含账号、查询参数或片段"
        );
        if !root.path().ends_with('/') {
            root.set_path(&format!("{}/", root.path()));
        }
        decoded_segments(&root)?;
        let client = reqwest::Client::builder()
            .user_agent("QiwoSync/1.0")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| anyhow!("无法创建网络连接"))?;
        Ok(Self {
            client,
            root,
            credentials: (!username.is_empty()).then(|| (username.into(), password.into())),
        })
    }

    fn url(&self, path: &str, directory: bool) -> Result<Url> {
        ensure!(path.is_empty() || valid_path(path), "远端路径无效");
        let mut url = self.root.clone();
        if !path.is_empty() {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow!("远端地址无效"))?;
            segments.pop_if_empty();
            for part in path.split('/') {
                segments.push(part);
            }
            if directory {
                segments.push("");
            }
        }
        Ok(url)
    }

    async fn read(&self, method: Method, url: Url, limit: usize) -> Result<Option<Vec<u8>>> {
        let listing = method.as_str() == "PROPFIND";
        let mut request = self.client.request(method, url);
        if listing {
            request = request.header("Depth", "1").header("Content-Type", "application/xml; charset=utf-8")
                .body(r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#);
        }
        if let Some((user, password)) = &self.credentials {
            request = request.basic_auth(user, Some(password));
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| anyhow!("网络连接失败或超时，请检查地址与网络"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status().is_redirection() {
            bail!("服务器要求跳转，请配置最终的 WebDAV 目录地址");
        }
        ensure!(
            if listing {
                response.status() == StatusCode::MULTI_STATUS
            } else {
                response.status() == StatusCode::OK
            },
            "服务器返回 HTTP {}，请检查地址与账号权限",
            response.status().as_u16()
        );
        ensure!(
            response.content_length().is_none_or(|n| n <= limit as u64),
            "响应超过扫描大小限制"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("读取响应失败或超时"))?
        {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= limit,
                "响应超过扫描大小限制"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
    }

    pub async fn get_manifest(&self) -> Result<Option<Vec<u8>>> {
        self.read(Method::GET, self.url(MANIFEST, false)?, 4 * 1024 * 1024)
            .await
    }

    pub async fn list(&self, dir: &str) -> Result<Option<Listing>> {
        let url = self.url(dir, true)?;
        let bytes = self
            .read(
                Method::from_bytes(b"PROPFIND")?,
                url.clone(),
                8 * 1024 * 1024,
            )
            .await?;
        bytes
            .map(|bytes| parse_listing(&bytes, &self.root, &url))
            .transpose()
    }
}

fn decoded_segments(url: &Url) -> Result<Vec<String>> {
    url.path_segments()
        .ok_or_else(|| anyhow!("远端路径无效"))?
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut decoded = Vec::new();
            let mut chars = part.as_bytes().iter().copied();
            while let Some(c) = chars.next() {
                if c == b'%' {
                    let a = chars.next().and_then(|v| (v as char).to_digit(16));
                    let b = chars.next().and_then(|v| (v as char).to_digit(16));
                    let (Some(a), Some(b)) = (a, b) else {
                        bail!("远端路径编码无效");
                    };
                    decoded.push((a * 16 + b) as u8);
                } else {
                    decoded.push(c);
                }
            }
            let part = String::from_utf8(decoded).map_err(|_| anyhow!("远端路径编码无效"))?;
            ensure!(valid_path(&part) && !part.contains('/'), "远端路径无效");
            Ok(part)
        })
        .collect()
}

fn parse_listing(bytes: &[u8], root: &Url, request: &Url) -> Result<Listing> {
    let tree = xml_tree(bytes)?;
    ensure!(tree.is("multistatus"), "服务器未返回 WebDAV 目录列表");
    let root_parts = decoded_segments(root)?;
    let request_parts = decoded_segments(request)?;
    let mut entries = std::collections::BTreeMap::new();
    // Jianguoyun documents a 750-entry response limit. Until its paging contract
    // is supported, never authorize migration from a possibly truncated page.
    let mut complete = tree.children.iter().filter(|n| n.is("response")).count() < 750;
    let mut saw_response = false;
    let mut saw_self = false;
    let mut is_collection = false;
    for response in tree.children.iter().filter(|n| n.is("response")) {
        saw_response = true;
        let path = (|| -> Result<Option<String>> {
            let href = response
                .child("href")
                .ok_or_else(|| anyhow!("missing href"))?
                .text
                .trim();
            let url = request.join(href)?;
            ensure!(
                url.origin() == root.origin()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "outside root"
            );
            let parts = decoded_segments(&url)?;
            ensure!(parts.starts_with(&root_parts), "outside root");
            if parts == request_parts {
                return Ok(None);
            }
            ensure!(
                parts.starts_with(&request_parts) && parts.len() == request_parts.len() + 1,
                "not a child"
            );
            Ok(Some(parts[root_parts.len()..].join("/")))
        })();
        let path = match path {
            Ok(Some(path)) => path,
            Ok(None) => {
                if saw_self {
                    complete = false;
                }
                saw_self = true;
                is_collection = response
                    .children
                    .iter()
                    .filter(|n| n.is("propstat"))
                    .filter(|n| n.child("status").is_some_and(|s| success(&s.text)))
                    .filter_map(|n| n.child("prop"))
                    .filter_map(|p| p.child("resourcetype"))
                    .any(|p| p.child("collection").is_some());
                let self_readable =
                    response
                        .children
                        .iter()
                        .filter(|n| n.is("propstat"))
                        .any(|n| {
                            n.child("status").is_some_and(|s| success(&s.text))
                                && n.child("prop")
                                    .and_then(|p| p.child("resourcetype"))
                                    .is_some()
                        });
                if !self_readable || response.child("status").is_some_and(|s| !success(&s.text)) {
                    complete = false;
                }
                continue;
            }
            Err(_) => {
                complete = false;
                continue;
            }
        };
        if response.child("status").is_some_and(|s| !success(&s.text)) {
            complete = false;
            continue;
        }
        let props: Vec<_> = response
            .children
            .iter()
            .filter(|n| n.is("propstat"))
            .filter(|n| n.child("status").is_some_and(|s| success(&s.text)))
            .filter_map(|n| n.child("prop"))
            .collect();
        let property = |name| props.iter().find_map(|prop| prop.child(name));
        let Some(resource_type) = property("resourcetype") else {
            complete = false;
            continue;
        };
        let is_dir = resource_type.child("collection").is_some();
        let size = property("getcontentlength").and_then(|p| p.text.trim().parse::<u64>().ok());
        let modified_at = property("getlastmodified").and_then(|p| {
            DateTime::parse_from_rfc2822(p.text.trim())
                .ok()
                .map(|d| d.with_timezone(&Utc))
        });
        let entry = Entry {
            path: path.clone(),
            is_dir,
            facts: FileFacts { size, modified_at },
        };
        if entries.insert(path, entry).is_some() {
            complete = false;
        }
    }
    ensure!(saw_response, "WebDAV 目录响应缺少条目，无法确认目录为空");
    Ok(Listing {
        entries: entries.into_values().collect(),
        complete,
        is_collection,
    })
}

fn success(status: &str) -> bool {
    status
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .is_some_and(|s| (200..300).contains(&s))
}

#[derive(Default)]
struct Node {
    name: String,
    dav: bool,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn is(&self, name: &str) -> bool {
        self.dav && self.name == name
    }
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|n| n.is(name))
    }
}

fn xml_tree(bytes: &[u8]) -> Result<Node> {
    let mut reader = NsReader::from_reader(bytes);
    reader.config_mut().expand_empty_elements = true;
    let mut stack = vec![Node::default()];
    let mut nodes = 0;
    loop {
        let (namespace, event) = reader
            .read_resolved_event()
            .map_err(|_| anyhow!("WebDAV XML 格式无效"))?;
        let dav = matches!(namespace, ResolveResult::Bound(ns) if ns.as_ref() == b"DAV:");
        match event {
            Event::Start(e) => {
                nodes += 1;
                ensure!(
                    nodes <= 150_000 && stack.len() < 64,
                    "WebDAV XML 超出扫描限制"
                );
                let node = Node {
                    name: String::from_utf8_lossy(e.local_name().as_ref()).into(),
                    dav,
                    ..Default::default()
                };
                stack.push(node);
            }
            Event::End(_) => {
                ensure!(stack.len() > 1, "WebDAV XML 格式无效");
                let node = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(node);
            }
            Event::Text(e) => stack
                .last_mut()
                .unwrap()
                .text
                .push_str(&e.decode().map_err(|_| anyhow!("XML 编码无效"))?),
            Event::CData(e) => stack
                .last_mut()
                .unwrap()
                .text
                .push_str(&e.decode().map_err(|_| anyhow!("XML 编码无效"))?),
            Event::GeneralRef(e) => {
                if let Some(c) = e
                    .resolve_char_ref()
                    .map_err(|_| anyhow!("XML 字符引用无效"))?
                {
                    stack.last_mut().unwrap().text.push(c);
                } else {
                    let text = match e.as_ref() {
                        b"amp" => "&",
                        b"lt" => "<",
                        b"gt" => ">",
                        b"quot" => "\"",
                        b"apos" => "'",
                        _ => bail!("XML 实体不受支持"),
                    };
                    stack.last_mut().unwrap().text.push_str(text);
                }
            }
            Event::DocType(_) => bail!("WebDAV XML 不允许包含 DTD"),
            Event::Eof => break,
            _ => (),
        }
    }
    ensure!(
        stack.len() == 1 && stack[0].children.len() == 1,
        "WebDAV XML 不完整"
    );
    Ok(stack.pop().unwrap().children.pop().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> Result<Listing> {
        let root = Url::parse("https://example.test/dav/").unwrap();
        let xml = format!(r#"<multistatus xmlns="DAV:">{body}</multistatus>"#);
        parse_listing(xml.as_bytes(), &root, &root)
    }

    fn item(href: &str, props: &str) -> String {
        format!(
            "<response><href>{href}</href><propstat><prop>{props}</prop><status>HTTP/1.1 200 OK</status></propstat></response>"
        )
    }

    #[test]
    fn parses_namespaces_encoded_paths_entities_and_optional_properties() {
        let list = parse(&item("/dav/%E4%B8%AD%E6%96%87%20%25%23&amp;.custom.yaml", "<resourcetype/><getcontentlength>12</getcontentlength><getlastmodified>Wed, 21 Oct 2015 07:28:00 GMT</getlastmodified>")).unwrap();
        assert!(list.complete);
        assert_eq!(list.entries[0].path, "中文 %#&.custom.yaml");
        assert_eq!(list.entries[0].facts.size, Some(12));
        assert!(list.entries[0].facts.modified_at.is_some());
        let list = parse(&item(
            "file.txt",
            "<resourcetype/><getcontentlength>n/a</getcontentlength>",
        ))
        .unwrap();
        assert_eq!(list.entries[0].facts.size, None);
    }

    #[test]
    fn rejects_outside_and_non_child_hrefs_without_following_them() {
        for href in [
            "https://evil.test/dav/x",
            "/dav-other/x",
            "/dav/%2fetc",
            "/dav/%5cetc",
            "/dav/x/y",
            "/dav/../secret",
            "/dav/x?token=secret",
            "/dav/%FF",
        ] {
            let list = parse(&item(href, "<resourcetype/>")).unwrap();
            assert!(!list.complete, "{href}");
            assert!(list.entries.is_empty(), "{href}");
        }
    }

    #[test]
    fn failed_properties_do_not_become_zero_byte_files_or_empty_directories() {
        let list = parse("<response><href>/dav/a</href><propstat><prop><resourcetype/><getcontentlength>999</getcontentlength></prop><status>HTTP/1.1 403 Forbidden</status></propstat></response>").unwrap();
        assert!(!list.complete);
        assert!(list.entries.is_empty());
        let list =
            parse("<response><href>/dav/</href><status>HTTP/1.1 403 Forbidden</status></response>")
                .unwrap();
        assert!(!list.complete);
    }

    #[test]
    fn possible_provider_page_boundary_is_never_complete() {
        let entries: String = (0..750)
            .map(|i| item(&format!("/dav/{i}.txt"), "<resourcetype/>"))
            .collect();
        assert!(!parse(&entries).unwrap().complete);
        let below: String = (0..749)
            .map(|i| item(&format!("/dav/{i}.txt"), "<resourcetype/>"))
            .collect();
        assert!(parse(&below).unwrap().complete);
    }

    #[test]
    fn malformed_xml_dtd_and_fake_webdav_responses_are_errors() {
        let root = Url::parse("https://example.test/dav/").unwrap();
        for xml in [
            "<multistatus xmlns='DAV:'>",
            "<html/>",
            "<multistatus xmlns='DAV:'/>",
            "<!DOCTYPE x><multistatus xmlns='DAV:'/>",
        ] {
            assert!(parse_listing(xml.as_bytes(), &root, &root).is_err());
        }
    }
}
