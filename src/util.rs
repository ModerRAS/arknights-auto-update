use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

pub fn nonempty(value: Option<&str>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub fn blank_to_none(value: &mut Option<String>) {
    if let Some(text) = value.as_ref() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            *value = None;
        } else if trimmed != text {
            *value = Some(trimmed.to_string());
        }
    }
}

/// 设备序列号或 `host:port`。拒绝空白与 shell 元字符。
/// 调用外部程序时仍然走参数数组，这里只是避免把明显非法的地址送进 adb。
pub fn valid_endpoint(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || value.len() > 128 {
        return false;
    }
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | ':'))
}

pub fn exe_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

pub fn percent(done: u64, total: Option<u64>) -> f64 {
    match total {
        Some(total) if total > 0 => ((done as f64) / (total as f64) * 100.0).clamp(0.0, 100.0),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_rejects_shell_metacharacters() {
        assert!(valid_endpoint("127.0.0.1:16384"));
        assert!(valid_endpoint("emulator-5554"));
        assert!(!valid_endpoint("127.0.0.1:1 && calc"));
        assert!(!valid_endpoint(""));
        assert!(!valid_endpoint("com.pkg/.."));
    }

    #[test]
    fn formats_gibibytes() {
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert!(format_bytes(4u64 * 1024 * 1024 * 1024).contains("GB"));
    }
}
