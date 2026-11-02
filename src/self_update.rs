use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_REPO: &str = "ModerRAS/arknights-auto-update";
pub const DEFAULT_RELEASE_API: &str =
    "https://api.github.com/repos/ModerRAS/arknights-auto-update/releases/latest";

const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 80 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 64;

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn current_commit() -> Option<&'static str> {
    option_env!("ARK_UPDATER_GIT_SHA")
        .map(str::trim)
        .filter(|value| looks_like_commit(value))
}

pub fn current_release_target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        (os, arch) => Err(format!("没有 {os}-{arch} 的发布包")),
    }
}

pub fn running_from_cargo_target() -> bool {
    let Ok(path) = std::env::current_exe() else {
        return false;
    };
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    text.contains("/target/debug/") || text.contains("/target/release/")
}

pub fn current_restart_args() -> Vec<String> {
    let mut args = std::env::args().skip(1);
    let mut kept = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--replace-from" | "--replace-to" | "--restart-arg" => {
                let _ = args.next();
            }
            "--wait-for-parent" | "--from-self-update" => {}
            _ => kept.push(arg),
        }
    }
    kept
}

pub fn ignore_terminal_hangup() {
    #[cfg(unix)]
    unsafe {
        // SAFETY: SIG_IGN 是进程级信号处置，不借用其他数据。替换后的新进程需要忽略挂起，
        // 否则原控制台关闭时会把刚拉起的进程一起带走。
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAsset {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRelease {
    pub tag: String,
    pub version: String,
    pub commit: Option<String>,
    pub assets: Vec<RemoteAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDownload {
    pub version: String,
    pub commit: Option<String>,
    pub archive: RemoteAsset,
    pub checksum: RemoteAsset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDecision {
    Keep { message: String },
    Update(PlannedDownload),
}

#[derive(Debug, Clone)]
pub enum StageEvent {
    Downloading { bytes: u64, total: Option<u64> },
    Verifying,
    Extracting,
}

#[derive(Debug)]
pub enum StageOutcome {
    UpToDate {
        message: String,
    },
    Staged {
        path: PathBuf,
        version: String,
        commit: Option<String>,
    },
}

pub struct StageRequest<'a> {
    pub version_client: &'a reqwest::Client,
    pub manual_client: &'a reqwest::Client,
    pub feed: &'a str,
    pub install_exe: &'a Path,
    pub work_dir: &'a Path,
    pub current_version: &'a str,
    pub current_commit: Option<&'a str>,
    pub target: &'a str,
    pub cancel: &'a CancellationToken,
    pub events: Option<&'a tokio::sync::mpsc::UnboundedSender<StageEvent>>,
}

pub fn decide_update(
    current_version: &str,
    current_commit: Option<&str>,
    target: &str,
    remote: &RemoteRelease,
) -> Result<UpdateDecision, String> {
    let local = parse_semver(current_version)?;
    let remote_version = parse_semver(&remote.version)?;
    if remote_version < local {
        return Ok(UpdateDecision::Keep {
            message: format!(
                "远端 {} 比当前 {} 旧，保持现在的程序。",
                remote.version, current_version
            ),
        });
    }
    let same_version = remote_version == local;
    if same_version {
        match (current_commit.map(str::trim), remote.commit.as_deref()) {
            (Some(local_commit), Some(remote_commit))
                if looks_like_commit(local_commit)
                    && looks_like_commit(remote_commit)
                    && !commits_differ(local_commit, remote_commit) =>
            {
                return Ok(UpdateDecision::Keep {
                    message: format!("程序已是发布 {}。", remote.version),
                });
            }
            (Some(_), Some(_)) => {}
            _ => {
                return Ok(UpdateDecision::Keep {
                    message: format!(
                        "程序版本已经是 {}。没有可对照的提交号，所以不重复下载。",
                        remote.version
                    ),
                });
            }
        }
    }
    let download = select_download(remote, target)?;
    Ok(UpdateDecision::Update(download))
}

pub async fn inspect_release(
    client: &reqwest::Client,
    feed: &str,
    cancel: &CancellationToken,
) -> Result<RemoteRelease, String> {
    fetch_release(client, feed, cancel).await
}

pub async fn stage_planned(
    request: StageRequest<'_>,
    plan: &PlannedDownload,
) -> Result<PathBuf, String> {
    download_and_stage(&request, plan).await
}

pub async fn stage_release(request: StageRequest<'_>) -> Result<StageOutcome, String> {
    ensure_not_cancelled(request.cancel)?;
    let remote = fetch_release(request.version_client, request.feed, request.cancel).await?;
    match decide_update(
        request.current_version,
        request.current_commit,
        request.target,
        &remote,
    )? {
        UpdateDecision::Keep { message } => Ok(StageOutcome::UpToDate { message }),
        UpdateDecision::Update(plan) => {
            let staged = download_and_stage(&request, &plan).await?;
            Ok(StageOutcome::Staged {
                path: staged,
                version: plan.version,
                commit: plan.commit,
            })
        }
    }
}

pub fn swap_executable(from: &Path, to: &Path) -> Result<(), String> {
    validate_swap_paths(from, to)?;
    #[cfg(unix)]
    {
        fs::rename(from, to).map_err(|err| format!("替换程序失败：{err}"))?;
        make_executable(to)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        let backup = backup_path(to);
        let _ = fs::remove_file(&backup);
        fs::rename(to, &backup).map_err(|err| format!("移开正在运行的程序失败：{err}"))?;
        if let Err(err) = fs::rename(from, to) {
            let _ = fs::rename(&backup, to);
            return Err(format!("写入新程序失败：{err}"));
        }
        let _ = fs::remove_file(&backup);
        Ok(())
    }
}

pub fn apply_replacement(
    from: &Path,
    to: &Path,
    wait_for_parent: bool,
    restart_args: &[String],
) -> Result<(), String> {
    if !wait_for_parent || from.as_os_str().is_empty() || to.as_os_str().is_empty() {
        return Err("替换参数不完整".into());
    }
    let current = std::env::current_exe().map_err(|err| format!("找不到当前程序：{err}"))?;
    let current = current
        .canonicalize()
        .map_err(|err| format!("无法解析当前程序路径：{err}"))?;
    let to = to
        .canonicalize()
        .map_err(|err| format!("无法解析替换目标：{err}"))?;
    if current != to {
        return Err("只能替换当前正在运行的程序".into());
    }
    let from = from
        .canonicalize()
        .map_err(|err| format!("无法解析新程序：{err}"))?;
    validate_swap_paths(&from, &to)?;
    ignore_terminal_hangup();
    #[cfg(unix)]
    detach_session();
    wait_for_parent_exit()?;
    let mut last = "替换程序失败".to_string();
    for _ in 0..50 {
        match swap_executable(&from, &to) {
            Ok(()) => {
                restart_process(&to, restart_args)?;
                std::process::exit(0);
            }
            Err(err) => {
                last = err;
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    Err(last)
}

pub fn relaunch_and_exit(staged: &Path, restart_args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|err| format!("找不到当前程序：{err}"))?;
    let mut command = std::process::Command::new(&exe);
    command
        .arg("--replace-from")
        .arg(staged)
        .arg("--replace-to")
        .arg(&exe)
        .arg("--wait-for-parent");
    for arg in restart_args {
        command.arg("--restart-arg").arg(arg);
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    match command.spawn() {
        Ok(child) => {
            // 保留子进程的 stdin 写端，直到本进程退出后内核关掉它。
            // 替换进程把管道结束当成“原进程已经退出”。
            std::mem::forget(child);
            eprintln!("正在退出以便完成程序更新");
            std::process::exit(0);
        }
        Err(err) => Err(format!("无法启动替换进程：{err}")),
    }
}

pub fn parse_semver(text: &str) -> Result<(u64, u64, u64), String> {
    let raw = text.trim().trim_start_matches('v');
    let mut parts = raw.split('.');
    let major = parse_semver_part(parts.next(), raw)?;
    let minor = parse_semver_part(parts.next(), raw)?;
    let patch = parse_semver_part(parts.next(), raw)?;
    if parts.next().is_some() {
        return Err(format!("版本号需要是 MAJOR.MINOR.PATCH：{text}"));
    }
    Ok((major, minor, patch))
}

pub fn parse_sha256_file(text: &str) -> Result<String, String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let token = line.split_whitespace().next().unwrap_or("");
        if token.len() == 64 && token.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Ok(token.to_ascii_lowercase());
        }
    }
    Err("校验文件里没有 SHA256".into())
}

pub fn ensure_asset_url(feed: &str, asset: &str) -> Result<(), String> {
    let asset_url = reqwest::Url::parse(asset).map_err(|err| format!("更新包地址无效：{err}"))?;
    let feed_url = reqwest::Url::parse(feed).map_err(|err| format!("程序更新源无效：{err}"))?;
    if github_repo(feed).is_some() {
        let host = asset_url.host_str().unwrap_or("");
        let allowed = asset_url.scheme() == "https"
            && (host == "github.com"
                || host.ends_with(".github.com")
                || host.ends_with(".githubusercontent.com"));
        if !allowed {
            return Err(format!("程序更新包只能从 GitHub 下载，当前是 {asset}"));
        }
        return Ok(());
    }
    let same_host = asset_url.host_str() == feed_url.host_str();
    let loopback = host_is_loopback(asset_url.host_str());
    let scheme_ok = asset_url.scheme() == "https" || (asset_url.scheme() == "http" && loopback);
    if scheme_ok && (same_host || loopback && host_is_loopback(feed_url.host_str())) {
        Ok(())
    } else {
        Err(format!("程序更新包地址不在更新源允许的范围内：{asset}"))
    }
}

fn select_download(remote: &RemoteRelease, target: &str) -> Result<PlannedDownload, String> {
    let tar_name = format!("ark-updater-{}-{target}.tar.gz", remote.tag);
    let zip_name = format!("ark-updater-{}-{target}.zip", remote.tag);
    let preferred = if target.contains("windows") {
        [&zip_name, &tar_name]
    } else {
        [&tar_name, &zip_name]
    };
    let archive = preferred
        .into_iter()
        .find_map(|name| remote.assets.iter().find(|asset| asset.name == *name))
        .cloned()
        .ok_or_else(|| format!("当前平台没有对应的发布包：{target}"))?;
    let checksum_name = format!("{}.sha256", archive.name);
    let checksum = remote
        .assets
        .iter()
        .find(|asset| asset.name == checksum_name)
        .cloned()
        .ok_or_else(|| format!("发布里没有 {checksum_name}"))?;
    Ok(PlannedDownload {
        version: remote.version.clone(),
        commit: remote.commit.clone(),
        archive,
        checksum,
    })
}

async fn fetch_release(
    client: &reqwest::Client,
    feed: &str,
    cancel: &CancellationToken,
) -> Result<RemoteRelease, String> {
    ensure_not_cancelled(cancel)?;
    let text = get_text(client, feed, feed, cancel, MAX_TEXT_BYTES).await?;
    let document: ReleaseDocument =
        serde_json::from_str(&text).map_err(|err| format!("发布信息无法解析：{err}"))?;
    let version = document.tag_name.trim().trim_start_matches('v').to_string();
    parse_semver(&version)?;
    let mut commit = document
        .commit
        .as_deref()
        .filter(|value| looks_like_commit(value))
        .map(ToOwned::to_owned)
        .or_else(|| {
            document
                .target_commitish
                .as_deref()
                .filter(|value| looks_like_commit(value))
                .map(ToOwned::to_owned)
        });
    if commit.is_none() {
        if let Some((owner, repo)) = github_repo(feed) {
            commit = fetch_tag_commit(client, &owner, &repo, &document.tag_name, cancel)
                .await
                .ok();
        }
    }
    let assets = document
        .assets
        .into_iter()
        .map(|asset| RemoteAsset {
            name: asset.name,
            url: asset.browser_download_url,
            size: asset.size,
        })
        .collect();
    Ok(RemoteRelease {
        tag: document.tag_name,
        version,
        commit,
        assets,
    })
}

async fn fetch_tag_commit(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    tag: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    if !tag.starts_with('v') || parse_semver(tag.trim_start_matches('v')).is_err() {
        return Err("标签不是稳定版本".into());
    }
    let url = format!("https://api.github.com/repos/{owner}/{repo}/git/refs/tags/{tag}");
    let text = get_text(client, &url, &url, cancel, MAX_TEXT_BYTES).await?;
    let reference: GitRef =
        serde_json::from_str(&text).map_err(|err| format!("标签信息无法解析：{err}"))?;
    if reference.object.kind == "commit" && looks_like_commit(&reference.object.sha) {
        return Ok(reference.object.sha);
    }
    if reference.object.kind == "tag" {
        let tag_url = format!(
            "https://api.github.com/repos/{owner}/{repo}/git/tags/{}",
            reference.object.sha
        );
        let text = get_text(client, &tag_url, &tag_url, cancel, MAX_TEXT_BYTES).await?;
        let annotated: GitTag =
            serde_json::from_str(&text).map_err(|err| format!("附注标签无法解析：{err}"))?;
        if annotated.object.kind == "commit" && looks_like_commit(&annotated.object.sha) {
            return Ok(annotated.object.sha);
        }
    }
    Err("标签没有指向提交".into())
}

async fn download_and_stage(
    request: &StageRequest<'_>,
    plan: &PlannedDownload,
) -> Result<PathBuf, String> {
    ensure_asset_url(request.feed, &plan.archive.url)?;
    ensure_asset_url(request.feed, &plan.checksum.url)?;
    fs::create_dir_all(request.work_dir).map_err(|err| format!("创建更新目录失败：{err}"))?;
    let want = plan
        .archive
        .size
        .unwrap_or(32 * 1024 * 1024)
        .saturating_add(MAX_BINARY_BYTES);
    ensure_space(request.work_dir, want)?;
    let checksum_text = get_text(
        request.manual_client,
        &plan.checksum.url,
        request.feed,
        request.cancel,
        MAX_TEXT_BYTES,
    )
    .await?;
    let expected = parse_sha256_file(&checksum_text)?;
    let archive_path = request.work_dir.join(safe_file_name(&plan.archive.name)?);
    let part_path = archive_path.with_extension("part");
    let extracted = request.work_dir.join("ark-updater.next");
    let outcome = async {
        download_file(
            request.manual_client,
            &plan.archive.url,
            request.feed,
            &part_path,
            MAX_ARCHIVE_BYTES,
            request.cancel,
            request.events,
        )
        .await?;
        emit(request.events, StageEvent::Verifying);
        let actual = tokio::task::spawn_blocking({
            let part_path = part_path.clone();
            move || sha256_file(&part_path)
        })
        .await
        .map_err(|err| format!("校验任务中断：{err}"))??;
        if actual != expected {
            return Err("程序包 SHA256 不一致".into());
        }
        fs::rename(&part_path, &archive_path).map_err(|err| format!("保存程序包失败：{err}"))?;
        emit(request.events, StageEvent::Extracting);
        let archive_path = archive_path.clone();
        let extracted_for_task = extracted.clone();
        tokio::task::spawn_blocking(move || extract_binary(&archive_path, &extracted_for_task))
            .await
            .map_err(|err| format!("解压任务中断：{err}"))??;
        let staged = staged_path(request.install_exe);
        copy_staged(&extracted, &staged)?;
        Ok(staged)
    }
    .await;
    if outcome.is_err() {
        let _ = fs::remove_file(&part_path);
        let _ = fs::remove_file(&extracted);
    }
    outcome
}

async fn get_text(
    client: &reqwest::Client,
    url: &str,
    feed: &str,
    cancel: &CancellationToken,
    max_bytes: u64,
) -> Result<String, String> {
    let response = open_checked(client, url, feed, cancel).await?;
    let bytes = read_body(response, max_bytes, cancel).await?;
    String::from_utf8(bytes).map_err(|_| "响应不是 UTF-8".into())
}

async fn open_checked(
    client: &reqwest::Client,
    start: &str,
    feed: &str,
    cancel: &CancellationToken,
) -> Result<reqwest::Response, String> {
    let mut next = start.to_string();
    for _ in 0..6 {
        ensure_not_cancelled(cancel)?;
        ensure_asset_url(feed, &next)?;
        let response = client
            .get(&next)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|err| format!("请求更新信息失败：{err}"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| "重定向缺少 Location".to_string())?;
            let location = location
                .to_str()
                .map_err(|_| "重定向地址无效".to_string())?;
            let base = reqwest::Url::parse(&next).map_err(|err| format!("更新地址无效：{err}"))?;
            next = base
                .join(location)
                .map_err(|err| format!("重定向地址无效：{err}"))?
                .to_string();
            continue;
        }
        let final_url = response.url().clone();
        ensure_asset_url(feed, final_url.as_str())?;
        if !response.status().is_success() {
            return Err(format!("更新请求返回 HTTP {}", response.status()));
        }
        return Ok(response);
    }
    Err("重定向过多".into())
}

async fn read_body(
    mut response: reqwest::Response,
    max_bytes: u64,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, String> {
    if let Some(length) = response.content_length() {
        if length > max_bytes {
            return Err("更新响应过大".into());
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| format!("读取更新响应失败：{err}"))?
    {
        ensure_not_cancelled(cancel)?;
        if body.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err("更新响应过大".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn download_file(
    client: &reqwest::Client,
    url: &str,
    feed: &str,
    dest: &Path,
    max_bytes: u64,
    cancel: &CancellationToken,
    events: Option<&tokio::sync::mpsc::UnboundedSender<StageEvent>>,
) -> Result<(), String> {
    let response = open_checked(client, url, feed, cancel).await?;
    if let Some(length) = response.content_length() {
        if length > max_bytes {
            return Err("程序包超过 64MB，已放弃".into());
        }
    }
    let total = response.content_length();
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("创建更新目录失败：{err}"))?;
    }
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|err| format!("创建程序包失败：{err}"))?;
    let mut written = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        ensure_not_cancelled(cancel)?;
        let chunk = chunk.map_err(|err| format!("下载程序包失败：{err}"))?;
        written = written.saturating_add(chunk.len() as u64);
        if written > max_bytes {
            return Err("程序包超过 64MB，已放弃".into());
        }
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|err| format!("写入程序包失败：{err}"))?;
        emit(
            events,
            StageEvent::Downloading {
                bytes: written,
                total,
            },
        );
    }
    file.sync_all()
        .await
        .map_err(|err| format!("保存程序包失败：{err}"))?;
    Ok(())
}

fn extract_binary(archive: &Path, dest: &Path) -> Result<(), String> {
    if archive
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        extract_zip(archive, dest)
    } else {
        extract_tar_gz(archive, dest)
    }
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|err| format!("打开压缩包失败：{err}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|err| format!("压缩包无法读取：{err}"))?;
    if zip.len() > MAX_ENTRIES {
        return Err("压缩包条目过多".into());
    }
    let mut found = false;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|err| format!("读取压缩条目失败：{err}"))?;
        let Some(path) = entry.enclosed_name() else {
            return Err("压缩包包含非法路径".into());
        };
        if !is_binary_name(&path) {
            continue;
        }
        copy_limited(&mut entry, dest)?;
        found = true;
        break;
    }
    if !found {
        return Err("压缩包里没有 ark-updater".into());
    }
    verify_executable_magic(dest)?;
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|err| format!("打开压缩包失败：{err}"))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    let mut found = false;
    let mut seen = 0usize;
    for entry in tar
        .entries()
        .map_err(|err| format!("压缩包无法读取：{err}"))?
    {
        seen += 1;
        if seen > MAX_ENTRIES {
            return Err("压缩包条目过多".into());
        }
        let mut entry = entry.map_err(|err| format!("读取压缩条目失败：{err}"))?;
        let path = entry
            .path()
            .map_err(|err| format!("压缩包路径无效：{err}"))?;
        if !path_is_safe(&path) {
            return Err("压缩包包含非法路径".into());
        }
        if !is_binary_name(&path) {
            continue;
        }
        copy_limited(&mut entry, dest)?;
        found = true;
        break;
    }
    if !found {
        return Err("压缩包里没有 ark-updater".into());
    }
    verify_executable_magic(dest)?;
    Ok(())
}

fn copy_limited(reader: &mut impl Read, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("创建更新目录失败：{err}"))?;
    }
    let mut out = File::create(dest).map_err(|err| format!("写出程序失败：{err}"))?;
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader
            .read(&mut buf)
            .map_err(|err| format!("解压程序失败：{err}"))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > MAX_BINARY_BYTES {
            let _ = fs::remove_file(dest);
            return Err("解压后的程序超过 80MB".into());
        }
        out.write_all(&buf[..read])
            .map_err(|err| format!("写出程序失败：{err}"))?;
    }
    if total == 0 {
        let _ = fs::remove_file(dest);
        return Err("解压后的程序是空的".into());
    }
    out.sync_all()
        .map_err(|err| format!("保存程序失败：{err}"))?;
    Ok(())
}

fn verify_executable_magic(path: &Path) -> Result<(), String> {
    let mut file = File::open(path).map_err(|err| format!("读取新程序失败：{err}"))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .map_err(|_| "解出的文件不是可执行程序".to_string())?;
    let macho = matches!(
        magic,
        [0xFE, 0xED, 0xFA, 0xCE]
            | [0xFE, 0xED, 0xFA, 0xCF]
            | [0xCE, 0xFA, 0xED, 0xFE]
            | [0xCF, 0xFA, 0xED, 0xFE]
            | [0xCA, 0xFE, 0xBA, 0xBE]
            | [0xBE, 0xBA, 0xFE, 0xCA]
    );
    if magic.starts_with(b"\x7FELF") || magic.starts_with(b"MZ") || macho {
        Ok(())
    } else {
        let _ = fs::remove_file(path);
        Err("解出的文件不是可执行程序".into())
    }
}

fn copy_staged(source: &Path, staged: &Path) -> Result<(), String> {
    if let Some(parent) = staged.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("创建程序目录失败：{err}"))?;
    }
    fs::copy(source, staged).map_err(|err| {
        format!("程序所在目录不可写，无法替换自己（{err}）。可以把程序放到可写目录后再更新。")
    })?;
    make_executable(staged)?;
    let file = File::options()
        .write(true)
        .open(staged)
        .map_err(|err| format!("打开新程序失败：{err}"))?;
    file.sync_all()
        .map_err(|err| format!("保存新程序失败：{err}"))?;
    Ok(())
}

fn staged_path(exe: &Path) -> PathBuf {
    let name = exe
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("ark-updater"));
    let mut staged = name.to_os_string();
    staged.push(".new");
    exe.with_file_name(staged)
}

#[cfg(windows)]
fn backup_path(exe: &Path) -> PathBuf {
    let name = exe
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("ark-updater"));
    let mut backup = name.to_os_string();
    backup.push(".old");
    exe.with_file_name(backup)
}

fn validate_swap_paths(from: &Path, to: &Path) -> Result<(), String> {
    let from_name = from.file_name().ok_or("替换来源缺少文件名")?;
    let to_name = to.file_name().ok_or("替换目标缺少文件名")?;
    let mut expected = to_name.to_os_string();
    expected.push(".new");
    if from_name != expected.as_os_str() {
        return Err("替换来源必须是目标旁边的 .new 文件".into());
    }
    if from.parent() != to.parent() {
        return Err("替换文件必须和当前程序在同一目录".into());
    }
    if !from.is_file() {
        return Err("找不到新程序文件".into());
    }
    Ok(())
}

fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|err| format!("读取程序权限失败：{err}"))?
            .permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        fs::set_permissions(path, permissions).map_err(|err| format!("设置程序权限失败：{err}"))?;
    }
    let _ = path;
    Ok(())
}

fn restart_process(exe: &Path, args: &[String]) -> Result<(), String> {
    std::process::Command::new(exe)
        .arg("--from-self-update")
        .args(args)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("重新启动失败：{err}"))
}

fn wait_for_parent_exit() -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 32];
        let result = std::io::Read::read(&mut stdin, &mut buf);
        let _ = tx.send(result);
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(format!("等待原进程退出失败：{err}")),
        Err(_) => Err("等待原进程退出超时".into()),
    }
}

#[cfg(unix)]
fn detach_session() {
    unsafe {
        // SAFETY: setsid 只作用于当前进程，失败时继续替换。调用前没有别的线程依赖控制终端。
        let _ = libc::setsid();
    }
}

fn ensure_not_cancelled(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("已取消".into())
    } else {
        Ok(())
    }
}

fn emit(events: Option<&tokio::sync::mpsc::UnboundedSender<StageEvent>>, event: StageEvent) {
    if let Some(sender) = events {
        let _ = sender.send(event);
    }
}

fn ensure_space(dir: &Path, want: u64) -> Result<(), String> {
    let probe = if dir.exists() {
        dir.to_path_buf()
    } else {
        dir.parent().unwrap_or(dir).to_path_buf()
    };
    match crate::platform::free_bytes(&probe) {
        Some(free) if free < want => Err(format!(
            "磁盘剩余 {}，程序更新至少需要 {}",
            crate::util::format_bytes(free),
            crate::util::format_bytes(want)
        )),
        _ => Ok(()),
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|err| format!("读取程序包失败：{err}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|err| format!("读取程序包失败：{err}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn safe_file_name(name: &str) -> Result<&str, String> {
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") || name.starts_with('.')
    {
        Err(format!("发布文件名无效：{name}"))
    } else {
        Ok(name)
    }
}

fn path_is_safe(path: &Path) -> bool {
    path.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn is_binary_name(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("ark-updater" | "ark-updater.exe")
    )
}

fn looks_like_commit(value: &str) -> bool {
    let value = value.trim();
    (7..=40).contains(&value.len()) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn commits_differ(left: &str, right: &str) -> bool {
    let left = left.trim().to_ascii_lowercase();
    let right = right.trim().to_ascii_lowercase();
    !(left.starts_with(&right) || right.starts_with(&left))
}

fn parse_semver_part(part: Option<&str>, raw: &str) -> Result<u64, String> {
    let Some(part) = part else {
        return Err(format!("版本号需要是 MAJOR.MINOR.PATCH：{raw}"));
    };
    if part.is_empty()
        || (part.len() > 1 && part.starts_with('0'))
        || !part.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(format!("版本号需要是 MAJOR.MINOR.PATCH：{raw}"));
    }
    part.parse()
        .map_err(|_| format!("版本号需要是 MAJOR.MINOR.PATCH：{raw}"))
}

fn host_is_loopback(host: Option<&str>) -> bool {
    matches!(host, Some("localhost" | "127.0.0.1" | "::1"))
}

fn github_repo(feed: &str) -> Option<(String, String)> {
    let url = reqwest::Url::parse(feed).ok()?;
    if url.host_str() != Some("api.github.com") {
        return None;
    }
    let mut parts = url.path().split('/').filter(|part| !part.is_empty());
    if parts.next() != Some("repos") {
        return None;
    }
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    Some((owner, repo))
}

#[derive(Debug, Deserialize)]
struct ReleaseDocument {
    tag_name: String,
    #[serde(default)]
    target_commitish: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    assets: Vec<ReleaseAssetDocument>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAssetDocument {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GitRef {
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitTag {
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitObject {
    sha: String,
    #[serde(rename = "type")]
    kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str, url: &str) -> RemoteAsset {
        RemoteAsset {
            name: name.into(),
            url: url.into(),
            size: None,
        }
    }

    fn release(version: &str, commit: Option<&str>, target: &str) -> RemoteRelease {
        let tag = format!("v{version}");
        let archive = format!("ark-updater-{tag}-{target}.tar.gz");
        RemoteRelease {
            tag,
            version: version.into(),
            commit: commit.map(str::to_string),
            assets: vec![
                asset(&archive, "https://github.com/example/archive"),
                asset(
                    &format!("{archive}.sha256"),
                    "https://github.com/example/checksum",
                ),
            ],
        }
    }

    #[test]
    fn newer_version_is_selected() {
        let remote = release("0.2.0", None, "x86_64-unknown-linux-musl");
        match decide_update("0.1.0", None, "x86_64-unknown-linux-musl", &remote).unwrap() {
            UpdateDecision::Update(plan) => assert_eq!(plan.version, "0.2.0"),
            UpdateDecision::Keep { message } => panic!("应更新：{message}"),
        }
    }

    #[test]
    fn same_version_without_commit_stays() {
        let remote = release(
            "0.1.0",
            Some("0123456789abcdef"),
            "x86_64-unknown-linux-musl",
        );
        match decide_update("0.1.0", None, "x86_64-unknown-linux-musl", &remote).unwrap() {
            UpdateDecision::Keep { message } => assert!(message.contains("提交号")),
            UpdateDecision::Update(_) => panic!("开发构建不应因同版本重复下载"),
        }
    }

    #[test]
    fn same_version_different_commit_updates() {
        let remote = release(
            "0.1.0",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "x86_64-pc-windows-msvc",
        );
        let remote = RemoteRelease {
            assets: vec![
                asset(
                    "ark-updater-v0.1.0-x86_64-pc-windows-msvc.zip",
                    "https://github.com/example/app.zip",
                ),
                asset(
                    "ark-updater-v0.1.0-x86_64-pc-windows-msvc.zip.sha256",
                    "https://github.com/example/app.zip.sha256",
                ),
            ],
            ..remote
        };
        match decide_update(
            "0.1.0",
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "x86_64-pc-windows-msvc",
            &remote,
        )
        .unwrap()
        {
            UpdateDecision::Update(plan) => assert!(plan.archive.name.ends_with(".zip")),
            UpdateDecision::Keep { message } => panic!("同版本新提交应更新：{message}"),
        }
    }

    #[test]
    fn older_release_is_kept() {
        let remote = release(
            "0.1.0",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "x86_64-unknown-linux-musl",
        );
        match decide_update(
            "0.2.0",
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "x86_64-unknown-linux-musl",
            &remote,
        )
        .unwrap()
        {
            UpdateDecision::Keep { message } => assert!(message.contains("旧")),
            UpdateDecision::Update(_) => panic!("不能降级"),
        }
    }

    #[test]
    fn sha256_accepts_hash_or_sum_line() {
        assert_eq!(
            parse_sha256_file("abc\nabcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789  app.tar.gz\n").unwrap(),
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
    }

    #[test]
    fn github_assets_stay_on_github() {
        assert!(ensure_asset_url(
            DEFAULT_RELEASE_API,
            "https://github.com/ModerRAS/arknights-auto-update/releases/download/v0.1.0/app.tar.gz"
        )
        .is_ok());
        assert!(ensure_asset_url(
            DEFAULT_RELEASE_API,
            "https://release-assets.githubusercontent.com/app"
        )
        .is_ok());
        assert!(ensure_asset_url(DEFAULT_RELEASE_API, "https://evil.example/app").is_err());
        assert!(ensure_asset_url(DEFAULT_RELEASE_API, "http://127.0.0.1/app").is_err());
    }

    #[test]
    fn swap_replaces_destination_bytes() {
        let dir = tempfile::tempdir().expect("temp");
        let dest = dir.path().join("ark-updater");
        let source = dir.path().join("ark-updater.new");
        fs::write(&dest, b"old").unwrap();
        fs::write(&source, b"new").unwrap();
        swap_executable(&source, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert!(!source.exists());
    }

    #[test]
    fn rejects_parent_directory_in_archive() {
        let dir = tempfile::tempdir().expect("temp");
        let archive = dir.path().join("bad.tar.gz");
        pack_malicious_parent_tar(&archive);
        let dest = dir.path().join("out");
        let err = extract_binary(&archive, &dest).unwrap_err();
        assert!(err.contains("非法"), "{err}");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn downloads_archive_checks_hash_and_extracts_binary() {
        let dir = tempfile::tempdir().expect("temp");
        let target = current_release_target().expect("target");
        let payload = b"\x7FELFark-updater-test";
        let archive_name = format!("ark-updater-v0.2.0-{target}.tar.gz");
        let archive_path = dir.path().join(&archive_name);
        pack_tar(
            &archive_path,
            &[
                ("ark-updater-v0.2.0/README.md", b"notes"),
                ("ark-updater-v0.2.0/ark-updater", payload),
            ],
        );
        let archive_bytes = fs::read(&archive_path).unwrap();
        let digest = format!("{:x}", Sha256::digest(&archive_bytes));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let checksum_name = format!("{archive_name}.sha256");
        let body = archive_bytes.clone();
        let checksum = format!("{digest}  {archive_name}\n");
        let json = serde_json::json!({
            "tag_name": "v0.2.0",
            "commit": "0123456789abcdef0123456789abcdef01234567",
            "assets": [
                {
                    "name": archive_name,
                    "browser_download_url": format!("http://{addr}/{archive_name}"),
                    "size": body.len(),
                },
                {
                    "name": checksum_name,
                    "browser_download_url": format!("http://{addr}/{checksum_name}"),
                    "size": checksum.len(),
                }
            ]
        })
        .to_string();
        tokio::spawn(async move {
            serve_release(
                listener,
                &json,
                &archive_name,
                &body,
                &checksum_name,
                &checksum,
            )
            .await;
        });

        let version_client = crate::http_client::build_clients(Some("off"))
            .unwrap()
            .version;
        let manual = crate::http_client::manual_redirect_client(Some("off")).unwrap();
        let install = dir.path().join("ark-updater");
        fs::write(&install, b"old").unwrap();
        let outcome = stage_release(StageRequest {
            version_client: &version_client,
            manual_client: &manual,
            feed: &format!("http://{addr}/latest"),
            install_exe: &install,
            work_dir: &dir.path().join("updates"),
            current_version: "0.1.0",
            current_commit: None,
            target,
            cancel: &CancellationToken::new(),
            events: None,
        })
        .await
        .expect("stage");
        match outcome {
            StageOutcome::Staged { path, version, .. } => {
                assert_eq!(version, "0.2.0");
                assert_eq!(fs::read(path).unwrap(), payload);
            }
            StageOutcome::UpToDate { message } => panic!("应下载：{message}"),
        }
        assert!(!dir.path().join("updates").join("README.md").exists());
    }

    #[tokio::test]
    async fn checksum_mismatch_leaves_no_staged_binary() {
        let dir = tempfile::tempdir().expect("temp");
        let target = current_release_target().expect("target");
        let archive_name = format!("ark-updater-v0.2.0-{target}.tar.gz");
        let archive_path = dir.path().join(&archive_name);
        pack_tar(&archive_path, &[("ark-updater", b"\x7FELFbad")]);
        let body = fs::read(&archive_path).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let checksum_name = format!("{archive_name}.sha256");
        let checksum = format!("{}\n", "ab".repeat(32));
        let json = serde_json::json!({
            "tag_name": "v0.2.0",
            "assets": [
                {"name": archive_name, "browser_download_url": format!("http://{addr}/{archive_name}")},
                {"name": checksum_name, "browser_download_url": format!("http://{addr}/{checksum_name}")}
            ]
        })
        .to_string();
        let archive_bytes = body.clone();
        tokio::spawn(async move {
            serve_release(
                listener,
                &json,
                &archive_name,
                &archive_bytes,
                &checksum_name,
                &checksum,
            )
            .await;
        });
        let version_client = crate::http_client::build_clients(Some("off"))
            .unwrap()
            .version;
        let manual = crate::http_client::manual_redirect_client(Some("off")).unwrap();
        let install = dir.path().join("ark-updater");
        fs::write(&install, b"old").unwrap();
        let err = stage_release(StageRequest {
            version_client: &version_client,
            manual_client: &manual,
            feed: &format!("http://{addr}/latest"),
            install_exe: &install,
            work_dir: &dir.path().join("updates"),
            current_version: "0.1.0",
            current_commit: None,
            target,
            cancel: &CancellationToken::new(),
            events: None,
        })
        .await
        .unwrap_err();
        assert!(err.contains("SHA256"), "{err}");
        assert!(!dir.path().join("ark-updater.new").exists());
    }

    fn pack_malicious_parent_tar(path: &Path) {
        let name = b"../ark-updater";
        let data = b"\x7FELFbad";
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name);
        header[100..108].copy_from_slice(b"0000755\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size = format!("{:011o}\0", data.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        let checksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());
        let mut raw = Vec::new();
        raw.extend_from_slice(&header);
        raw.extend_from_slice(data);
        let pad = (512 - (data.len() % 512)) % 512;
        raw.extend(std::iter::repeat_n(0, pad));
        raw.extend(std::iter::repeat_n(0, 1024));
        let file = File::create(path).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap();
    }

    fn pack_tar(path: &Path, files: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.finish().unwrap();
    }

    async fn serve_release(
        listener: tokio::net::TcpListener,
        json: &str,
        archive_name: &str,
        archive: &[u8],
        checksum_name: &str,
        checksum: &str,
    ) {
        let routes = [
            ("/latest", json.as_bytes().to_vec(), "application/json"),
            (
                &format!("/{archive_name}"),
                archive.to_vec(),
                "application/gzip",
            ),
            (
                &format!("/{checksum_name}"),
                checksum.as_bytes().to_vec(),
                "text/plain",
            ),
        ];
        let routes: Vec<(String, Vec<u8>, &'static str)> = routes
            .into_iter()
            .map(|(path, body, kind)| (path.to_string(), body, kind))
            .collect();
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0u8; 2048];
            let Ok(n) = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await else {
                continue;
            };
            let text = String::from_utf8_lossy(&buf[..n]);
            let path = text.split_whitespace().nth(1).unwrap_or("");
            let found = routes.iter().find(|(route, _, _)| route == path);
            let (status, kind, body) = if let Some((_, body, kind)) = found {
                ("200 OK", *kind, body.clone())
            } else {
                ("404 Not Found", "text/plain", b"missing".to_vec())
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, &body).await;
        }
    }
}
