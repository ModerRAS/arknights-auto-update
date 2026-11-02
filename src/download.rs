use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use futures::StreamExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio_util::sync::CancellationToken;

use crate::util::format_bytes;
use crate::version::{check_filename_version, FilenameCheck};

pub const OFFICIAL_APK_PAGE: &str = "https://ak.hypergryph.com/downloads/android_lastest";
pub const BACKUP_LINKS: &str =
    "https://raw.githubusercontent.com/SkyBird233/gryph-links/main/links/arknights";
/// 设备侧安装前至少保留的空间。
pub const REQUIRED_FREE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MIN_APK_BYTES: u64 = 1024 * 1024;
const HTML_CAP: usize = 2 * 1024 * 1024;
const FLUSH_EVERY: u64 = 8 * 1024 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct ApkArtifact {
    pub path: PathBuf,
    pub bytes: u64,
    pub version: String,
    pub filename: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheMeta {
    version: String,
    filename: String,
    size: u64,
    url: String,
}

fn apk_link_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"https?://[^\s"'<>]+?\.apk(?:\?[^\s"'<>]*)?"#).expect("内置 APK 链接正则无效")
    })
}

pub fn decode_basic_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

pub fn extract_apk_link(text: &str) -> Option<String> {
    let decoded = decode_basic_entities(text);
    apk_link_regex()
        .find(&decoded)
        .map(|item| item.as_str().trim_end_matches('"').to_string())
}

pub fn extract_backup_apk(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(url) = extract_apk_link(line) {
            return Some(url);
        }
    }
    extract_apk_link(text)
}

pub fn url_looks_like_apk(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or(url);
    path.to_ascii_lowercase().ends_with(".apk")
}

pub fn content_type_is_apk(content_type: &str) -> bool {
    let value = content_type.to_ascii_lowercase();
    value.contains("android.package-archive") || value.contains("application/vnd.android.package")
}

pub fn filename_from_url(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("arknights.apk");
    let sanitized: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() || !sanitized.to_ascii_lowercase().ends_with(".apk") {
        "arknights.apk".into()
    } else {
        sanitized
    }
}

pub fn parse_content_range(header: &str) -> Option<(u64, Option<u64>)> {
    let rest = header.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    Some((start, total))
}

#[derive(Debug, PartialEq, Eq)]
pub enum BodyMode {
    Restart,
    Append { start: u64 },
}

pub fn plan_body_mode(
    requested_start: u64,
    status: u16,
    content_range_start: Option<u64>,
) -> Result<BodyMode, String> {
    match status {
        200 => Ok(BodyMode::Restart),
        206 => {
            let start = content_range_start.unwrap_or(requested_start);
            if start == 0 {
                Ok(BodyMode::Restart)
            } else if start == requested_start {
                Ok(BodyMode::Append { start })
            } else {
                Err(format!(
                    "服务器续传起点 {start} 与本地 {requested_start} 不一致"
                ))
            }
        }
        other => Err(format!("意外的 HTTP 状态 {other}")),
    }
}

pub fn has_pk_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x50 && bytes[1] == 0x4B
}

pub async fn verify_apk_file(path: &Path, expected: Option<u64>) -> Result<u64, String> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|err| format!("无法读取 {}：{err}", path.display()))?;
    let len = meta.len();
    if let Some(expected) = expected {
        if len != expected {
            return Err(format!(
                "大小不符：实际 {}（{len} 字节），期望 {}（{expected} 字节）",
                format_bytes(len),
                format_bytes(expected)
            ));
        }
    } else if len < MIN_APK_BYTES {
        return Err(format!("文件过小（{len} 字节），且服务器未提供总大小"));
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|err| format!("无法打开 {}：{err}", path.display()))?;
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic)
        .await
        .map_err(|err| format!("无法读取 APK 魔数：{err}"))?;
    if !has_pk_magic(&magic) {
        return Err(format!(
            "不是 APK/ZIP（文件头 {:02X} {:02X}，应为 PK）",
            magic[0], magic[1]
        ));
    }
    Ok(len)
}

fn meta_path(apk: &Path) -> PathBuf {
    let mut name = apk
        .file_name()
        .map(|item| item.to_string_lossy().to_string())
        .unwrap_or_else(|| "arknights.apk".into());
    name.push_str(".meta.json");
    apk.with_file_name(name)
}

async fn write_meta(apk: &Path, meta: &CacheMeta) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(meta).map_err(|err| format!("写入缓存元数据失败：{err}"))?;
    tokio::fs::write(meta_path(apk), text)
        .await
        .map_err(|err| format!("写入缓存元数据失败：{err}"))
}

async fn read_meta(apk: &Path) -> Option<CacheMeta> {
    let text = tokio::fs::read_to_string(meta_path(apk)).await.ok()?;
    serde_json::from_str(&text).ok()
}

pub async fn find_cached_apk(cache_dir: &Path, client_version: &str) -> Option<ApkArtifact> {
    let mut entries = tokio::fs::read_dir(cache_dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy().to_string();
        if !name.to_ascii_lowercase().ends_with(".apk") || name.ends_with(".apk.part") {
            continue;
        }
        let meta = read_meta(&path).await;
        let version_ok = match check_filename_version(&name, client_version) {
            FilenameCheck::Match => true,
            FilenameCheck::Mismatch { .. } => false,
            FilenameCheck::Unmarked => meta
                .as_ref()
                .is_some_and(|item| item.version == client_version),
        };
        if !version_ok {
            continue;
        }
        let verified = match meta.as_ref() {
            Some(item) => verify_apk_file(&path, Some(item.size)).await.is_ok(),
            None => verify_apk_file(&path, None).await.is_ok(),
        };
        if !verified {
            continue;
        }
        let bytes = tokio::fs::metadata(&path).await.ok()?.len();
        return Some(ApkArtifact {
            path,
            bytes,
            version: client_version.to_string(),
            filename: name,
            url: meta.map(|item| item.url).unwrap_or_default(),
        });
    }
    None
}

async fn read_capped(response: reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| format!("读取下载页失败：{err}"))?;
        if buffer.len().saturating_add(chunk.len()) > cap {
            return Err("下载页响应过大，已停止读取，避免把 APK 本体读进内存".into());
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer)
}

pub async fn resolve_apk_url(client: &reqwest::Client) -> Result<String, String> {
    match resolve_official(client).await {
        Ok(url) => Ok(url),
        Err(official_error) => {
            tracing::warn!(error = %official_error, "官方下载入口失败，改用备用源");
            resolve_backup(client).await.map_err(|backup_error| {
                format!("官方入口失败：{official_error}；备用源失败：{backup_error}")
            })
        }
    }
}

async fn resolve_official(client: &reqwest::Client) -> Result<String, String> {
    let response = client
        .get(OFFICIAL_APK_PAGE)
        .send()
        .await
        .map_err(|err| format!("请求官方下载入口失败：{err}"))?;
    if !response.status().is_success() {
        return Err(format!("官方下载入口返回 HTTP {}", response.status()));
    }
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    if url_looks_like_apk(&final_url) || content_type_is_apk(&content_type) {
        return Ok(final_url);
    }
    if response.content_length().unwrap_or(0) > HTML_CAP as u64 {
        return Err("官方入口的最终响应不像下载页，且体积过大".into());
    }
    let body = read_capped(response, HTML_CAP).await?;
    let text = String::from_utf8_lossy(&body);
    extract_apk_link(&text).ok_or_else(|| "官方下载页里没有找到 .apk 链接".into())
}

async fn resolve_backup(client: &reqwest::Client) -> Result<String, String> {
    let response = client
        .get(BACKUP_LINKS)
        .send()
        .await
        .map_err(|err| format!("请求备用源失败：{err}"))?;
    if !response.status().is_success() {
        return Err(format!("备用源返回 HTTP {}", response.status()));
    }
    let text = response
        .text()
        .await
        .map_err(|err| format!("读取备用源失败：{err}"))?;
    extract_backup_apk(&text).ok_or_else(|| "备用源中没有 .apk 直链".into())
}

struct ProgressGate {
    last: Option<Instant>,
}

impl ProgressGate {
    fn new() -> Self {
        Self { last: None }
    }

    fn allow(&mut self, force: bool) -> bool {
        let due = match self.last {
            None => true,
            Some(last) => last.elapsed() >= PROGRESS_INTERVAL,
        };
        if force || due {
            self.last = Some(Instant::now());
            true
        } else {
            false
        }
    }
}

pub async fn obtain_apk<F>(
    client: &reqwest::Client,
    url: &str,
    cache_dir: &Path,
    client_version: &str,
    cancel: &CancellationToken,
    mut on_progress: F,
) -> Result<ApkArtifact, String>
where
    F: FnMut(u64, Option<u64>),
{
    if cancel.is_cancelled() {
        return Err("已取消".into());
    }
    tokio::fs::create_dir_all(cache_dir)
        .await
        .map_err(|err| format!("创建缓存目录失败：{err}"))?;
    // 假设：真正的 APK 约 1.8GB。开头只挡掉几乎写满的磁盘；知道 Content-Length 后再按文件大小复核。
    ensure_host_space(cache_dir, 512 * 1024 * 1024)?;

    let filename = filename_from_url(url);
    match check_filename_version(&filename, client_version) {
        FilenameCheck::Mismatch { found, expected } => {
            return Err(format!(
                "APK 文件名版本 {found} 与官方 clientVersion 压缩值 {expected} 不一致"
            ));
        }
        FilenameCheck::Match | FilenameCheck::Unmarked => {}
    }
    let final_path = cache_dir.join(&filename);
    if let Some(artifact) = find_cached_apk(cache_dir, client_version).await {
        if artifact.path == final_path || artifact.filename == filename {
            on_progress(artifact.bytes, Some(artifact.bytes));
            return Ok(artifact);
        }
    }

    let part_path = cache_dir.join(format!("{filename}.part"));
    let mut attempt_resume = true;
    for attempt in 0..2 {
        if cancel.is_cancelled() {
            return Err("已取消".into());
        }
        match download_once(
            client,
            url,
            &part_path,
            attempt_resume,
            cancel,
            &mut on_progress,
        )
        .await
        {
            Ok(total) => {
                if verify_apk_file(&part_path, total).await.is_ok()
                    || (total.is_none() && verify_apk_file(&part_path, None).await.is_ok())
                {
                    if final_path.exists() {
                        tokio::fs::remove_file(&final_path).await.ok();
                    }
                    tokio::fs::rename(&part_path, &final_path)
                        .await
                        .map_err(|err| format!("重命名 APK 失败：{err}"))?;
                    let bytes = tokio::fs::metadata(&final_path)
                        .await
                        .map_err(|err| format!("读取 APK 大小失败：{err}"))?
                        .len();
                    let artifact = ApkArtifact {
                        path: final_path.clone(),
                        bytes,
                        version: client_version.to_string(),
                        filename: filename.clone(),
                        url: url.to_string(),
                    };
                    write_meta(
                        &final_path,
                        &CacheMeta {
                            version: client_version.to_string(),
                            filename: filename.clone(),
                            size: bytes,
                            url: url.to_string(),
                        },
                    )
                    .await?;
                    on_progress(bytes, Some(bytes));
                    return Ok(artifact);
                }
                let message = verify_apk_file(&part_path, total)
                    .await
                    .err()
                    .unwrap_or_else(|| "APK 校验失败".into());
                tokio::fs::remove_file(&part_path).await.ok();
                if attempt == 1 {
                    return Err(message);
                }
                attempt_resume = false;
                tracing::warn!(error = %message, "APK 校验失败，丢弃临时文件后重下一次");
            }
            Err(err) if err == "range-restart" && attempt == 0 => {
                tokio::fs::remove_file(&part_path).await.ok();
                attempt_resume = false;
            }
            Err(err) => return Err(err),
        }
    }
    Err("下载失败".into())
}

fn ensure_host_space(dir: &Path, want: u64) -> Result<(), String> {
    let probe = if dir.exists() {
        dir.to_path_buf()
    } else {
        dir.parent().unwrap_or(dir).to_path_buf()
    };
    match crate::platform::free_bytes(&probe) {
        Some(free) if free < want => Err(format!(
            "下载目录所在磁盘剩余 {}，至少需要 {}",
            format_bytes(free),
            format_bytes(want)
        )),
        Some(_) => Ok(()),
        None => {
            // 假设：读不到宿主磁盘剩余空间时只告警，不中断。设备侧 df 仍会单独检查。
            tracing::warn!("无法读取下载目录剩余空间，继续下载");
            Ok(())
        }
    }
}

async fn download_once<F>(
    client: &reqwest::Client,
    url: &str,
    part_path: &Path,
    resume: bool,
    cancel: &CancellationToken,
    on_progress: &mut F,
) -> Result<Option<u64>, String>
where
    F: FnMut(u64, Option<u64>),
{
    let existing = if resume {
        tokio::fs::metadata(part_path)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0)
    } else {
        0
    };
    let mut request = client.get(url);
    if existing > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("连接下载地址失败：{err}"))?;
    let status = response.status().as_u16();
    if status == 416 {
        return Err("range-restart".into());
    }
    if !response.status().is_success() {
        return Err(format!("下载返回 HTTP {status}"));
    }
    let content_range = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_content_range);
    let mode = plan_body_mode(existing, status, content_range.map(|(start, _)| start))?;
    let total = match &mode {
        BodyMode::Restart => response.content_length(),
        BodyMode::Append { start } => content_range.and_then(|(_, total)| total).or_else(|| {
            response
                .content_length()
                .map(|len| start.saturating_add(len))
        }),
    };
    if let Some(total_bytes) = total {
        let parent = part_path.parent().unwrap_or(part_path);
        ensure_host_space(parent, total_bytes.saturating_add(64 * 1024 * 1024))?;
    }
    let start = match mode {
        BodyMode::Restart => 0,
        BodyMode::Append { start } => start,
    };

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part_path)
        .await
        .map_err(|err| format!("打开临时文件失败：{err}"))?;
    if start == 0 {
        file.set_len(0)
            .await
            .map_err(|err| format!("清空临时文件失败：{err}"))?;
    } else {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|err| format!("定位临时文件失败：{err}"))?;
    }
    let mut writer = BufWriter::new(file);
    let mut downloaded = start;
    let mut since_flush = 0u64;
    let mut gate = ProgressGate::new();
    if gate.allow(true) {
        on_progress(downloaded, total);
    }
    let mut stream = response.bytes_stream();
    while let Some(item) = stream.next().await {
        if cancel.is_cancelled() {
            writer.flush().await.ok();
            if let Err(err) = writer.get_mut().sync_all().await {
                tracing::warn!(error = %err, "取消下载时同步临时文件失败");
            }
            return Err("已取消".into());
        }
        let chunk = item.map_err(|err| format!("读取下载流失败：{err}"))?;
        writer
            .write_all(&chunk)
            .await
            .map_err(|err| format!("写入 APK 失败：{err}"))?;
        downloaded += chunk.len() as u64;
        since_flush += chunk.len() as u64;
        if since_flush >= FLUSH_EVERY {
            writer
                .flush()
                .await
                .map_err(|err| format!("刷写 APK 失败：{err}"))?;
            since_flush = 0;
        }
        if gate.allow(false) {
            on_progress(downloaded, total);
        }
    }
    writer
        .flush()
        .await
        .map_err(|err| format!("刷写 APK 失败：{err}"))?;
    writer
        .get_mut()
        .sync_all()
        .await
        .map_err(|err| format!("同步 APK 失败：{err}"))?;
    if gate.allow(true) {
        on_progress(downloaded, total);
    }
    if let Some(total) = total {
        if downloaded != total {
            return Err(format!(
                "下载不完整：已写入 {} 字节，期望 {total} 字节",
                downloaded
            ));
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, HeaderMap, StatusCode};
    use axum::response::Response;
    use axum::routing::get;
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn extracts_sample_apk_and_entities() {
        let html = r#"<html><a href="https://ak.hycdn.cn/GzD1CpaWgmSq1wew/79.0/package/1/1/Android/79.0.0_R4C8v7EmosKsRK1Q/arknights-hg-2791.apk">ok</a></html>"#;
        let link = extract_apk_link(html).expect("link");
        assert!(link.ends_with("arknights-hg-2791.apk"));
        assert!(url_looks_like_apk(&link));
        assert!(!url_looks_like_apk(
            "https://launcher.hypergryph.com/game/latest/hash/1/1"
        ));
        let encoded = r#"https://cdn.example/a.apk?token=1&amp;n=2"#;
        assert!(extract_apk_link(encoded).unwrap().contains("&n=2"));
    }

    #[test]
    fn backup_uses_first_apk_line() {
        let text = "# comment\nnot a link\nhttps://cdn.example/first.apk\nhttps://cdn.example/second.apk\n";
        assert_eq!(
            extract_backup_apk(text).as_deref(),
            Some("https://cdn.example/first.apk")
        );
    }

    #[test]
    fn keeps_vendor_download_spelling() {
        assert!(OFFICIAL_APK_PAGE.ends_with("/downloads/android_lastest"));
    }

    #[test]
    fn parses_content_range_and_plans_resume() {
        assert_eq!(
            parse_content_range("bytes 100-199/1000"),
            Some((100, Some(1000)))
        );
        assert_eq!(
            plan_body_mode(100, 206, Some(100)).unwrap(),
            BodyMode::Append { start: 100 }
        );
        assert_eq!(plan_body_mode(100, 200, None).unwrap(), BodyMode::Restart);
        assert!(has_pk_magic(&[0x50, 0x4B, 0x03, 0x04]));
        assert!(!has_pk_magic(&[0x00, 0x01]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resumes_partial_download() {
        let body = {
            let mut data = vec![0x50, 0x4B, 0x03, 0x04];
            data.extend(std::iter::repeat_n(9u8, 4000));
            data
        };
        let total = body.len();
        let hits = Arc::new(AtomicUsize::new(0));
        let ranges = Arc::new(std::sync::Mutex::new(Vec::new()));
        let state = Arc::new(ServeState {
            body: body.clone(),
            hits: hits.clone(),
            ranges: ranges.clone(),
        });
        let app = Router::new()
            .route("/arknights-hg-2791.apk", get(serve_apk))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let dir = tempfile::tempdir().expect("temp");
        let part = dir.path().join("arknights-hg-2791.apk.part");
        tokio::fs::write(&part, &body[..1000]).await.expect("seed");
        let client = reqwest::Client::builder().build().expect("client");
        let url = format!("http://{addr}/arknights-hg-2791.apk");
        let artifact = obtain_apk(
            &client,
            &url,
            dir.path(),
            "2.7.91",
            &CancellationToken::new(),
            |_done, _total| {},
        )
        .await
        .expect("download");
        assert_eq!(artifact.bytes, total as u64);
        assert!(artifact.path.ends_with("arknights-hg-2791.apk"));
        let saved = tokio::fs::read(&artifact.path).await.expect("read");
        assert_eq!(saved, body);
        let seen = ranges.lock().expect("ranges").clone();
        assert!(
            seen.iter().any(|item| item == "bytes=1000-"),
            "ranges={seen:?}"
        );
        assert!(hits.load(Ordering::SeqCst) >= 1);

        let hits_before = hits.load(Ordering::SeqCst);
        let again = obtain_apk(
            &client,
            &url,
            dir.path(),
            "2.7.91",
            &CancellationToken::new(),
            |_d, _t| {},
        )
        .await
        .expect("cache");
        assert_eq!(again.bytes, artifact.bytes);
        assert_eq!(hits.load(Ordering::SeqCst), hits_before);
    }

    struct ServeState {
        body: Vec<u8>,
        hits: Arc<AtomicUsize>,
        ranges: Arc<std::sync::Mutex<Vec<String>>>,
    }

    async fn serve_apk(
        axum::extract::State(state): axum::extract::State<Arc<ServeState>>,
        headers: HeaderMap,
    ) -> Response {
        state.hits.fetch_add(1, Ordering::SeqCst);
        if let Some(range) = headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
        {
            state.ranges.lock().expect("lock").push(range.to_string());
            let start = range
                .trim()
                .strip_prefix("bytes=")
                .and_then(|rest| rest.trim_end_matches('-').parse::<usize>().ok())
                .unwrap_or(0);
            let slice = state.body.get(start..).unwrap_or_default().to_vec();
            let end = state.body.len().saturating_sub(1);
            let content_range = format!("bytes {start}-{end}/{}", state.body.len());
            return Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(
                    header::CONTENT_TYPE,
                    "application/vnd.android.package-archive",
                )
                .header(header::CONTENT_RANGE, content_range)
                .header(header::CONTENT_LENGTH, slice.len())
                .header(header::ACCEPT_RANGES, "bytes")
                .body(Body::from(slice))
                .expect("response");
        }
        Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                "application/vnd.android.package-archive",
            )
            .header(header::CONTENT_LENGTH, state.body.len())
            .header(header::ACCEPT_RANGES, "bytes")
            .body(Body::from(state.body.clone()))
            .expect("response")
    }
}
