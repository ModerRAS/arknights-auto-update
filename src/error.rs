use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    NoMatchingAbi,
    SignatureConflict,
    InsufficientStorage,
    DownloadFailed,
    InstallFailed,
    DeviceOffline,
    AdbError,
    Cancelled,
}

#[derive(Debug, Error, Clone)]
pub enum UpdateError {
    #[error("需要带 ARM 翻译的模拟器（MuMu 12 / 雷电 9）。当前 ABI：{abi}")]
    NoMatchingAbi { abi: String },

    #[error(
        "签名冲突（INSTALL_FAILED_UPDATE_INCOMPATIBLE）。请先手动卸载已安装的官服明日方舟后再更新，本程序不会自动卸载。{detail}"
    )]
    SignatureConflict { detail: String },

    #[error("存储空间不足，需要至少 4GB 可用空间。{detail}")]
    InsufficientStorage { detail: String },

    #[error("下载失败：{0}")]
    DownloadFailed(String),

    #[error("安装失败：{0}")]
    InstallFailed(String),

    #[error("设备离线或未能在超时内上线：{0}")]
    DeviceOffline(String),

    #[error("adb 错误：{0}")]
    AdbError(String),

    #[error("已取消")]
    Cancelled,
}

impl UpdateError {
    pub fn kind(&self) -> FailureKind {
        match self {
            Self::NoMatchingAbi { .. } => FailureKind::NoMatchingAbi,
            Self::SignatureConflict { .. } => FailureKind::SignatureConflict,
            Self::InsufficientStorage { .. } => FailureKind::InsufficientStorage,
            Self::DownloadFailed(_) => FailureKind::DownloadFailed,
            Self::InstallFailed(_) => FailureKind::InstallFailed,
            Self::DeviceOffline(_) => FailureKind::DeviceOffline,
            Self::AdbError(_) => FailureKind::AdbError,
            Self::Cancelled => FailureKind::Cancelled,
        }
    }

    pub fn from_download(message: String) -> Self {
        if message == "已取消" {
            Self::Cancelled
        } else {
            Self::DownloadFailed(message)
        }
    }
}
