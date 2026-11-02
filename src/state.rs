use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::{broadcast, Mutex, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::config::AppConfig;
use crate::download::ApkArtifact;
use crate::error::{FailureKind, UpdateError};
use crate::singleflight::SingleFlight;
use crate::version::OfficialVersion;

pub const LOG_CAPACITY: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    Idle,
    WaitingOnline,
    Checking,
    Downloading,
    Installing,
    Launching,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskPhase {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::WaitingOnline
                | Self::Checking
                | Self::Downloading
                | Self::Installing
                | Self::Launching
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadPhase {
    Idle,
    Resolving,
    Downloading,
    Verifying,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub phase: TaskPhase,
    pub percent: f64,
    pub message: String,
    pub failure: Option<FailureKind>,
}

impl TaskView {
    pub fn idle() -> Self {
        Self {
            phase: TaskPhase::Idle,
            percent: 0.0,
            message: "空闲".into(),
            failure: None,
        }
    }

    pub fn phase(phase: TaskPhase, message: impl Into<String>) -> Self {
        let message = message.into();
        let percent = if phase == TaskPhase::Succeeded {
            100.0
        } else {
            0.0
        };
        Self {
            phase,
            percent,
            message,
            failure: None,
        }
    }

    pub fn failed(err: &UpdateError) -> Self {
        let phase = if matches!(err, UpdateError::Cancelled) {
            TaskPhase::Cancelled
        } else {
            TaskPhase::Failed
        };
        Self {
            phase,
            percent: 0.0,
            message: err.to_string(),
            failure: Some(err.kind()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadSnapshot {
    pub phase: DownloadPhase,
    pub version: Option<String>,
    pub path: Option<String>,
    pub bytes: u64,
    pub total: Option<u64>,
    pub percent: f64,
    pub error: Option<String>,
}

impl Default for DownloadSnapshot {
    fn default() -> Self {
        Self {
            phase: DownloadPhase::Idle,
            version: None,
            path: None,
            bytes: 0,
            total: None,
            percent: 0.0,
            error: None,
        }
    }
}

impl DownloadSnapshot {
    pub fn ready(artifact: &ApkArtifact) -> Self {
        Self {
            phase: DownloadPhase::Ready,
            version: Some(artifact.version.clone()),
            path: Some(artifact.path.display().to_string()),
            bytes: artifact.bytes,
            total: Some(artifact.bytes),
            percent: 100.0,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OfficialSnapshot {
    pub client_version: Option<String>,
    pub res_version: Option<String>,
    pub checked_at_ms: Option<u64>,
    pub error: Option<String>,
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdbStatus {
    pub path: Option<String>,
    pub version: Option<String>,
    pub source: String,
    pub error: Option<String>,
}

impl Default for AdbStatus {
    fn default() -> Self {
        Self {
            path: None,
            version: None,
            source: "未检测".into(),
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceSnapshot {
    pub serial: String,
    pub model: String,
    pub android_version: String,
    pub abi: String,
    pub abi_compatible: bool,
    pub installed_version: Option<String>,
    pub installed_label: String,
    pub bilibili_installed: bool,
    pub online: bool,
    pub emulator_id: Option<String>,
    pub emulator_name: Option<String>,
    pub adb_path: String,
    pub task: TaskView,
}

impl DeviceSnapshot {
    pub fn placeholder(serial: &str, task: TaskView) -> Self {
        Self {
            serial: serial.to_string(),
            model: "等待上线".into(),
            android_version: "-".into(),
            abi: "-".into(),
            abi_compatible: false,
            installed_version: None,
            installed_label: "未知".into(),
            bilibili_installed: false,
            online: false,
            emulator_id: None,
            emulator_name: None,
            adb_path: String::new(),
            task,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub ts_ms: u64,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BusEvent {
    Log {
        ts_ms: u64,
        level: String,
        target: String,
        message: String,
    },
    Device {
        serial: String,
        task: TaskView,
    },
    Download {
        snapshot: DownloadSnapshot,
    },
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
}

pub struct CachedVersion {
    pub version: OfficialVersion,
    pub fetched_at: Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoStatus {
    pub enabled: bool,
    pub install: bool,
    pub interval: String,
    pub next_run_ms: Option<u64>,
    pub message: String,
}

impl Default for AutoStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            install: false,
            interval: String::new(),
            next_run_ms: None,
            message: "正在安排自动更新".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppUpdatePhase {
    Idle,
    Checking,
    Downloading,
    Verifying,
    Applying,
    UpToDate,
    Failed,
}

impl AppUpdatePhase {
    pub fn blocks_game_update(self) -> bool {
        matches!(self, Self::Downloading | Self::Verifying | Self::Applying)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AppUpdateStatus {
    pub current_version: String,
    pub current_commit: Option<String>,
    pub latest_version: Option<String>,
    pub latest_commit: Option<String>,
    pub phase: AppUpdatePhase,
    pub percent: f64,
    pub bytes: u64,
    pub total: Option<u64>,
    pub message: String,
    pub error: Option<String>,
    pub update_available: bool,
    pub next_run_ms: Option<u64>,
}

impl AppUpdateStatus {
    fn starting() -> Self {
        Self {
            current_version: crate::self_update::current_version().to_string(),
            current_commit: crate::self_update::current_commit().map(str::to_string),
            latest_version: None,
            latest_commit: None,
            phase: AppUpdatePhase::Idle,
            percent: 0.0,
            bytes: 0,
            total: None,
            message: "还没检查过程序更新".into(),
            error: None,
            update_available: false,
            next_run_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchStatus {
    pub enabled: bool,
    pub interval: String,
    pub next_run_ms: Option<u64>,
    pub message: String,
    pub offline_emulators: Vec<String>,
}

impl WatchStatus {
    fn starting() -> Self {
        Self {
            enabled: false,
            interval: String::new(),
            next_run_ms: None,
            message: "正在安排模拟器开机检测".into(),
            offline_emulators: Vec::new(),
        }
    }
}

pub struct AppState {
    pub paths: AppPaths,
    pub bound_listen: String,
    pub config: RwLock<AppConfig>,
    pub config_mtime: Mutex<Option<SystemTime>>,
    pub official: RwLock<OfficialSnapshot>,
    pub version_cache: RwLock<Option<CachedVersion>>,
    pub devices: RwLock<Vec<DeviceSnapshot>>,
    pub download: RwLock<DownloadSnapshot>,
    pub adb: RwLock<AdbStatus>,
    pub events: broadcast::Sender<BusEvent>,
    pub recent_logs: Arc<std::sync::Mutex<VecDeque<LogLine>>>,
    pub version_client: RwLock<reqwest::Client>,
    pub download_client: RwLock<reqwest::Client>,
    pub flight: SingleFlight<ApkArtifact>,
    pub device_locks: DashMap<String, Arc<Mutex<()>>>,
    pub update_cancel: Mutex<Option<CancellationToken>>,
    pub update_gate: Mutex<()>,
    pub game_update_gen: AtomicU64,
    pub game_update_running: AtomicBool,
    pub app_job_running: AtomicBool,
    pub app_cancel: Mutex<Option<CancellationToken>>,
    pub concurrency: RwLock<Arc<Semaphore>>,
    pub save_lock: Mutex<()>,
    pub known_versions: Mutex<HashMap<String, String>>,
    pub auto: RwLock<AutoStatus>,
    pub app_update: RwLock<AppUpdateStatus>,
    pub watch: RwLock<WatchStatus>,
    pub proxy: RwLock<crate::http_client::ProxyStatus>,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(
        config: AppConfig,
        paths: AppPaths,
        bound_listen: String,
        events: broadcast::Sender<BusEvent>,
        recent_logs: Arc<std::sync::Mutex<VecDeque<LogLine>>>,
        version_client: reqwest::Client,
        download_client: reqwest::Client,
    ) -> Arc<Self> {
        let permits = config.concurrency.clamp(1, 16);
        let known_path = paths.data_dir.join("known-versions.json");
        let proxy = initial_proxy(&config);
        Arc::new(Self {
            paths,
            bound_listen,
            concurrency: RwLock::new(Arc::new(Semaphore::new(permits))),
            config: RwLock::new(config),
            config_mtime: Mutex::new(None),
            official: RwLock::new(OfficialSnapshot::default()),
            version_cache: RwLock::new(None),
            devices: RwLock::new(Vec::new()),
            download: RwLock::new(DownloadSnapshot::default()),
            adb: RwLock::new(AdbStatus::default()),
            events,
            recent_logs,
            version_client: RwLock::new(version_client),
            download_client: RwLock::new(download_client),
            flight: SingleFlight::new(),
            device_locks: DashMap::new(),
            update_cancel: Mutex::new(None),
            update_gate: Mutex::new(()),
            game_update_gen: AtomicU64::new(0),
            game_update_running: AtomicBool::new(false),
            app_job_running: AtomicBool::new(false),
            app_cancel: Mutex::new(None),
            save_lock: Mutex::new(()),
            known_versions: Mutex::new(load_known_versions(&known_path)),
            auto: RwLock::new(AutoStatus::default()),
            app_update: RwLock::new(AppUpdateStatus::starting()),
            watch: RwLock::new(WatchStatus::starting()),
            proxy: RwLock::new(proxy),
        })
    }

    pub async fn reload_http_clients(&self) -> Result<(), String> {
        let proxy = self.config.read().await.proxy.clone();
        let built = crate::http_client::build_clients(proxy.as_deref())?;
        *self.version_client.write().await = built.version;
        *self.download_client.write().await = built.download;
        *self.proxy.write().await = built.proxy.status;
        Ok(())
    }

    pub async fn note_installed(&self, serial: &str, version: &str) {
        let mut known = self.known_versions.lock().await;
        if known.get(serial).map(String::as_str) == Some(version) {
            return;
        }
        known.insert(serial.to_string(), version.to_string());
        let path = self.paths.data_dir.join("known-versions.json");
        match serde_json::to_string_pretty(&*known) {
            Ok(text) => {
                if let Err(err) = std::fs::write(&path, text) {
                    tracing::warn!(path = %path.display(), error = %err, "写入已知版本失败");
                }
            }
            Err(err) => tracing::warn!(error = %err, "序列化已知版本失败"),
        }
    }

    pub fn emit(&self, event: BusEvent) {
        let _ = self.events.send(event);
    }

    pub fn device_lock(&self, serial: &str) -> Arc<Mutex<()>> {
        self.device_locks
            .entry(serial.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn set_device_task(&self, serial: &str, task: TaskView) {
        {
            let mut devices = self.devices.write().await;
            if let Some(device) = devices.iter_mut().find(|item| item.serial == serial) {
                device.task = task.clone();
            } else {
                devices.push(DeviceSnapshot::placeholder(serial, task.clone()));
            }
        }
        self.emit(BusEvent::Device {
            serial: serial.to_string(),
            task,
        });
    }

    pub async fn upsert_device(&self, mut snapshot: DeviceSnapshot) {
        let mut devices = self.devices.write().await;
        if let Some(existing) = devices
            .iter_mut()
            .find(|item| item.serial == snapshot.serial)
        {
            if existing.task.phase.is_active() {
                snapshot.task = existing.task.clone();
            }
            *existing = snapshot;
        } else {
            devices.push(snapshot);
        }
    }

    pub async fn set_download(&self, snapshot: DownloadSnapshot) {
        *self.download.write().await = snapshot.clone();
        self.emit(BusEvent::Download { snapshot });
    }

    pub async fn reload_concurrency(&self) {
        let permits = self.config.read().await.concurrency.clamp(1, 16);
        *self.concurrency.write().await = Arc::new(Semaphore::new(permits));
    }

    pub fn cache_dir(&self, config: &AppConfig) -> PathBuf {
        config.cache_dir(&self.paths.data_dir)
    }
}

pub fn merge_device_rows(
    old: &[DeviceSnapshot],
    mut scanned: Vec<DeviceSnapshot>,
) -> Vec<DeviceSnapshot> {
    for row in &mut scanned {
        if let Some(previous) = old.iter().find(|item| item.serial == row.serial) {
            if previous.task.phase.is_active() {
                row.task = previous.task.clone();
            }
        }
    }
    for previous in old {
        if previous.task.phase.is_active()
            && !scanned.iter().any(|item| item.serial == previous.serial)
        {
            scanned.push(previous.clone());
        }
    }
    scanned
}

fn initial_proxy(config: &AppConfig) -> crate::http_client::ProxyStatus {
    match crate::http_client::resolve_from_environment(config.proxy.as_deref()) {
        Ok(resolved) => resolved.status,
        Err(err) => crate::http_client::ProxyStatus {
            source: "direct".into(),
            display: err,
        },
    }
}

fn load_known_versions(path: &Path) -> HashMap<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(map) => map,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "已知版本文件无法解析，忽略");
                HashMap::new()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "读取已知版本失败");
            HashMap::new()
        }
    }
}

pub fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(serial: &str, phase: TaskPhase) -> DeviceSnapshot {
        DeviceSnapshot {
            serial: serial.into(),
            model: "m".into(),
            android_version: "12".into(),
            abi: "arm64-v8a".into(),
            abi_compatible: true,
            installed_version: None,
            installed_label: "未安装".into(),
            bilibili_installed: false,
            online: true,
            emulator_id: None,
            emulator_name: None,
            adb_path: "adb".into(),
            task: TaskView::phase(phase, "x"),
        }
    }

    #[test]
    fn merge_keeps_active_tasks() {
        let old = vec![
            row("a", TaskPhase::Installing),
            row("gone", TaskPhase::Downloading),
        ];
        let scanned = vec![row("a", TaskPhase::Idle), row("b", TaskPhase::Idle)];
        let merged = merge_device_rows(&old, scanned);
        let a = merged.iter().find(|item| item.serial == "a").unwrap();
        assert_eq!(a.task.phase, TaskPhase::Installing);
        assert!(merged.iter().any(|item| item.serial == "gone"));
        assert!(merged.iter().any(|item| item.serial == "b"));
    }
}
