use std::path::PathBuf;

use serde_json::Value;

/// 假设：MuMu 12 多开端口步长为 32（实例 0 → 16384，实例 1 → 16416）。
/// `mumu-cli info` 能解析出端口时以 JSON 为准。
pub fn mumu_adb_port_guess(index: u32) -> u16 {
    16_384u32
        .saturating_add(index.saturating_mul(32))
        .min(u32::from(u16::MAX)) as u16
}

pub fn extract_adb_port(value: &Value) -> Option<u16> {
    fn as_port(value: &Value) -> Option<u16> {
        match value {
            Value::Number(number) => number.as_u64().and_then(|port| u16::try_from(port).ok()),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        }
    }
    fn walk(value: &Value, parent: &str) -> Option<u16> {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let lower = key.to_ascii_lowercase();
                    if matches!(
                        lower.as_str(),
                        "adb_port" | "adbport" | "adb_host_port" | "host_port"
                    ) {
                        if let Some(port) = as_port(child) {
                            return Some(port);
                        }
                    }
                    if lower == "port" && parent.contains("adb") {
                        if let Some(port) = as_port(child) {
                            return Some(port);
                        }
                    }
                    if let Some(port) = walk(child, &lower) {
                        return Some(port);
                    }
                }
                None
            }
            Value::Array(items) => items.iter().find_map(|child| walk(child, parent)),
            _ => None,
        }
    }
    walk(value, "")
}

pub fn find_mumu_cli(install_dir: Option<&str>) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(dir) = install_dir {
        let root = PathBuf::from(dir);
        candidates.push(root.join("nx_main").join("mumu-cli.exe"));
        candidates.push(root.join("nx_main").join("MuMuManager.exe"));
        candidates.push(root.join("mumu-cli.exe"));
    }
    for root in crate::platform::emulator_search_roots() {
        candidates.push(root.join("nx_main").join("mumu-cli.exe"));
        candidates.push(root.join("nx_main").join("MuMuManager.exe"));
    }
    for fallback in [
        r"C:\Program Files\Netease\MuMu Player 12\nx_main\mumu-cli.exe",
        r"C:\Program Files\Netease\MuMu Player 12\nx_main\MuMuManager.exe",
        r"C:\Program Files\Netease\MuMuPlayer-12.0\nx_main\mumu-cli.exe",
        r"C:\Program Files\Netease\MuMuPlayer-12.0\nx_main\MuMuManager.exe",
    ] {
        candidates.push(PathBuf::from(fallback));
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "未找到 mumu-cli.exe / MuMuManager.exe。请在模拟器条目里填写 start_program。".into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_adb_port() {
        let value = serde_json::json!({
            "index": 0,
            "adb": { "host": "127.0.0.1", "port": 16384 }
        });
        assert_eq!(extract_adb_port(&value), Some(16384));
        let direct = serde_json::json!({"adb_port": "16416"});
        assert_eq!(extract_adb_port(&direct), Some(16416));
    }
}
