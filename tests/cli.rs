use std::process::Command;

fn binary() -> &'static str {
    // Cargo has used both the hyphenated binary name and the underscore form.
    if let Some(path) = option_env!("CARGO_BIN_EXE_arknights-auto-update") {
        path
    } else if let Some(path) = option_env!("CARGO_BIN_EXE_arknights_auto_update") {
        path
    } else {
        panic!("cargo did not set CARGO_BIN_EXE for arknights-auto-update")
    }
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .output()
        .expect("run arknights-auto-update")
}

#[test]
fn version_flag_prints_package_version() {
    let output = run(&["--version"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert_eq!(
        stdout,
        format!("arknights-auto-update {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn short_version_flag_matches_long_flag() {
    let long = run(&["--version"]);
    let short = run(&["-V"]);
    assert_eq!(long.stdout, short.stdout);
    assert!(short.status.success());
}

#[test]
fn help_flags_and_no_args_exit_zero() {
    for args in [Vec::new(), vec!["--help"], vec!["-h"]] {
        let output = run(&args);
        assert!(output.status.success(), "{args:?}");
        let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
        assert!(stdout.contains("--version"), "{args:?}");
    }
}

#[test]
fn unknown_argument_exits_with_usage_error() {
    let output = run(&["--unknown"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("未知参数"));
}
