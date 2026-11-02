use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::util::{blank_to_none, nonempty, valid_endpoint};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EmulatorKind {
    Mumu,
    Ldplayer,
    #[default]
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmulatorConfig {
    pub id: String,
    pub name: String,
    pub kind: EmulatorKind,
    pub start_program: Option<String>,
    pub start_args: Vec<String>,
    pub vmindex: u32,
    pub expected_serial: Option<String>,
    pub adb_port: Option<u16>,
    pub adb_path: Option<String>,
    pub install_dir: Option<String>,
}

impl Default for EmulatorConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: "未命名模拟器".into(),
            kind: EmulatorKind::Custom,
            start_program: None,
            start_args: Vec::new(),
            vmindex: 0,
            expected_serial: None,
            adb_port: None,
            adb_path: None,
            install_dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub listen: String,
    pub adb_path: Option<String>,
    pub apk_cache_dir: Option<String>,
    pub concurrency: usize,
    pub version_cache_ttl_secs: u64,
    pub device_wait_timeout_secs: u64,
    pub install_timeout_secs: u64,
    pub auto_launch_game: bool,
    /// 空字符串或 `off` 表示关闭。示例：`6h`、`30m`、`1d`。
    pub auto_check_interval: String,
    pub auto_update_on_check: bool,
    /// 假设：非空时跳过官方入口解析，直接把该 URL 当作 APK 直链（自建镜像或测试）。
    pub apk_url_override: Option<String>,
    /// 空字符串跟随环境变量，`off` 强制直连。
    pub proxy: Option<String>,
    /// 程序自身更新间隔。空字符串或 `off` 表示关闭。
    #[serde(default = "default_app_update_interval")]
    pub app_update_interval: String,
    /// 发现新的程序构建后是否自动替换并重启。
    #[serde(default = "default_true")]
    pub auto_apply_app_update: bool,
    /// 空则使用 GitHub 最新 Release。测试可指向本机的一份相同 JSON。
    pub app_update_feed: Option<String>,
    /// 模拟器开机检测间隔。空字符串或 `off` 表示关闭。
    #[serde(default = "default_watch_interval")]
    pub emulator_watch_interval: String,
    pub extra_devices: Vec<String>,
    pub emulators: Vec<EmulatorConfig>,
}

fn default_app_update_interval() -> String {
    "6h".into()
}

fn default_watch_interval() -> String {
    "1m".into()
}

fn default_true() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8787".into(),
            adb_path: None,
            apk_cache_dir: None,
            concurrency: 3,
            version_cache_ttl_secs: 600,
            device_wait_timeout_secs: 180,
            install_timeout_secs: 1800,
            auto_launch_game: false,
            auto_check_interval: "6h".into(),
            auto_update_on_check: true,
            apk_url_override: None,
            proxy: None,
            app_update_interval: default_app_update_interval(),
            auto_apply_app_update: true,
            app_update_feed: None,
            emulator_watch_interval: default_watch_interval(),
            extra_devices: Vec::new(),
            emulators: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|err| format!("读取配置 {} 失败：{err}", path.display()))?;
        let mut config: Self = toml::from_str(&text)
            .map_err(|err| format!("解析配置 {} 失败：{err}", path.display()))?;
        config.normalize();
        config.validate()?;
        Ok(config)
    }

    pub fn normalize(&mut self) {
        self.listen = self.listen.trim().to_string();
        blank_to_none(&mut self.adb_path);
        blank_to_none(&mut self.apk_cache_dir);
        blank_to_none(&mut self.apk_url_override);
        blank_to_none(&mut self.app_update_feed);
        self.proxy = normalize_proxy(self.proxy.take());
        self.app_update_interval = self.app_update_interval.trim().to_string();
        self.emulator_watch_interval = self.emulator_watch_interval.trim().to_string();
        if self.concurrency == 0 {
            self.concurrency = 1;
        }
        self.concurrency = self.concurrency.min(16);
        if self.version_cache_ttl_secs == 0 {
            self.version_cache_ttl_secs = 600;
        }
        if self.device_wait_timeout_secs == 0 {
            self.device_wait_timeout_secs = 180;
        }
        if self.install_timeout_secs < 60 {
            self.install_timeout_secs = 1800;
        }
        self.extra_devices = self
            .extra_devices
            .iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        for emulator in &mut self.emulators {
            emulator.name = emulator.name.trim().to_string();
            if emulator.name.is_empty() {
                emulator.name = "未命名模拟器".into();
            }
            blank_to_none(&mut emulator.start_program);
            blank_to_none(&mut emulator.expected_serial);
            blank_to_none(&mut emulator.adb_path);
            blank_to_none(&mut emulator.install_dir);
            emulator.start_args = emulator
                .start_args
                .iter()
                .map(|arg| arg.trim().to_string())
                .filter(|arg| !arg.is_empty())
                .collect();
            emulator.id = emulator.id.trim().to_string();
            if emulator.id.is_empty() {
                emulator.id = stable_emulator_id(emulator);
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        ensure_loopback(&self.listen)?;
        parse_interval(&self.auto_check_interval)?;
        parse_interval(&self.app_update_interval)?;
        parse_interval(&self.emulator_watch_interval)?;
        if let Some(proxy) = &self.proxy {
            if !proxy.eq_ignore_ascii_case("off") {
                crate::http_client::validate_proxy_url(proxy)?;
            }
        }
        if let Some(feed) = &self.app_update_feed {
            crate::http_client::validate_feed_url(feed)?;
        }
        let mut seen = std::collections::HashSet::new();
        for emulator in &self.emulators {
            if !seen.insert(emulator.id.clone()) {
                return Err(format!("模拟器 id 重复：{}", emulator.id));
            }
            if emulator.kind == EmulatorKind::Custom && emulator.start_program.is_none() {
                return Err(format!(
                    "自定义模拟器「{}」必须填写启动程序和参数数组",
                    emulator.name
                ));
            }
            if let Some(serial) = &emulator.expected_serial {
                if !valid_endpoint(serial) {
                    return Err(format!("模拟器「{}」的序列号无效：{serial}", emulator.name));
                }
            }
        }
        for endpoint in &self.extra_devices {
            if !valid_endpoint(endpoint) {
                return Err(format!("手动设备地址无效：{endpoint}"));
            }
        }
        Ok(())
    }

    pub fn cache_dir(&self, data_dir: &Path) -> PathBuf {
        match nonempty(self.apk_cache_dir.as_deref()) {
            Some(path) => PathBuf::from(path),
            None => data_dir.join("cache"),
        }
    }
}

pub fn save(path: &Path, config: &AppConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("创建配置目录 {} 失败：{err}", parent.display()))?;
    }
    let body = toml::to_string_pretty(config).map_err(|err| format!("序列化配置失败：{err}"))?;
    let text = format!(
        "# ark-updater 配置。WebUI 保存时会重写本文件，注释以 config.example.toml 为准。\n\n{body}"
    );
    fs::write(path, text).map_err(|err| format!("写入配置 {} 失败：{err}", path.display()))
}

pub fn ensure_loopback(listen: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = listen.parse().map_err(|err| {
        format!("监听地址无效（需要 IP:端口，例如 127.0.0.1:8787）：{listen}（{err}）")
    })?;
    if !addr.ip().is_loopback() {
        return Err(format!("只允许监听本机回环地址，当前为 {listen}"));
    }
    Ok(addr)
}

fn normalize_proxy(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.eq_ignore_ascii_case("off") || trimmed.eq_ignore_ascii_case("direct") {
        Some("off".into())
    } else if trimmed == value {
        Some(value)
    } else {
        Some(trimmed.to_string())
    }
}

/// 解析 `6h` / `30m` / `1d` / `90s`。空字符串和 `off` 表示关闭。
pub fn parse_interval(raw: &str) -> Result<Option<Duration>, String> {
    let text = raw.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("off") || text == "0" {
        return Ok(None);
    }
    let split_at = text
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(text.len());
    if split_at == 0 {
        return Err(format!("无法解析时间间隔「{raw}」，示例：6h、30m、1d"));
    }
    let number: u64 = text[..split_at]
        .parse()
        .map_err(|_| format!("无法解析时间间隔「{raw}」"))?;
    let unit = text[split_at..].trim();
    let seconds = match unit {
        "s" | "sec" | "secs" => number,
        "m" | "min" | "mins" => number.saturating_mul(60),
        "h" | "hr" | "hour" | "hours" => number.saturating_mul(3600),
        "d" | "day" | "days" => number.saturating_mul(86_400),
        _ => return Err(format!("无法解析时间间隔「{raw}」，单位用 s/m/h/d")),
    };
    if seconds == 0 {
        Ok(None)
    } else {
        Ok(Some(Duration::from_secs(seconds)))
    }
}

fn stable_emulator_id(emulator: &EmulatorConfig) -> String {
    // 假设：用户没填 id 时，用类型、索引和名称生成稳定 id，避免每次热重载都换新 id。
    let basis = format!("{:?}-{}-{}", emulator.kind, emulator.vmindex, emulator.name);
    let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
    for byte in basis.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    format!("emu-{hash:x}")
}

pub fn resolve_config_path(cli_path: Option<&Path>) -> PathBuf {
    if let Some(path) = cli_path {
        return path.to_path_buf();
    }
    let portable = crate::util::exe_dir().join("config.toml");
    if portable.is_file() {
        portable
    } else {
        crate::platform::default_data_dir().join("config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_unattended() {
        let config = AppConfig::default();
        assert_eq!(config.auto_check_interval, "6h");
        assert!(config.auto_update_on_check);
        assert_eq!(config.app_update_interval, "6h");
        assert!(config.auto_apply_app_update);
        assert_eq!(config.emulator_watch_interval, "1m");
        assert!(config.proxy.is_none());
    }

    #[test]
    fn old_config_keeps_new_maintenance_defaults() {
        let loaded: AppConfig = toml::from_str("listen = \"127.0.0.1:8787\"\n").expect("parse");
        assert_eq!(loaded.app_update_interval, "6h");
        assert!(loaded.auto_apply_app_update);
        assert_eq!(loaded.emulator_watch_interval, "1m");
        assert!(loaded.proxy.is_none());
        assert!(loaded.app_update_feed.is_none());
    }

    #[test]
    fn rejects_bad_proxy_and_feed() {
        let mut config = AppConfig {
            proxy: Some("ftp://127.0.0.1:1".into()),
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
        config.proxy = Some("off".into());
        config.app_update_feed = Some("http://example.com/latest".into());
        assert!(config.validate().is_err());
        config.app_update_feed = Some("http://127.0.0.1:9/latest".into());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn parses_intervals() {
        assert_eq!(parse_interval("").unwrap(), None);
        assert_eq!(parse_interval("off").unwrap(), None);
        assert_eq!(
            parse_interval("6h").unwrap(),
            Some(Duration::from_secs(6 * 3600))
        );
        assert_eq!(
            parse_interval("30m").unwrap(),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(
            parse_interval("1d").unwrap(),
            Some(Duration::from_secs(86_400))
        );
        assert!(parse_interval("soon").is_err());
    }

    #[test]
    fn rejects_public_bind() {
        assert!(ensure_loopback("127.0.0.1:8787").is_ok());
        assert!(ensure_loopback("0.0.0.0:8787").is_err());
    }

    #[test]
    fn roundtrip_toml() {
        let mut config = AppConfig {
            auto_check_interval: "6h".into(),
            concurrency: 2,
            ..AppConfig::default()
        };
        config.emulators.push(EmulatorConfig {
            id: "mumu-0".into(),
            name: "MuMu 12 实例 0".into(),
            kind: EmulatorKind::Mumu,
            start_program: Some(
                r"C:\Program Files\Netease\MuMu Player 12\nx_main\mumu-cli.exe".into(),
            ),
            start_args: vec!["control".into(), "-v".into(), "0".into(), "launch".into()],
            vmindex: 0,
            expected_serial: Some("127.0.0.1:16384".into()),
            adb_port: Some(16384),
            adb_path: None,
            install_dir: None,
        });
        config.normalize();
        let text = toml::to_string_pretty(&config).expect("toml");
        let mut loaded: AppConfig = toml::from_str(&text).expect("parse");
        loaded.normalize();
        assert_eq!(loaded.auto_check_interval, "6h");
        assert!(loaded.auto_update_on_check);
        assert_eq!(loaded.concurrency, 2);
        assert_eq!(loaded.emulators.len(), 1);
        assert_eq!(loaded.emulators[0].start_args[0], "control");
        assert!(loaded.emulators[0]
            .start_program
            .as_ref()
            .unwrap()
            .contains("Program Files"));
    }
}
