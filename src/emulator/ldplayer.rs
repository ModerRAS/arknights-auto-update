use std::path::PathBuf;

pub fn ldplayer_adb_port(index: u32) -> u16 {
    5554u32
        .saturating_add(index.saturating_mul(2))
        .min(u32::from(u16::MAX)) as u16
}

pub fn find_ldconsole(install_dir: Option<&str>) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(dir) = install_dir {
        candidates.push(PathBuf::from(dir).join("ldconsole.exe"));
    }
    for root in crate::platform::emulator_search_roots() {
        candidates.push(root.join("ldconsole.exe"));
    }
    for fallback in [
        r"C:\leidian\LDPlayer9\ldconsole.exe",
        r"D:\leidian\LDPlayer9\ldconsole.exe",
        r"C:\Program Files\leidian\LDPlayer9\ldconsole.exe",
        r"C:\leidian\LDPlayer14\ldconsole.exe",
        r"C:\leidian\LDPlayer4\ldconsole.exe",
    ] {
        candidates.push(PathBuf::from(fallback));
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "未找到 ldconsole.exe。请在模拟器条目里填写 start_program。".into())
}
