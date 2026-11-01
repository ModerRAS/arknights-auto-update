use std::path::{Path, PathBuf};

use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    // SAFETY: wide 以 NUL 结尾；free 是合法的 u64 输出指针。失败时返回 0，不读取未初始化数据。
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        None
    } else {
        Some(free)
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn GetDiskFreeSpaceExW(
        directory: *const u16,
        free_bytes_available: *mut u64,
        total_bytes: *mut u64,
        total_free_bytes: *mut u64,
    ) -> i32;
}

fn read_reg_string(subkey: &str, value: &str) -> Option<String> {
    let hive = RegKey::predef(HKEY_LOCAL_MACHINE);
    let key = hive.open_subkey(subkey).ok()?;
    key.get_value::<String, _>(value)
        .ok()
        .filter(|text| !text.trim().is_empty())
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

pub fn emulator_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for key in [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\MuMuPlayer",
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\MuMuPlayer",
    ] {
        if let Some(location) = read_reg_string(key, "InstallLocation") {
            push_unique(&mut roots, PathBuf::from(location.trim()));
        }
        if let Some(uninstall) = read_reg_string(key, "UninstallString") {
            let trimmed = uninstall.trim().trim_matches('"');
            if let Some(parent) = Path::new(trimmed).parent() {
                push_unique(&mut roots, parent.to_path_buf());
            }
        }
    }
    for key in [
        r"SOFTWARE\leidian\ldplayer9",
        r"SOFTWARE\leidian\ldplayer14",
        r"SOFTWARE\leidian\ldplayer",
        r"SOFTWARE\WOW6432Node\leidian\ldplayer9",
        r"SOFTWARE\WOW6432Node\leidian\ldplayer14",
    ] {
        if let Some(dir) = read_reg_string(key, "InstallDir") {
            push_unique(&mut roots, PathBuf::from(dir.trim()));
        }
    }
    for fallback in [
        r"C:\Program Files\Netease\MuMu Player 12",
        r"C:\Program Files\Netease\MuMuPlayer-12.0",
        r"C:\Program Files\leidian\LDPlayer9",
        r"C:\leidian\LDPlayer9",
        r"D:\leidian\LDPlayer9",
        r"C:\leidian\LDPlayer14",
    ] {
        let path = PathBuf::from(fallback);
        if path.is_dir() {
            push_unique(&mut roots, path);
        }
    }
    roots
}

pub fn bundled_adb_candidates() -> Vec<(PathBuf, &'static str)> {
    let mut found = Vec::new();
    for root in emulator_search_roots() {
        let mumu_adb = root.join("nx_main").join("adb.exe");
        if mumu_adb.is_file() {
            found.push((mumu_adb, "模拟器自带"));
        }
        let root_adb = root.join("adb.exe");
        if root_adb.is_file() {
            found.push((root_adb, "模拟器自带"));
        }
    }
    found
}

pub fn open_browser(url: &str) {
    // `start` 是 cmd 内建命令。空标题参数必须单独成项，避免把 URL 当成窗口标题。
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}
