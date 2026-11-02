use std::cmp::Ordering;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

pub const VERSION_URL: &str = "https://ak-conf.hypergryph.com/config/prod/official/Android/version";
pub const PACKAGE_OFFICIAL: &str = "com.hypergryph.arknights";
pub const PACKAGE_BILIBILI: &str = "com.hypergryph.arknights.bilibili";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficialVersion {
    pub client_version: String,
    pub res_version: String,
}

#[derive(Debug, Deserialize)]
struct RawVersion {
    #[serde(rename = "clientVersion")]
    client_version: String,
    #[serde(rename = "resVersion", default)]
    res_version: String,
}

pub fn parse_official_version(text: &str) -> Result<OfficialVersion, String> {
    let raw: RawVersion =
        serde_json::from_str(text).map_err(|err| format!("版本接口 JSON 无法解析：{err}"))?;
    let client_version = raw.client_version.trim().to_string();
    if client_version.is_empty() {
        return Err("版本接口缺少 clientVersion".into());
    }
    Ok(OfficialVersion {
        client_version,
        res_version: raw.res_version.trim().to_string(),
    })
}

/// 按 `.` 分段，每段只取前导数字。没有数字的段视为 0。
/// 假设：`2.7.91-beta` 的最后一段是 91。
pub fn version_parts(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|segment| {
            let digits: String = segment
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect();
            digits.parse::<u64>().unwrap_or(0)
        })
        .collect()
}

pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let left_parts = version_parts(left);
    let right_parts = version_parts(right);
    let len = left_parts.len().max(right_parts.len());
    for index in 0..len {
        let left_value = left_parts.get(index).copied().unwrap_or(0);
        let right_value = right_parts.get(index).copied().unwrap_or(0);
        match left_value.cmp(&right_value) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

pub fn version_is_newer(official: &str, installed: &str) -> bool {
    compare_versions(official, installed) == Ordering::Greater
}

pub fn compact_version(version: &str) -> String {
    version.chars().filter(|ch| ch.is_ascii_digit()).collect()
}

fn filename_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r"(?i)arknights-hg-(\d+)\.apk").expect("内置文件名版本正则无效"))
}

pub fn filename_build_number(name: &str) -> Option<String> {
    filename_regex()
        .captures(name)
        .and_then(|caps| caps.get(1).map(|item| item.as_str().to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilenameCheck {
    Match,
    Mismatch {
        found: String,
        expected: String,
    },
    /// 假设：文件名不含 `arknights-hg-<数字>.apk` 时无法交叉校验，不因此失败。
    Unmarked,
}

pub fn check_filename_version(url_or_name: &str, client_version: &str) -> FilenameCheck {
    let expected = compact_version(client_version);
    match filename_build_number(url_or_name) {
        None => FilenameCheck::Unmarked,
        Some(found) if found == expected => FilenameCheck::Match,
        Some(found) => FilenameCheck::Mismatch { found, expected },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_official_payload() {
        let raw = r#"{"clientVersion":"2.7.91","resVersion":"26-10-29-15-36-11-33649006"}"#;
        let parsed = parse_official_version(raw).expect("parse");
        assert_eq!(parsed.client_version, "2.7.91");
        assert_eq!(parsed.res_version, "26-10-29-15-36-11-33649006");
    }

    #[test]
    fn compares_numeric_segments() {
        assert_eq!(compare_versions("2.7.91", "2.7.91"), Ordering::Equal);
        assert_eq!(compare_versions("2.7.10", "2.7.9"), Ordering::Greater);
        assert_eq!(compare_versions("2.7.9", "2.7.10"), Ordering::Less);
        assert_eq!(compare_versions("2.8", "2.7.91"), Ordering::Greater);
        assert_eq!(compare_versions("2.7", "2.7.0"), Ordering::Equal);
        assert_eq!(compare_versions("2.7.91", "2.7.90"), Ordering::Greater);
        assert!(version_is_newer("2.7.91", "2.6.01"));
        assert!(!version_is_newer("2.7.91", "2.7.91"));
    }

    #[test]
    fn filename_build_matches_client_version() {
        let url = "https://ak.hycdn.cn/GzD1CpaWgmSq1wew/79.0/package/1/1/Android/79.0.0_R4C8v7EmosKsRK1Q/arknights-hg-2791.apk";
        assert_eq!(filename_build_number(url).as_deref(), Some("2791"));
        assert_eq!(check_filename_version(url, "2.7.91"), FilenameCheck::Match);
        assert_eq!(
            check_filename_version("arknights-hg-2790.apk", "2.7.91"),
            FilenameCheck::Mismatch {
                found: "2790".into(),
                expected: "2791".into(),
            }
        );
        assert_eq!(
            check_filename_version("game.apk", "2.7.91"),
            FilenameCheck::Unmarked
        );
        assert_eq!(compact_version("2.7.91"), "2791");
    }
}
