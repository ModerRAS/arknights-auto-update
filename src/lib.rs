//! Argument parsing and version reporting for `arknights-auto-update`.

/// Command selected from the process arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Print usage.
    Help,
    /// Print the package name and version.
    Version,
}

/// Package version from `Cargo.toml`.
pub fn package_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `name version` line printed by `--version`.
pub fn version_line() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), package_version())
}

/// Usage text, including the trailing newline.
pub fn help() -> &'static str {
    "\
arknights-auto-update

用法:
    arknights-auto-update [--help | --version]

选项:
    -h, --help       显示帮助
    -V, --version    显示版本
"
}

/// Parse arguments after the program name.
///
/// No arguments selects help. At most one flag is accepted.
pub fn parse_args<I, S>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(Command::Help);
    };
    if args.next().is_some() {
        return Err("参数过多".to_string());
    }
    match first.as_ref() {
        "-h" | "--help" => Ok(Command::Help),
        "-V" | "--version" => Ok(Command::Version),
        other => Err(format!("未知参数: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_semver_component(part: &str) -> bool {
        let bytes = part.as_bytes();
        if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
            return false;
        }
        bytes.len() == 1 || bytes[0] != b'0'
    }

    #[test]
    fn package_version_is_strict_semver() {
        let version = package_version();
        let mut parts = version.split('.');
        for _ in 0..3 {
            let part = parts.next().expect("semver component");
            assert!(is_semver_component(part), "{version}");
        }
        assert!(parts.next().is_none(), "{version}");
    }

    #[test]
    fn version_line_matches_package_metadata() {
        assert_eq!(
            version_line(),
            format!("arknights-auto-update {}", package_version())
        );
    }

    #[test]
    fn help_lists_version_flag() {
        assert!(help().contains("--version"));
        assert!(help().ends_with('\n'));
    }

    #[test]
    fn parse_defaults_and_flags() {
        assert_eq!(parse_args(std::iter::empty::<&str>()), Ok(Command::Help));
        assert_eq!(parse_args(["--help"]), Ok(Command::Help));
        assert_eq!(parse_args(["-h"]), Ok(Command::Help));
        assert_eq!(parse_args(["--version"]), Ok(Command::Version));
        assert_eq!(parse_args(["-V"]), Ok(Command::Version));
    }

    #[test]
    fn parse_rejects_unknown_and_extra_arguments() {
        assert_eq!(
            parse_args(["--unknown"]),
            Err("未知参数: --unknown".to_string())
        );
        assert_eq!(
            parse_args(["--help", "--version"]),
            Err("参数过多".to_string())
        );
    }
}
