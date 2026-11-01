use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;

use crate::config::AppConfig;
use crate::process_util::{run_command, CommandError, CommandOutput, CommandSpec};
use crate::util::nonempty;
use crate::version::{PACKAGE_BILIBILI, PACKAGE_OFFICIAL};

pub const COMMON_ADB_PORTS: &[u16] = &[16384, 16416, 7555, 5555, 62001, 62025, 21503, 5556];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDeviceLine {
    pub serial: String,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct AdbInfo {
    pub path: PathBuf,
    pub version: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProps {
    pub model: String,
    pub android_version: String,
    pub abi: String,
}

pub fn props_script() -> &'static str {
    "getprop ro.product.model; echo SPLIT; getprop ro.build.version.release; echo SPLIT; getprop ro.product.cpu.abilist; echo SPLIT; getprop ro.product.cpu.abi"
}

pub fn inventory_script() -> String {
    format!(
        "echo '---OFFICIAL---'; dumpsys package {PACKAGE_OFFICIAL}; echo '---BILIBILI---'; dumpsys package {PACKAGE_BILIBILI}"
    )
}

pub fn storage_script() -> &'static str {
    "df /data"
}

pub fn parse_adb_devices(output: &str) -> Vec<AdbDeviceLine> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.to_ascii_lowercase().starts_with("list of") {
                return None;
            }
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            if serial == "*" || serial.starts_with('*') {
                return None;
            }
            let state = parts.next().unwrap_or("unknown").to_string();
            Some(AdbDeviceLine { serial, state })
        })
        .collect()
}

fn version_name_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"versionName=(\S+)").expect("内置 versionName 正则无效"))
}

pub fn parse_version_name(dumpsys: &str) -> Option<String> {
    let name = version_name_regex()
        .captures(dumpsys)?
        .get(1)?
        .as_str()
        .trim()
        .trim_end_matches('\r');
    if name.is_empty() || name.eq_ignore_ascii_case("null") {
        None
    } else {
        Some(name.to_string())
    }
}

pub fn parse_package_inventory(output: &str) -> (Option<String>, bool) {
    let (official, bilibili) = output.split_once("---BILIBILI---").unwrap_or((output, ""));
    (
        parse_version_name(official),
        parse_version_name(bilibili).is_some(),
    )
}

pub fn parse_props(output: &str) -> DeviceProps {
    let parts: Vec<&str> = output.split("SPLIT").map(str::trim).collect();
    let model = nonempty(parts.first().copied()).unwrap_or_else(|| "未知".into());
    let android_version = nonempty(parts.get(1).copied()).unwrap_or_else(|| "未知".into());
    let mut abi = nonempty(parts.get(2).copied()).unwrap_or_default();
    let primary = nonempty(parts.get(3).copied()).unwrap_or_default();
    if abi.is_empty() {
        abi = primary;
    } else if !primary.is_empty()
        && !abi
            .to_ascii_lowercase()
            .split([',', ' '])
            .any(|item| item == primary.to_ascii_lowercase())
    {
        abi = format!("{abi},{primary}");
    }
    if abi.is_empty() {
        abi = "未知".into();
    }
    DeviceProps {
        model,
        android_version,
        abi,
    }
}

/// ABI 判定大小写不敏感。只要列表里有 arm64-v8a 或 armeabi-v7a 就认为可以安装官服包。
pub fn abi_compatible(abi_list: &str) -> bool {
    abi_list
        .split([',', ' ', ';'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .any(|item| {
            let item = item.to_ascii_lowercase();
            item == "arm64-v8a" || item == "armeabi-v7a"
        })
}

pub fn parse_df_available_bytes(output: &str) -> Option<u64> {
    let mut factor = 1024u64;
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("512-blocks") {
            factor = 512;
        } else if lower.contains("1k-blocks") || lower.contains("1024-blocks") {
            factor = 1024;
        } else if lower.contains("1m-blocks") {
            factor = 1024 * 1024;
        }
    }
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.to_ascii_lowercase().starts_with("filesystem") {
            continue;
        }
        let cols: Vec<&str> = trimmed.split_whitespace().collect();
        let Some(percent_at) = cols.iter().position(|col| col.ends_with('%')) else {
            continue;
        };
        if percent_at == 0 {
            continue;
        }
        if let Ok(available) = cols[percent_at - 1].parse::<u64>() {
            return Some(available.saturating_mul(factor));
        }
    }
    None
}

pub fn adb_server_mismatch(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("server version")
        || lower.contains("doesn't match this client")
        || lower.contains("does not match this client")
        || lower.contains("protocol fault")
}

pub fn classify_install_output(output: &str) -> Option<crate::error::FailureKind> {
    use crate::error::FailureKind;
    let lower = output.to_ascii_lowercase();
    if lower.contains("install_failed_no_matching_abis") {
        return Some(FailureKind::NoMatchingAbi);
    }
    if lower.contains("install_failed_update_incompatible")
        || lower.contains("signatures do not match")
    {
        return Some(FailureKind::SignatureConflict);
    }
    if lower.contains("install_failed_insufficient_storage")
        || lower.contains("not enough space")
        || lower.contains("install_failed_no_space")
    {
        return Some(FailureKind::InsufficientStorage);
    }
    if lower.contains("device offline")
        || lower.contains("device not found")
        || lower.contains("no devices/emulators found")
        || lower.contains("error: closed")
    {
        return Some(FailureKind::DeviceOffline);
    }
    None
}

fn adb_file_name() -> &'static str {
    if cfg!(windows) {
        "adb.exe"
    } else {
        "adb"
    }
}

pub fn adb_on_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let name = adb_file_name();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub async fn probe_adb(path: &Path, source: &str) -> Result<AdbInfo, String> {
    if !path.is_file() {
        return Err(format!("文件不存在：{}", path.display()));
    }
    let spec = CommandSpec::new(path, ["version"]);
    let output = run_command(&spec, Duration::from_secs(8))
        .await
        .map_err(|err| err.to_string())?;
    let text = output.combined();
    if !output.success() {
        return Err(format!("`adb version` 失败：{}", text.trim()));
    }
    let version = text.lines().next().unwrap_or("").trim().to_string();
    let lower = text.to_ascii_lowercase();
    if !lower.contains("android debug bridge") && !lower.contains("version") {
        return Err(format!("该程序不像 adb：{version}"));
    }
    Ok(AdbInfo {
        path: path.to_path_buf(),
        version,
        source: source.to_string(),
    })
}

pub async fn discover_adb(config: &AppConfig) -> Result<AdbInfo, String> {
    if let Some(configured) = nonempty(config.adb_path.as_deref()) {
        return probe_adb(Path::new(&configured), "配置文件")
            .await
            .map_err(|err| format!("配置的 adb_path 不可用：{err}"));
    }
    discover_adb_auto().await
}

pub async fn discover_adb_auto() -> Result<AdbInfo, String> {
    let mut tried = Vec::new();
    if let Some(from_env) = nonempty(std::env::var("ADB").ok().as_deref()) {
        match probe_adb(Path::new(&from_env), "环境变量 ADB").await {
            Ok(info) => return Ok(info),
            Err(err) => tried.push(format!("环境变量 ADB（{from_env}）：{err}")),
        }
    }
    if let Some(on_path) = adb_on_path() {
        match probe_adb(&on_path, "PATH").await {
            Ok(info) => return Ok(info),
            Err(err) => tried.push(format!("PATH（{}）：{err}", on_path.display())),
        }
    }
    for (candidate, source) in crate::platform::static_adb_candidates() {
        if !candidate.is_file() {
            continue;
        }
        match probe_adb(&candidate, source).await {
            Ok(info) => return Ok(info),
            Err(err) => tried.push(format!("{source}（{}）：{err}", candidate.display())),
        }
    }
    let mut message =
        "未找到可用的 adb。请安装 platform-tools，或在设置中填写 adb 路径。".to_string();
    if !tried.is_empty() {
        message.push_str(" 已尝试：");
        message.push_str(&tried.join("；"));
    }
    Err(message)
}

async fn run_adb(
    adb: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<CommandOutput, CommandError> {
    let spec = CommandSpec::new(adb, args.iter().cloned());
    let first = run_command(&spec, timeout).await?;
    if first.success() || !adb_server_mismatch(&first.combined()) {
        return Ok(first);
    }
    tracing::warn!(adb = %adb.display(), "检测到 adb server 冲突，改用当前 adb 重启 server");
    let _ = run_command(
        &CommandSpec::new(adb, ["kill-server"]),
        Duration::from_secs(10),
    )
    .await;
    let _ = run_command(
        &CommandSpec::new(adb, ["start-server"]),
        Duration::from_secs(20),
    )
    .await;
    run_command(&spec, timeout).await
}

pub async fn adb_connect(adb: &Path, endpoint: &str) -> Result<String, String> {
    let output = run_adb(
        adb,
        &["connect".into(), endpoint.into()],
        Duration::from_secs(5),
    )
    .await
    .map_err(|err| err.to_string())?;
    Ok(output.combined())
}

pub async fn adb_devices(adb: &Path) -> Result<Vec<AdbDeviceLine>, String> {
    let output = run_adb(adb, &["devices".into()], Duration::from_secs(15))
        .await
        .map_err(|err| err.to_string())?;
    if !output.success() {
        return Err(format!("adb devices 失败：{}", output.combined().trim()));
    }
    Ok(parse_adb_devices(&output.stdout_str()))
}

pub async fn adb_shell(
    adb: &Path,
    serial: &str,
    script: &str,
    timeout: Duration,
) -> Result<String, String> {
    let output = run_adb(
        adb,
        &["-s".into(), serial.into(), "shell".into(), script.into()],
        timeout,
    )
    .await
    .map_err(|err| err.to_string())?;
    if !output.success() && output.stdout.is_empty() {
        return Err(format!("adb shell 失败：{}", output.combined().trim()));
    }
    Ok(output.stdout_str())
}

pub async fn adb_install(
    adb: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<CommandOutput, String> {
    run_adb(adb, args, timeout)
        .await
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_list() {
        let text = "List of devices attached\n127.0.0.1:16384\tdevice\nemulator-5554\toffline\n\n";
        let devices = parse_adb_devices(text);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "127.0.0.1:16384");
        assert_eq!(devices[0].state, "device");
        assert_eq!(devices[1].state, "offline");
    }

    #[test]
    fn version_name_is_loose() {
        let dumpsys =
            "Package [com.hypergryph.arknights] (1)\n    versionName=2.6.01\n    versionCode=123\n";
        assert_eq!(parse_version_name(dumpsys).as_deref(), Some("2.6.01"));
        assert_eq!(
            parse_version_name("Unable to find package: com.hypergryph.arknights"),
            None
        );
        let mixed = "---OFFICIAL---\nversionName=2.7.01\n---BILIBILI---\nUnable to find package: com.hypergryph.arknights.bilibili\n";
        assert_eq!(
            parse_package_inventory(mixed),
            (Some("2.7.01".into()), false)
        );
    }

    #[test]
    fn abi_and_props_and_df() {
        assert!(!abi_compatible("x86_64"));
        assert!(!abi_compatible("x86,x86_64"));
        assert!(abi_compatible("arm64-v8a"));
        assert!(abi_compatible("ARM64-V8A"));
        assert!(abi_compatible("x86_64,arm64-v8a"));
        assert!(abi_compatible("armeabi-v7a"));
        assert!(!abi_compatible(""));

        let props =
            parse_props("MuMu Player\nSPLIT\n12\nSPLIT\nx86_64,arm64-v8a\nSPLIT\narm64-v8a\n");
        assert_eq!(props.model, "MuMu Player");
        assert_eq!(props.android_version, "12");
        assert_eq!(props.abi, "x86_64,arm64-v8a");

        let df = "Filesystem 1K-blocks Used Available Use% Mounted on\n/dev/block/data 20971520 1024 17825792 10% /data\n";
        assert_eq!(parse_df_available_bytes(df), Some(17_825_792 * 1024));
        let df512 =
            "Filesystem 512-blocks Used Available Capacity Mounted on\n/data 100 10 80 20% /data\n";
        assert_eq!(parse_df_available_bytes(df512), Some(80 * 512));
    }

    #[test]
    fn classifies_install_failures() {
        use crate::error::FailureKind;
        assert_eq!(
            classify_install_output("Failure [INSTALL_FAILED_NO_MATCHING_ABIS]"),
            Some(FailureKind::NoMatchingAbi)
        );
        assert_eq!(
            classify_install_output(
                "Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: signatures do not match]"
            ),
            Some(FailureKind::SignatureConflict)
        );
        assert_eq!(
            classify_install_output("Failure [INSTALL_FAILED_INSUFFICIENT_STORAGE]"),
            Some(FailureKind::InsufficientStorage)
        );
        assert_eq!(
            classify_install_output("error: device offline"),
            Some(FailureKind::DeviceOffline)
        );
        assert_eq!(classify_install_output("Success"), None);
    }
}
