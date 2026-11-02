use std::process::Command;

fn binary() -> &'static str {
    if let Some(path) = option_env!("CARGO_BIN_EXE_ark-updater") {
        path
    } else if let Some(path) = option_env!("CARGO_BIN_EXE_ark_updater") {
        path
    } else {
        panic!("cargo did not set CARGO_BIN_EXE for ark-updater")
    }
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .output()
        .expect("run ark-updater")
}

#[test]
fn version_flag_prints_package_version() {
    for args in [["--version"], ["-V"]] {
        let output = run(&args);
        assert!(output.status.success(), "{args:?}");
        let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "{args:?}: {stdout}"
        );
        assert!(stdout.contains("ark-updater"), "{args:?}: {stdout}");
    }
}

#[test]
fn incomplete_replace_does_not_start_the_server() {
    let output = run(&["--replace-from", "missing-new-binary"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("替换") || stderr.contains("参数"),
        "{stderr}"
    );
}

#[test]
fn help_exits_without_starting_the_server() {
    for args in [["--help"], ["-h"]] {
        let output = run(&args);
        assert!(output.status.success(), "{args:?}");
        let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
        assert!(stdout.contains("--open"), "{args:?}: {stdout}");
        assert!(!stdout.contains("已启动"), "{args:?}: {stdout}");
    }
}

#[test]
fn unknown_argument_exits_with_usage_error() {
    let output = run(&["--unknown"]);
    assert_eq!(output.status.code(), Some(2));
}
