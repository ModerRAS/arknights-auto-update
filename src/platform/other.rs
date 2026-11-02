use std::path::{Path, PathBuf};

pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if bytes.contains(&0) {
        return None;
    }
    let mut c_path = Vec::with_capacity(bytes.len() + 1);
    c_path.extend_from_slice(bytes);
    c_path.push(0);
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: c_path 以 NUL 结尾且不含内部 NUL；stat 由 statvfs 完整写入后再读取。
    let rc = unsafe { libc::statvfs(c_path.as_ptr().cast(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: statvfs 成功时结构体已被初始化。
    let stat = unsafe { stat.assume_init() };
    // 假设：当前目标上 statvfs 的块计数已经是 u64。32 位平台若类型更窄，需要再做一次加宽。
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

pub fn emulator_search_roots() -> Vec<PathBuf> {
    Vec::new()
}

pub fn bundled_adb_candidates() -> Vec<(PathBuf, &'static str)> {
    Vec::new()
}

pub fn open_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_has_free_space() {
        let dir = tempfile::tempdir().expect("temp");
        let free = free_bytes(dir.path()).expect("statvfs");
        assert!(free > 0);
    }
}
