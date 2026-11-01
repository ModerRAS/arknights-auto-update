use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::adb::{
    abi_compatible, adb_connect, adb_devices, adb_shell, discover_adb, inventory_script,
    parse_df_available_bytes, parse_package_inventory, parse_props, probe_adb, props_script,
    storage_script, AdbInfo, COMMON_ADB_PORTS,
};
use crate::config::{save, AppConfig, EmulatorConfig};
use crate::download::{find_cached_apk, obtain_apk, resolve_apk_url, REQUIRED_FREE_BYTES};
use crate::emulator::{emulator_adb_override, expected_serial, launch_emulator, wait_until_online};
use crate::error::UpdateError;
use crate::installer::{install_apk, launch_game};
use crate::state::{
    file_mtime, merge_device_rows, AdbStatus, AppState, CachedVersion, DeviceSnapshot,
    DownloadPhase, DownloadSnapshot, OfficialSnapshot, SharedState, TaskPhase, TaskView,
};
use crate::util::{format_bytes, nonempty, percent};
use crate::version::{
    parse_official_version, version_is_newer, OfficialVersion, PACKAGE_BILIBILI, VERSION_URL,
};

#[derive(Debug, Clone, Default)]
pub enum DeviceSelector {
    #[default]
    All,
    Only(Vec<String>),
}

impl<'de> Deserialize<'de> for DeviceSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(text) if text == "all" => Ok(Self::All),
            serde_json::Value::Array(items) => {
                let mut serials = Vec::new();
                for item in items {
                    let Some(serial) = item.as_str() else {
                        return Err(serde::de::Error::custom("设备序列号必须是字符串"));
                    };
                    serials.push(serial.to_string());
                }
                Ok(Self::Only(serials))
            }
            _ => Err(serde::de::Error::custom(
                "devices 需要是 \"all\" 或序列号数组",
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpdateRequest {
    pub devices: DeviceSelector,
    pub force: bool,
}

#[derive(Clone)]
struct Target {
    serial: String,
    online: bool,
    emulator: Option<crate::config::EmulatorConfig>,
    adb_path: String,
}

struct QueriedDevice {
    model: String,
    android_version: String,
    abi: String,
    installed: Option<String>,
    bilibili_installed: bool,
    free_bytes: Option<u64>,
}

pub enum Decision {
    UpToDate,
    NeedInstall,
    Reject(UpdateError),
}

pub struct DecideInput<'a> {
    pub installed: Option<&'a str>,
    pub official: &'a str,
    pub abi: &'a str,
    pub free_bytes: Option<u64>,
    pub force: bool,
}

/// 已是最新时直接返回，避免对无需安装的设备误报 ABI。
/// 需要安装时先做 ABI，再做空间，通过后才允许下载。
pub fn decide(input: DecideInput<'_>) -> Decision {
    let needs_install = input.force
        || match input.installed {
            None => true,
            Some(installed) => version_is_newer(input.official, installed),
        };
    if !needs_install {
        return Decision::UpToDate;
    }
    if !abi_compatible(input.abi) {
        return Decision::Reject(UpdateError::NoMatchingAbi {
            abi: input.abi.to_string(),
        });
    }
    if let Some(free) = input.free_bytes {
        if free < REQUIRED_FREE_BYTES {
            return Decision::Reject(UpdateError::InsufficientStorage {
                detail: format!("设备可用 {}", format_bytes(free)),
            });
        }
    }
    Decision::NeedInstall
}

pub async fn note_config_mtime(state: &AppState) {
    *state.config_mtime.lock().await = file_mtime(&state.paths.config_path);
}

pub async fn persist_config(state: &AppState, config: &AppConfig) -> Result<(), String> {
    let _guard = state.save_lock.lock().await;
    save(&state.paths.config_path, config)?;
    *state.config_mtime.lock().await = file_mtime(&state.paths.config_path);
    Ok(())
}

pub async fn watch_config(state: SharedState) {
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mtime = file_mtime(&state.paths.config_path);
        let known = *state.config_mtime.lock().await;
        if mtime.is_some() && mtime != known {
            match AppConfig::load(&state.paths.config_path) {
                Ok(config) => {
                    *state.config.write().await = config;
                    *state.config_mtime.lock().await = mtime;
                    state.reload_concurrency().await;
                    if let Err(err) = refresh_adb_status(&state).await {
                        tracing::warn!(error = %err, "热重载后的 adb 不可用");
                    }
                    tracing::info!("配置已热重载");
                }
                Err(err) => {
                    tracing::warn!(error = %err, "配置热重载失败，继续使用内存中的配置");
                }
            }
        }
    }
}

const AUTO_STARTUP_GRACE: Duration = Duration::from_secs(8);

pub async fn auto_check_loop(state: SharedState) {
    let mut armed: Option<Duration> = None;
    let mut next_run: Option<Instant> = None;
    let mut message = "正在安排自动更新".to_string();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let config = state.config.read().await.clone();
        let interval = match crate::config::parse_interval(&config.auto_check_interval) {
            Ok(interval) => interval,
            Err(err) => {
                tracing::warn!(error = %err, "自动检查间隔无效");
                message = format!("自动检查间隔无效：{err}");
                publish_auto(&state, &config, None, message.clone()).await;
                armed = None;
                next_run = None;
                continue;
            }
        };
        let Some(interval) = interval else {
            armed = None;
            next_run = None;
            message = "自动更新已关闭。把间隔设为 6h 并勾选自动安装后，程序会自己完成更新。".into();
            publish_auto(&state, &config, None, message.clone()).await;
            continue;
        };
        if armed != Some(interval) {
            let first = armed.is_none();
            armed = Some(interval);
            next_run = Some(Instant::now() + if first { AUTO_STARTUP_GRACE } else { interval });
            if first {
                message = "无人值守已开启。启动后会自己检查并安装，不用点按钮。".into();
            }
        }
        let due = next_run.unwrap_or_else(Instant::now);
        publish_auto(&state, &config, Some(due), message.clone()).await;
        if Instant::now() < due {
            continue;
        }
        if update_busy(&state).await {
            next_run = Some(Instant::now() + Duration::from_secs(60));
            message = "已有更新在进行，一分钟后再自动检查。".into();
            continue;
        }
        next_run = Some(Instant::now() + interval);
        message = run_auto_cycle(&state).await;
    }
}

async fn publish_auto(
    state: &AppState,
    config: &AppConfig,
    next_run: Option<Instant>,
    message: String,
) {
    let enabled = crate::config::parse_interval(&config.auto_check_interval)
        .ok()
        .flatten()
        .is_some();
    *state.auto.write().await = crate::state::AutoStatus {
        enabled,
        install: config.auto_update_on_check,
        interval: config.auto_check_interval.clone(),
        next_run_ms: next_run.map(instant_to_unix_ms),
        message,
    };
}

fn instant_to_unix_ms(at: Instant) -> u64 {
    let now = Instant::now();
    let now_ms = crate::util::now_ms();
    let delta = if at >= now {
        u64::try_from((at - now).as_millis()).unwrap_or(u64::MAX)
    } else {
        u64::try_from((now - at).as_millis()).unwrap_or(0)
    };
    if at >= now {
        now_ms.saturating_add(delta)
    } else {
        now_ms.saturating_sub(delta)
    }
}

async fn update_busy(state: &AppState) -> bool {
    let devices_busy = state
        .devices
        .read()
        .await
        .iter()
        .any(|device| device.task.phase.is_active());
    if devices_busy {
        return true;
    }
    matches!(
        state.download.read().await.phase,
        DownloadPhase::Resolving | DownloadPhase::Downloading | DownloadPhase::Verifying
    )
}

async fn run_auto_cycle(state: &SharedState) -> String {
    let config = state.config.read().await.clone();
    let version = match check_official(state, true).await {
        Ok(version) => version,
        Err(err) => {
            tracing::warn!(error = %err, "自动检查失败");
            return format!("自动检查失败：{err}");
        }
    };
    tracing::info!(client = %version.client_version, res = %version.res_version, "自动检查完成");
    if !config.auto_update_on_check {
        return format!(
            "已看到官方 {}。自动安装是关的，所以不会自己装。",
            version.client_version
        );
    }
    if let Err(err) = refresh_devices(state).await {
        tracing::warn!(error = %err, "自动更新前扫描设备失败");
    }
    let known = state.known_versions.lock().await.clone();
    let devices = state.devices.read().await.clone();
    match plan_unattended(
        &version.client_version,
        &devices,
        &config.emulators,
        &config.extra_devices,
        &known,
    ) {
        UnattendedPlan::Skip(message) => {
            tracing::info!(%message, "本轮不拉起模拟器");
            message
        }
        UnattendedPlan::Update(serials) => {
            let count = serials.len();
            let request = UpdateRequest {
                devices: DeviceSelector::Only(serials),
                force: false,
            };
            match start_update(state, request).await {
                Ok(()) => {
                    tracing::info!(count, "已开始无人值守更新");
                    format!(
                        "已开始更新 {count} 台设备，官方版本 {}",
                        version.client_version
                    )
                }
                Err(err) => {
                    tracing::warn!(error = %err, "无人值守更新未启动");
                    format!("没能开始自动更新：{err}")
                }
            }
        }
    }
}

pub enum UnattendedPlan {
    Skip(String),
    Update(Vec<String>),
}

/// 在线设备用刚读到的版本；离线模拟器用上次成功记录的版本。
/// 已经不落后于官方 clientVersion 的模拟器不会被拉起。
pub fn plan_unattended(
    official: &str,
    devices: &[DeviceSnapshot],
    emulators: &[EmulatorConfig],
    extra_devices: &[String],
    known: &HashMap<String, String>,
) -> UnattendedPlan {
    let mut serials = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for device in devices.iter().filter(|device| device.online) {
        if installed_is_current(official, device.installed_version.as_deref()) {
            continue;
        }
        if seen.insert(device.serial.clone()) {
            serials.push(device.serial.clone());
        }
    }
    let mut offline_emulator = false;
    for emulator in emulators {
        let Some(serial) = expected_serial(emulator) else {
            continue;
        };
        if devices
            .iter()
            .any(|device| device.online && device.serial == serial)
        {
            continue;
        }
        offline_emulator = true;
        let recorded = known.get(&serial).map(String::as_str);
        if installed_is_current(official, recorded) {
            continue;
        }
        if seen.insert(serial.clone()) {
            serials.push(serial);
        }
    }
    if !serials.is_empty() {
        return UnattendedPlan::Update(serials);
    }
    let has_online = devices.iter().any(|device| device.online);
    if !offline_emulator && !has_online {
        if extra_devices.is_empty() {
            return UnattendedPlan::Skip(
                "还没有模拟器或在线设备。在设置里添加一次模拟器后，程序会自己拉起并安装。".into(),
            );
        }
        return UnattendedPlan::Skip("手动添加的设备不在线，而且没有可拉起的模拟器。".into());
    }
    UnattendedPlan::Skip(format!(
        "已是最新的 clientVersion {official}，本轮不拉起模拟器。"
    ))
}

fn installed_is_current(official: &str, installed: Option<&str>) -> bool {
    match installed {
        Some(version) => !version_is_newer(official, version),
        None => false,
    }
}

pub async fn check_official(state: &AppState, force: bool) -> Result<OfficialVersion, String> {
    if !force {
        let ttl = state.config.read().await.version_cache_ttl_secs;
        let cached = state.version_cache.read().await.clone_version();
        if let Some((version, fetched_at)) = cached {
            if fetched_at.elapsed() < Duration::from_secs(ttl) {
                publish_official(state, &version, true, None).await;
                return Ok(version);
            }
        }
    }
    match fetch_official(&state.version_client).await {
        Ok(version) => {
            *state.version_cache.write().await = Some(CachedVersion {
                version: version.clone(),
                fetched_at: Instant::now(),
            });
            publish_official(state, &version, false, None).await;
            tracing::info!(
                client = %version.client_version,
                res = %version.res_version,
                "已获取官方版本"
            );
            Ok(version)
        }
        Err(err) => {
            let mut snapshot = state.official.read().await.clone();
            snapshot.error = Some(err.clone());
            *state.official.write().await = snapshot;
            Err(err)
        }
    }
}

trait CacheView {
    fn clone_version(&self) -> Option<(OfficialVersion, Instant)>;
}

impl CacheView for Option<CachedVersion> {
    fn clone_version(&self) -> Option<(OfficialVersion, Instant)> {
        self.as_ref()
            .map(|item| (item.version.clone(), item.fetched_at))
    }
}

async fn publish_official(
    state: &AppState,
    version: &OfficialVersion,
    cached: bool,
    error: Option<String>,
) {
    *state.official.write().await = OfficialSnapshot {
        client_version: Some(version.client_version.clone()),
        res_version: Some(version.res_version.clone()),
        checked_at_ms: Some(crate::util::now_ms()),
        error,
        cached,
    };
}

async fn fetch_official(client: &reqwest::Client) -> Result<OfficialVersion, String> {
    let response = client
        .get(VERSION_URL)
        .send()
        .await
        .map_err(|err| format!("请求官方版本失败：{err}"))?;
    if !response.status().is_success() {
        return Err(format!("官方版本接口返回 HTTP {}", response.status()));
    }
    let text = response
        .text()
        .await
        .map_err(|err| format!("读取官方版本失败：{err}"))?;
    parse_official_version(&text)
}

pub async fn refresh_adb_status(state: &SharedState) -> Result<AdbInfo, String> {
    let config = state.config.read().await.clone();
    match discover_adb(&config).await {
        Ok(info) => {
            *state.adb.write().await = AdbStatus {
                path: Some(info.path.display().to_string()),
                version: Some(info.version.clone()),
                source: info.source.clone(),
                error: None,
            };
            Ok(info)
        }
        Err(err) => {
            let source = if config.adb_path.is_some() {
                "配置文件"
            } else {
                "自动发现"
            };
            *state.adb.write().await = AdbStatus {
                path: config.adb_path.clone(),
                version: None,
                source: source.into(),
                error: Some(err.clone()),
            };
            tracing::warn!(error = %err, "adb 不可用");
            Err(err)
        }
    }
}

pub async fn refresh_devices(state: &SharedState) -> Result<(), String> {
    let config = state.config.read().await.clone();
    let global = match refresh_adb_status(state).await {
        Ok(info) => Some(info),
        Err(err) => {
            let old = state.devices.read().await.clone();
            *state.devices.write().await = merge_device_rows(&old, Vec::new());
            return Err(err);
        }
    };

    let mut bins: Vec<(AdbInfo, Vec<String>)> = Vec::new();
    if let Some(info) = global {
        let mut endpoints = common_endpoints();
        endpoints.extend(config.extra_devices.iter().cloned());
        for emulator in &config.emulators {
            if emulator_adb_override(emulator).is_some() {
                continue;
            }
            if let Some(serial) = expected_serial(emulator) {
                push_unique(&mut endpoints, serial);
            }
        }
        bins.push((info, endpoints));
    }
    for emulator in &config.emulators {
        let Some(path) = emulator_adb_override(emulator) else {
            continue;
        };
        match probe_adb(&path, "模拟器专用").await {
            Ok(info) => {
                let endpoints = expected_serial(emulator).into_iter().collect();
                bins.push((info, endpoints));
            }
            Err(err) => tracing::warn!(
                emulator = %emulator.name,
                error = %err,
                "模拟器专用 adb 不可用，该实例改走全局 adb"
            ),
        }
    }

    let mut owners: Vec<(String, AdbInfo)> = Vec::new();
    for (info, endpoints) in &bins {
        connect_many(&info.path, endpoints).await;
        match adb_devices(&info.path).await {
            Ok(devices) => {
                for device in devices.into_iter().filter(|item| item.state == "device") {
                    if let Some(existing) = owners
                        .iter_mut()
                        .find(|(serial, _)| serial == &device.serial)
                    {
                        existing.1 = info.clone();
                    } else {
                        owners.push((device.serial, info.clone()));
                    }
                }
            }
            Err(err) => tracing::warn!(adb = %info.path.display(), error = %err, "枚举设备失败"),
        }
    }

    let limit = config.concurrency.clamp(1, 16);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(limit));
    let mut handles = Vec::new();
    for (serial, info) in owners {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|err| err.to_string())?;
        let state = Arc::clone(state);
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let lock = state.device_lock(&serial);
            let _guard = lock.lock().await;
            match query_device(&info.path, &serial).await {
                Ok(queried) => Some(snapshot_from_query(&serial, &info, &queried)),
                Err(err) => {
                    tracing::warn!(serial, error = %err, "读取设备信息失败");
                    None
                }
            }
        }));
    }
    let mut scanned = Vec::new();
    for handle in handles {
        if let Ok(Some(row)) = handle.await {
            scanned.push(row);
        }
    }
    attach_configured_rows(&config, &mut scanned);
    let old = state.devices.read().await.clone();
    *state.devices.write().await = merge_device_rows(&old, scanned);
    Ok(())
}

fn common_endpoints() -> Vec<String> {
    COMMON_ADB_PORTS
        .iter()
        .map(|port| format!("127.0.0.1:{port}"))
        .collect()
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.iter().any(|item| item == &value) {
        list.push(value);
    }
}

async fn connect_many(adb: &Path, endpoints: &[String]) {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let mut handles = Vec::new();
    for endpoint in endpoints {
        let adb = adb.to_path_buf();
        let endpoint = endpoint.clone();
        let semaphore = semaphore.clone();
        handles.push(tokio::spawn(async move {
            let Ok(_permit) = semaphore.acquire().await else {
                return;
            };
            let _ = adb_connect(&adb, &endpoint).await;
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
}

async fn query_device(adb: &Path, serial: &str) -> Result<QueriedDevice, String> {
    let props_raw = adb_shell(adb, serial, props_script(), Duration::from_secs(25)).await?;
    let props = parse_props(&props_raw);
    let inventory = adb_shell(adb, serial, &inventory_script(), Duration::from_secs(45)).await?;
    let (installed, bilibili_installed) = parse_package_inventory(&inventory);
    let free_bytes = match adb_shell(adb, serial, storage_script(), Duration::from_secs(20)).await {
        Ok(text) => parse_df_available_bytes(&text),
        Err(err) => {
            tracing::warn!(serial, error = %err, "读取设备剩余空间失败");
            None
        }
    };
    Ok(QueriedDevice {
        model: props.model,
        android_version: props.android_version,
        abi: props.abi,
        installed,
        bilibili_installed,
        free_bytes,
    })
}

fn snapshot_from_query(serial: &str, adb: &AdbInfo, queried: &QueriedDevice) -> DeviceSnapshot {
    DeviceSnapshot {
        serial: serial.to_string(),
        model: queried.model.clone(),
        android_version: queried.android_version.clone(),
        abi: queried.abi.clone(),
        abi_compatible: abi_compatible(&queried.abi),
        installed_label: queried.installed.clone().unwrap_or_else(|| "未安装".into()),
        installed_version: queried.installed.clone(),
        bilibili_installed: queried.bilibili_installed,
        online: true,
        emulator_id: None,
        emulator_name: None,
        adb_path: adb.path.display().to_string(),
        task: TaskView::idle(),
    }
}

fn attach_configured_rows(config: &AppConfig, rows: &mut Vec<DeviceSnapshot>) {
    attach_emulator_metadata(config, rows);
    for endpoint in &config.extra_devices {
        if rows.iter().any(|item| item.serial == endpoint.as_str()) {
            continue;
        }
        let mut placeholder = DeviceSnapshot::placeholder(endpoint, TaskView::idle());
        placeholder.model = "未连接".into();
        placeholder.installed_label = "未知".into();
        rows.push(placeholder);
    }
}

fn attach_emulator_metadata(config: &AppConfig, rows: &mut Vec<DeviceSnapshot>) {
    for emulator in &config.emulators {
        let Some(serial) = expected_serial(emulator) else {
            continue;
        };
        if let Some(row) = rows.iter_mut().find(|item| item.serial == serial) {
            row.emulator_id = Some(emulator.id.clone());
            row.emulator_name = Some(emulator.name.clone());
            continue;
        }
        let mut placeholder = DeviceSnapshot::placeholder(&serial, TaskView::idle());
        placeholder.model = "未启动".into();
        placeholder.emulator_id = Some(emulator.id.clone());
        placeholder.emulator_name = Some(emulator.name.clone());
        placeholder.installed_label = "未知".into();
        rows.push(placeholder);
    }
}

fn target_for(config: &AppConfig, devices: &[DeviceSnapshot], serial: &str) -> Target {
    let device = devices.iter().find(|item| item.serial == serial);
    let emulator = config.emulators.iter().find(|emulator| {
        expected_serial(emulator).as_deref() == Some(serial) || emulator.id == serial
    });
    let serial = device
        .map(|item| item.serial.clone())
        .or_else(|| emulator.and_then(expected_serial))
        .unwrap_or_else(|| serial.to_string());
    Target {
        online: device.is_some_and(|item| item.online),
        adb_path: device.map(|item| item.adb_path.clone()).unwrap_or_default(),
        emulator: emulator.cloned(),
        serial,
    }
}

async fn resolve_targets(state: &AppState, request: &UpdateRequest) -> Result<Vec<Target>, String> {
    let config = state.config.read().await.clone();
    let devices = state.devices.read().await.clone();
    let mut targets: Vec<Target> = Vec::new();
    match &request.devices {
        DeviceSelector::Only(list) => {
            if list.is_empty() {
                return Err("没有选择设备".into());
            }
            for serial in list {
                let target = target_for(&config, &devices, serial);
                if !targets.iter().any(|item| item.serial == target.serial) {
                    targets.push(target);
                }
            }
        }
        DeviceSelector::All => {
            for device in devices.iter().filter(|item| item.online) {
                targets.push(target_for(&config, &devices, &device.serial));
            }
            for emulator in &config.emulators {
                let Some(serial) = expected_serial(emulator) else {
                    continue;
                };
                if targets.iter().any(|item| item.serial == serial) {
                    continue;
                }
                targets.push(target_for(&config, &devices, &serial));
            }
        }
    }
    if targets.is_empty() {
        return Err("没有在线设备，也没有已配置的模拟器".into());
    }
    Ok(targets)
}

pub async fn start_update(state: &SharedState, request: UpdateRequest) -> Result<(), String> {
    let official = check_official(state, false).await?;
    let targets = resolve_targets(state, &request).await?;
    let token = CancellationToken::new();
    if let Some(previous) = state.update_cancel.lock().await.replace(token.clone()) {
        previous.cancel();
    }
    let force = request.force;
    let state = Arc::clone(state);
    tokio::spawn(async move {
        run_batch(state, targets, official, force, token).await;
    });
    Ok(())
}

pub async fn cancel_all(state: &AppState) {
    if let Some(token) = state.update_cancel.lock().await.take() {
        token.cancel();
    }
    {
        let mut devices = state.devices.write().await;
        for device in devices.iter_mut() {
            if device.task.phase.is_active() {
                device.task = TaskView::phase(TaskPhase::Cancelled, "已取消");
                device.task.failure = Some(crate::error::FailureKind::Cancelled);
            }
        }
    }
    let download = state.download.read().await.clone();
    if matches!(
        download.phase,
        DownloadPhase::Resolving | DownloadPhase::Downloading | DownloadPhase::Verifying
    ) {
        let mut next = download;
        next.phase = DownloadPhase::Failed;
        next.error = Some("已取消".into());
        state.set_download(next).await;
    }
    tracing::info!("已取消全部更新任务");
}

async fn run_batch(
    state: SharedState,
    targets: Vec<Target>,
    official: OfficialVersion,
    force: bool,
    token: CancellationToken,
) {
    let semaphore = state.concurrency.read().await.clone();
    let mut handles = Vec::new();
    for target in targets {
        if token.is_cancelled() {
            break;
        }
        let permit = tokio::select! {
            _ = token.cancelled() => break,
            result = semaphore.clone().acquire_owned() => result,
        };
        let Ok(permit) = permit else {
            tracing::error!("并发信号量已关闭");
            break;
        };
        let state = Arc::clone(&state);
        let official = official.clone();
        let token = token.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            update_one(state, target, official, force, token).await;
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
}

async fn resolve_target_adb(state: &AppState, target: &Target) -> Result<AdbInfo, UpdateError> {
    if let Some(emulator) = &target.emulator {
        if let Some(path) = emulator_adb_override(emulator) {
            return probe_adb(&path, "模拟器专用")
                .await
                .map_err(UpdateError::AdbError);
        }
    }
    if !target.adb_path.is_empty() {
        if let Ok(info) = probe_adb(Path::new(&target.adb_path), "设备记录").await {
            return Ok(info);
        }
    }
    let config = state.config.read().await.clone();
    discover_adb(&config).await.map_err(UpdateError::AdbError)
}

async fn update_one(
    state: SharedState,
    target: Target,
    official: OfficialVersion,
    force: bool,
    token: CancellationToken,
) {
    let original_serial = target.serial.clone();
    if token.is_cancelled() {
        state
            .set_device_task(
                &original_serial,
                TaskView::phase(TaskPhase::Cancelled, "已取消"),
            )
            .await;
        return;
    }
    let adb = match resolve_target_adb(&state, &target).await {
        Ok(info) => info,
        Err(err) => {
            state
                .set_device_task(&original_serial, TaskView::failed(&err))
                .await;
            return;
        }
    };

    let serial = if target.online {
        target.serial.clone()
    } else {
        let Some(emulator) = target.emulator.clone() else {
            let err = UpdateError::DeviceOffline(format!(
                "{original_serial} 不在线，且没有对应的模拟器启动项"
            ));
            state
                .set_device_task(&original_serial, TaskView::failed(&err))
                .await;
            return;
        };
        state
            .set_device_task(
                &original_serial,
                TaskView::phase(
                    TaskPhase::WaitingOnline,
                    format!("正在拉起 {}", emulator.name),
                ),
            )
            .await;
        if let Err(err) = launch_emulator(&emulator).await {
            state
                .set_device_task(&original_serial, TaskView::failed(&err))
                .await;
            return;
        }
        let wait = Duration::from_secs(state.config.read().await.device_wait_timeout_secs);
        match wait_until_online(&adb, &emulator, wait, &token).await {
            Ok(serial) => serial,
            Err(err) => {
                state
                    .set_device_task(&original_serial, TaskView::failed(&err))
                    .await;
                return;
            }
        }
    };

    state
        .set_device_task(
            &serial,
            TaskView::phase(TaskPhase::Checking, "正在检查版本、ABI 和剩余空间"),
        )
        .await;
    let queried = {
        let lock = state.device_lock(&serial);
        let _guard = lock.lock().await;
        query_device(&adb.path, &serial).await
    };
    let queried = match queried {
        Ok(queried) => queried,
        Err(err) => {
            state
                .set_device_task(&serial, TaskView::failed(&UpdateError::AdbError(err)))
                .await;
            return;
        }
    };
    remember_query(&state, &serial, &adb, &queried).await;

    match decide(DecideInput {
        installed: queried.installed.as_deref(),
        official: &official.client_version,
        abi: &queried.abi,
        free_bytes: queried.free_bytes,
        force,
    }) {
        Decision::UpToDate => {
            state
                .set_device_task(
                    &serial,
                    TaskView::phase(
                        TaskPhase::Succeeded,
                        format!("已是最新版本 {}", official.client_version),
                    ),
                )
                .await;
            return;
        }
        Decision::Reject(err) => {
            tracing::info!(serial = %serial, error = %err, "预检未通过，未开始下载");
            state.set_device_task(&serial, TaskView::failed(&err)).await;
            return;
        }
        Decision::NeedInstall => {
            if queried.free_bytes.is_none() {
                tracing::warn!(serial = %serial, "未能解析 df，继续更新");
            }
        }
    }

    let current = state.download.read().await.percent;
    let mut downloading = TaskView::phase(TaskPhase::Downloading, "等待共享下载");
    downloading.percent = current;
    state.set_device_task(&serial, downloading).await;

    let apk = match ensure_shared_apk(&state, &official, token.clone()).await {
        Ok(apk) => apk,
        Err(err) => {
            state.set_device_task(&serial, TaskView::failed(&err)).await;
            return;
        }
    };
    if token.is_cancelled() {
        state
            .set_device_task(&serial, TaskView::phase(TaskPhase::Cancelled, "已取消"))
            .await;
        return;
    }

    state
        .set_device_task(&serial, TaskView::phase(TaskPhase::Installing, "正在安装"))
        .await;
    let timeout = Duration::from_secs(state.config.read().await.install_timeout_secs);
    let installed = {
        let lock = state.device_lock(&serial);
        let _guard = lock.lock().await;
        install_apk(
            &adb.path,
            &serial,
            &apk.path,
            target.emulator.as_ref(),
            timeout,
            force,
        )
        .await
    };
    match installed {
        Ok(method) => {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let version_now = {
                let lock = state.device_lock(&serial);
                let _guard = lock.lock().await;
                adb_shell(
                    &adb.path,
                    &serial,
                    &inventory_script(),
                    Duration::from_secs(45),
                )
                .await
                .ok()
                .and_then(|text| parse_package_inventory(&text).0)
            };
            if let Some(found) = version_now.as_ref() {
                if found != &official.client_version
                    && version_is_newer(&official.client_version, found)
                {
                    let err = UpdateError::InstallFailed(format!(
                        "安装命令已返回成功（{}），但读到的版本仍是 {found}，官方为 {}",
                        method.label(),
                        official.client_version
                    ));
                    state.set_device_task(&serial, TaskView::failed(&err)).await;
                    return;
                }
            }
            if state.config.read().await.auto_launch_game {
                state
                    .set_device_task(
                        &serial,
                        TaskView::phase(TaskPhase::Launching, "正在启动游戏"),
                    )
                    .await;
                if let Err(err) = launch_game(&adb.path, &serial, target.emulator.as_ref()).await {
                    tracing::warn!(serial = %serial, error = %err, "安装已完成，但启动游戏失败");
                }
            }
            {
                let mut devices = state.devices.write().await;
                if let Some(device) = devices.iter_mut().find(|item| item.serial == serial) {
                    device.online = true;
                    device.installed_version = version_now
                        .clone()
                        .or_else(|| Some(official.client_version.clone()));
                    device.installed_label = device
                        .installed_version
                        .clone()
                        .unwrap_or_else(|| "未安装".into());
                    device.adb_path = adb.path.display().to_string();
                }
            }
            let label = version_now.unwrap_or_else(|| official.client_version.clone());
            state.note_installed(&serial, &label).await;
            let mut done = TaskView::phase(
                TaskPhase::Succeeded,
                format!("已安装 {label}（{}）", method.label()),
            );
            done.percent = 100.0;
            state.set_device_task(&serial, done).await;
            tracing::info!(serial = %serial, method = method.label(), "安装完成");
        }
        Err(err) => {
            let err = enrich_signature(err, queried.bilibili_installed);
            state.set_device_task(&serial, TaskView::failed(&err)).await;
        }
    }
}

fn enrich_signature(err: UpdateError, bilibili_installed: bool) -> UpdateError {
    match err {
        UpdateError::SignatureConflict { detail } if bilibili_installed => UpdateError::SignatureConflict {
            detail: format!(
                "{detail} 另外检测到 B 服包 {PACKAGE_BILIBILI}。它和官服包名不同，通常可以共存；本次失败是官服签名不一致，请手动卸载官服后再装。"
            ),
        },
        other => other,
    }
}

async fn remember_query(state: &AppState, serial: &str, adb: &AdbInfo, queried: &QueriedDevice) {
    let installed = queried.installed.clone();
    {
        let mut devices = state.devices.write().await;
        let task = devices
            .iter()
            .find(|item| item.serial == serial)
            .map(|item| item.task.clone())
            .unwrap_or_else(TaskView::idle);
        let mut row = snapshot_from_query(serial, adb, queried);
        row.task = task;
        if let Some(existing) = devices.iter_mut().find(|item| item.serial == serial) {
            row.emulator_id = existing.emulator_id.clone();
            row.emulator_name = existing.emulator_name.clone();
            *existing = row;
        } else {
            devices.push(row);
        }
    }
    if let Some(version) = installed {
        state.note_installed(serial, &version).await;
    }
}

async fn ensure_shared_apk(
    state: &SharedState,
    version: &OfficialVersion,
    cancel: CancellationToken,
) -> Result<crate::download::ApkArtifact, UpdateError> {
    let config = state.config.read().await.clone();
    let cache = state.cache_dir(&config);
    if let Some(hit) = find_cached_apk(&cache, &version.client_version).await {
        state.set_download(DownloadSnapshot::ready(&hit)).await;
        return Ok(hit);
    }
    let override_url = config.apk_url_override.clone();
    let key = version.client_version.clone();
    let job_state = Arc::clone(state);
    let job_version = version.clone();
    let result = state
        .flight
        .run(&key, move || {
            let state = job_state;
            let version = job_version;
            let cancel = cancel.clone();
            async move { download_leader(state, version, override_url, cancel).await }
        })
        .await;
    match result {
        Ok(artifact) => {
            state.set_download(DownloadSnapshot::ready(&artifact)).await;
            Ok(artifact)
        }
        Err(message) => {
            let err = UpdateError::from_download(message);
            let mut snapshot = DownloadSnapshot {
                version: Some(version.client_version.clone()),
                error: Some(err.to_string()),
                ..DownloadSnapshot::default()
            };
            snapshot.phase = if matches!(err, UpdateError::Cancelled) {
                DownloadPhase::Idle
            } else {
                DownloadPhase::Failed
            };
            state.set_download(snapshot).await;
            Err(err)
        }
    }
}

async fn download_leader(
    state: SharedState,
    version: OfficialVersion,
    override_url: Option<String>,
    cancel: CancellationToken,
) -> Result<crate::download::ApkArtifact, String> {
    if cancel.is_cancelled() {
        return Err("已取消".into());
    }
    state
        .set_download(DownloadSnapshot {
            phase: DownloadPhase::Resolving,
            version: Some(version.client_version.clone()),
            ..DownloadSnapshot::default()
        })
        .await;
    let url = if let Some(url) = nonempty(override_url.as_deref()) {
        url
    } else {
        resolve_apk_url(&state.download_client).await?
    };
    let config = state.config.read().await.clone();
    let cache = state.cache_dir(&config);
    let progress_state = Arc::clone(&state);
    let progress_version = version.client_version.clone();
    obtain_apk(
        &state.download_client,
        &url,
        &cache,
        &version.client_version,
        &cancel,
        move |done, total| {
            let state = Arc::clone(&progress_state);
            let version = progress_version.clone();
            tokio::spawn(async move {
                publish_download_progress(state, version, done, total).await;
            });
        },
    )
    .await
}

async fn publish_download_progress(
    state: SharedState,
    version: String,
    done: u64,
    total: Option<u64>,
) {
    let snapshot = DownloadSnapshot {
        phase: DownloadPhase::Downloading,
        version: Some(version),
        path: None,
        bytes: done,
        total,
        percent: percent(done, total),
        error: None,
    };
    {
        let mut devices = state.devices.write().await;
        for device in devices.iter_mut() {
            if device.task.phase == TaskPhase::Downloading {
                device.task.percent = snapshot.percent;
                device.task.message = match total {
                    Some(total) => {
                        format!("下载中 {} / {}", format_bytes(done), format_bytes(total))
                    }
                    None => format!("下载中 {}", format_bytes(done)),
                };
            }
        }
    }
    state.set_download(snapshot).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FailureKind;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[test]
    fn unattended_plan_skips_current_emulator_and_updates_unknown() {
        let current = emulator("127.0.0.1:16384");
        let unknown = emulator("127.0.0.1:16416");
        let mut known = HashMap::new();
        known.insert("127.0.0.1:16384".into(), "2.7.91".into());
        match plan_unattended("2.7.91", &[], &[current, unknown], &[], &known) {
            UnattendedPlan::Update(serials) => {
                assert_eq!(serials, vec!["127.0.0.1:16416".to_string()]);
            }
            UnattendedPlan::Skip(message) => panic!("不应跳过：{message}"),
        }
    }

    #[test]
    fn unattended_plan_uses_live_version_for_online_devices() {
        let mut behind = DeviceSnapshot::placeholder("127.0.0.1:5555", TaskView::idle());
        behind.online = true;
        behind.installed_version = Some("2.7.80".into());
        let mut current = DeviceSnapshot::placeholder("127.0.0.1:5557", TaskView::idle());
        current.online = true;
        current.installed_version = Some("2.7.91".into());
        match plan_unattended("2.7.91", &[behind, current], &[], &[], &HashMap::new()) {
            UnattendedPlan::Update(serials) => {
                assert_eq!(serials, vec!["127.0.0.1:5555".to_string()]);
            }
            UnattendedPlan::Skip(message) => panic!("不应跳过：{message}"),
        }
    }

    #[test]
    fn unattended_plan_without_targets_asks_for_an_emulator() {
        match plan_unattended("2.7.91", &[], &[], &[], &HashMap::new()) {
            UnattendedPlan::Skip(message) => assert!(message.contains("添加一次模拟器")),
            UnattendedPlan::Update(_) => panic!("没有目标时不应更新"),
        }
    }

    fn emulator(serial: &str) -> EmulatorConfig {
        EmulatorConfig {
            id: serial.into(),
            name: serial.into(),
            kind: crate::config::EmulatorKind::Custom,
            start_program: Some("emulator.exe".into()),
            start_args: vec!["launch".into()],
            vmindex: 0,
            expected_serial: Some(serial.into()),
            adb_port: None,
            adb_path: None,
            install_dir: None,
        }
    }

    #[test]
    fn offline_manual_device_stays_visible() {
        let mut config = AppConfig::default();
        config.extra_devices.push("127.0.0.1:5599".into());
        let mut rows = Vec::new();
        attach_configured_rows(&config, &mut rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].serial, "127.0.0.1:5599");
        assert!(!rows[0].online);
        assert_eq!(rows[0].model, "未连接");
    }

    #[test]
    fn precheck_order_blocks_x86_before_download_decision() {
        let blocked = decide(DecideInput {
            installed: None,
            official: "2.7.91",
            abi: "x86_64",
            free_bytes: Some(REQUIRED_FREE_BYTES),
            force: false,
        });
        assert!(matches!(
            blocked,
            Decision::Reject(UpdateError::NoMatchingAbi { .. })
        ));
        let current = decide(DecideInput {
            installed: Some("2.7.91"),
            official: "2.7.91",
            abi: "x86_64",
            free_bytes: Some(1),
            force: false,
        });
        assert!(matches!(current, Decision::UpToDate));
        let low = decide(DecideInput {
            installed: Some("2.7.0"),
            official: "2.7.91",
            abi: "arm64-v8a",
            free_bytes: Some(1024),
            force: false,
        });
        assert!(matches!(
            low,
            Decision::Reject(UpdateError::InsufficientStorage { .. })
        ));
        let ready = decide(DecideInput {
            installed: None,
            official: "2.7.91",
            abi: "x86_64,armeabi-v7a",
            free_bytes: None,
            force: false,
        });
        assert!(matches!(ready, Decision::NeedInstall));
        let _ = FailureKind::DownloadFailed;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn x86_update_does_not_download() {
        let dir = tempfile::tempdir().expect("temp");
        let adb = dir.path().join("fake-adb");
        write_fake_adb(&adb, "x86", &dir.path().join("install.log"));
        let cache = dir.path().join("cache");
        let mut config = AppConfig::default();
        config.adb_path = Some(adb.display().to_string());
        config.apk_cache_dir = Some(cache.display().to_string());
        config.apk_url_override = Some("http://127.0.0.1:9/should-not-download.apk".into());
        let state = test_state(config, dir.path()).await;
        seed_version(&state).await;
        refresh_devices(&state).await.expect("refresh");
        let devices = state.devices.read().await.clone();
        assert_eq!(devices.len(), 1);
        assert!(!devices[0].abi_compatible);
        assert_eq!(devices[0].installed_label, "未安装");
        start_update(
            &state,
            UpdateRequest {
                devices: DeviceSelector::All,
                force: false,
            },
        )
        .await
        .expect("start");
        wait_until_settled(&state, 1).await;
        let device = state.devices.read().await[0].clone();
        assert_eq!(device.task.failure, Some(FailureKind::NoMatchingAbi));
        assert!(device.task.message.contains("MuMu 12"));
        assert!(!cache.join("should-not-download.apk").exists());
        let mut entries = tokio::fs::read_dir(&cache).await;
        if let Ok(ref mut entries) = entries {
            assert!(entries.next_entry().await.ok().flatten().is_none());
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_devices_share_one_download() {
        let dir = tempfile::tempdir().expect("temp");
        let install_log = dir.path().join("install.log");
        let adb = dir.path().join("fake-adb");
        write_fake_adb(&adb, "two", &install_log);
        let cache = dir.path().join("cache");
        let body = {
            let mut data = vec![0x50, 0x4B, 0x03, 0x04];
            data.extend(std::iter::repeat(4u8).take(3000));
            data
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let payload = body.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let served = Arc::new(SlowApk {
            hits: hits.clone(),
            body: payload,
        });
        tokio::spawn(async move {
            let app = axum::Router::new()
                .route("/arknights-hg-2791.apk", axum::routing::get(slow_apk))
                .with_state(served);
            axum::serve(listener, app).await.expect("serve");
        });

        let mut config = AppConfig::default();
        config.adb_path = Some(adb.display().to_string());
        config.apk_cache_dir = Some(cache.display().to_string());
        config.apk_url_override = Some(format!("http://{addr}/arknights-hg-2791.apk"));
        config.concurrency = 2;
        let state = test_state(config, dir.path()).await;
        seed_version(&state).await;
        refresh_devices(&state).await.expect("refresh");
        assert_eq!(state.devices.read().await.len(), 2);
        start_update(
            &state,
            UpdateRequest {
                devices: DeviceSelector::All,
                force: false,
            },
        )
        .await
        .expect("start");
        wait_until_settled(&state, 2).await;
        let devices = state.devices.read().await.clone();
        assert!(
            devices
                .iter()
                .all(|device| device.task.phase == TaskPhase::Succeeded),
            "{:?}",
            devices
                .iter()
                .map(|device| (&device.serial, &device.task.message, device.task.phase))
                .collect::<Vec<_>>()
        );
        assert_eq!(hits.load(AtomicOrdering::SeqCst), 1);
        let installs = std::fs::read_to_string(&install_log).unwrap_or_default();
        assert!(installs.contains("127.0.0.1:5591"), "{installs}");
        assert!(installs.contains("127.0.0.1:5592"), "{installs}");
        let apks = std::fs::read_dir(&cache)
            .expect("cache")
            .filter_map(|item| item.ok())
            .filter(|item| item.path().extension().and_then(|ext| ext.to_str()) == Some("apk"))
            .count();
        assert_eq!(apks, 1);
        let _ = body;
    }

    #[cfg(unix)]
    async fn test_state(config: AppConfig, root: &Path) -> SharedState {
        let (tx, _rx) = tokio::sync::broadcast::channel(32);
        let recent = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let client = reqwest::Client::builder().build().expect("client");
        AppState::new(
            config,
            crate::state::AppPaths {
                config_path: root.join("config.toml"),
                data_dir: root.to_path_buf(),
                log_dir: root.join("logs"),
            },
            "127.0.0.1:0".into(),
            tx,
            recent,
            client.clone(),
            client,
        )
    }

    #[cfg(unix)]
    async fn seed_version(state: &AppState) {
        let version = OfficialVersion {
            client_version: "2.7.91".into(),
            res_version: "test".into(),
        };
        *state.version_cache.write().await = Some(CachedVersion {
            version: version.clone(),
            fetched_at: Instant::now(),
        });
        publish_official(state, &version, true, None).await;
    }

    #[cfg(unix)]
    async fn wait_until_settled(state: &AppState, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let devices = state.devices.read().await.clone();
            let settled = devices.len() >= count
                && devices.iter().filter(|device| device.online).all(|device| {
                    matches!(
                        device.task.phase,
                        TaskPhase::Succeeded | TaskPhase::Failed | TaskPhase::Cancelled
                    )
                });
            if settled {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "timeout: {:?}",
                    devices
                        .iter()
                        .map(|device| (
                            device.serial.clone(),
                            device.task.phase,
                            device.task.message.clone()
                        ))
                        .collect::<Vec<_>>()
                );
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    #[cfg(unix)]
    struct SlowApk {
        hits: Arc<AtomicUsize>,
        body: Vec<u8>,
    }

    #[cfg(unix)]
    async fn slow_apk(
        axum::extract::State(state): axum::extract::State<Arc<SlowApk>>,
    ) -> axum::response::Response {
        state.hits.fetch_add(1, AtomicOrdering::SeqCst);
        tokio::time::sleep(Duration::from_millis(400)).await;
        axum::response::Response::builder()
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/vnd.android.package-archive",
            )
            .header(
                axum::http::header::CONTENT_LENGTH,
                state.body.len().to_string(),
            )
            .body(axum::body::Body::from(state.body.clone()))
            .expect("response")
    }

    #[cfg(unix)]
    fn write_fake_adb(path: &Path, mode: &str, install_log: &Path) {
        let install_log = install_log.display();
        let script = format!(
            r#"#!/bin/sh
MODE="{mode}"
INSTALL_LOG="{install_log}"
if [ "$1" = "version" ]; then
  echo "Android Debug Bridge version 1.0.41-fake"
  exit 0
fi
if [ "$1" = "kill-server" ] || [ "$1" = "start-server" ]; then
  exit 0
fi
if [ "$1" = "connect" ]; then
  echo "connected to $2"
  exit 0
fi
if [ "$1" = "devices" ]; then
  echo "List of devices attached"
  if [ "$MODE" = "x86" ]; then
    printf '%s\n' "127.0.0.1:5599	device"
  else
    printf '%s\n' "127.0.0.1:5591	device" "127.0.0.1:5592	device"
  fi
  exit 0
fi
if [ "$1" = "-s" ]; then
  serial="$2"
  sub="$3"
  script="${{4-}}"
  if [ "$sub" = "shell" ]; then
    case "$script" in
      *ro.product.model*)
        if [ "$MODE" = "x86" ]; then
          printf '%s\n' "x86AVD" "SPLIT" "14" "SPLIT" "x86_64" "SPLIT" "x86_64"
        else
          printf '%s\n' "MuMu" "SPLIT" "12" "SPLIT" "arm64-v8a" "SPLIT" "arm64-v8a"
        fi
        exit 0
        ;;
      *dumpsys*)
        echo "Unable to find package: com.hypergryph.arknights"
        exit 0
        ;;
      df*)
        echo "Filesystem 1K-blocks Used Available Use% Mounted on"
        echo "/dev/block/data 20971520 1024 20970496 1% /data"
        exit 0
        ;;
    esac
    echo "unhandled shell" >&2
    exit 1
  fi
  if [ "$sub" = "install" ]; then
    echo "Success"
    echo "$serial" >> "$INSTALL_LOG"
    exit 0
  fi
fi
echo "unhandled: $*" >&2
exit 1
"#
        );
        std::fs::write(path, script).expect("script");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}
