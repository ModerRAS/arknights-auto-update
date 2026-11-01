use std::path::PathBuf;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{bundled_adb_candidates, emulator_search_roots, free_bytes, open_browser};

#[cfg(not(windows))]
mod other;
#[cfg(not(windows))]
pub use other::{bundled_adb_candidates, emulator_search_roots, free_bytes, open_browser};

pub fn default_data_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("ark-updater");
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("ark-updater");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("ark-updater");
    }
    PathBuf::from("ark-updater-data")
}

pub fn static_adb_candidates() -> Vec<(PathBuf, &'static str)> {
    let mut found = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        found.push((
            PathBuf::from(local)
                .join("Android")
                .join("Sdk")
                .join("platform-tools")
                .join("adb.exe"),
            "Android SDK",
        ));
    }
    found.push((
        PathBuf::from(r"C:\platform-tools\adb.exe"),
        "C:\\platform-tools",
    ));
    found.extend(bundled_adb_candidates());
    found
}
