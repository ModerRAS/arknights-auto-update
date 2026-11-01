use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::adb::{adb_install, classify_install_output};
use crate::config::{EmulatorConfig, EmulatorKind};
use crate::emulator::{find_ldconsole, find_mumu_cli};
use crate::error::{FailureKind, UpdateError};
use crate::process_util::CommandSpec;
use crate::version::PACKAGE_OFFICIAL;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    MumuCli,
    LdplayerCli,
    Adb,
    AdbNoStreaming,
}

impl InstallMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::MumuCli => "MuMu CLI",
            Self::LdplayerCli => "雷电 CLI",
            Self::Adb => "adb install -r",
            Self::AdbNoStreaming => "adb install --no-streaming -r",
        }
    }
}

pub fn mumu_install_specs(cli: &Path, vmindex: u32, apk: &Path) -> Vec<CommandSpec> {
    let index = vmindex.to_string();
    let apk = apk.display().to_string();
    vec![
        CommandSpec::new(
            cli,
            [
                "control",
                "-v",
                index.as_str(),
                "app",
                "install",
                "-apk",
                apk.as_str(),
            ],
        ),
        CommandSpec::new(
            cli,
            ["-v", index.as_str(), "app", "install", "-apk", apk.as_str()],
        ),
        CommandSpec::new(
            cli,
            ["api", "-v", index.as_str(), "install_apk", apk.as_str()],
        ),
    ]
}

pub fn mumu_launch_spec(cli: &Path, vmindex: u32) -> CommandSpec {
    let index = vmindex.to_string();
    CommandSpec::new(cli, ["control", "-v", index.as_str(), "launch"])
}

pub fn mumu_app_launch_spec(cli: &Path, vmindex: u32) -> CommandSpec {
    let index = vmindex.to_string();
    CommandSpec::new(
        cli,
        [
            "control",
            "-v",
            index.as_str(),
            "app",
            "launch",
            "-pkg",
            PACKAGE_OFFICIAL,
        ],
    )
}

pub fn ld_install_spec(cli: &Path, index: u32, apk: &Path) -> CommandSpec {
    CommandSpec::new(
        cli,
        [
            "installapp",
            "--index",
            index.to_string().as_str(),
            "--filename",
            apk.display().to_string().as_str(),
        ],
    )
}

pub fn ld_launch_spec(cli: &Path, index: u32) -> CommandSpec {
    CommandSpec::new(cli, ["launch", "--index", index.to_string().as_str()])
}

pub fn ld_app_launch_spec(cli: &Path, index: u32) -> CommandSpec {
    CommandSpec::new(
        cli,
        [
            "runapp",
            "--index",
            index.to_string().as_str(),
            "--packagename",
            PACKAGE_OFFICIAL,
        ],
    )
}

pub fn adb_install_specs(serial: &str, apk: &Path, allow_downgrade: bool) -> [Vec<String>; 2] {
    let apk = apk.display().to_string();
    let mut streaming = vec!["-s".into(), serial.into(), "install".into(), "-r".into()];
    let mut plain = vec![
        "-s".into(),
        serial.into(),
        "install".into(),
        "--no-streaming".into(),
        "-r".into(),
    ];
    if allow_downgrade {
        streaming.push("-d".into());
        plain.push("-d".into());
    }
    streaming.push(apk.clone());
    plain.push(apk);
    [streaming, plain]
}

fn fatal_from_output(output: &str) -> Option<UpdateError> {
    match classify_install_output(output) {
        Some(FailureKind::NoMatchingAbi) => Some(UpdateError::NoMatchingAbi {
            abi: "安装器返回 INSTALL_FAILED_NO_MATCHING_ABIS".into(),
        }),
        Some(FailureKind::SignatureConflict) => Some(UpdateError::SignatureConflict {
            detail: shorten(output),
        }),
        Some(FailureKind::InsufficientStorage) => Some(UpdateError::InsufficientStorage {
            detail: shorten(output),
        }),
        Some(FailureKind::DeviceOffline) => Some(UpdateError::DeviceOffline(shorten(output))),
        _ => None,
    }
}

fn shorten(output: &str) -> String {
    let mut text = output.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > 400 {
        text = text.chars().take(400).collect();
        text.push('…');
    }
    text
}

async fn run_spec(
    spec: &CommandSpec,
    timeout: Duration,
) -> Result<crate::process_util::CommandOutput, String> {
    tracing::info!(program = %spec.program.display(), args = ?spec.args, "执行安装命令");
    crate::process_util::run_command(spec, timeout)
        .await
        .map_err(|err| err.to_string())
}

pub async fn install_apk(
    adb: &Path,
    serial: &str,
    apk: &Path,
    emulator: Option<&EmulatorConfig>,
    timeout: Duration,
    allow_downgrade: bool,
) -> Result<InstallMethod, UpdateError> {
    if let Some(emulator) = emulator {
        match try_native(emulator, apk, timeout).await {
            Ok(method) => return Ok(method),
            Err(NativeAttempt::Fatal(err)) => return Err(err),
            Err(NativeAttempt::Fallback(message)) => {
                tracing::warn!(error = %message, serial, "模拟器原生安装未成功，改用 adb");
            }
        }
    }
    let [streaming, plain] = adb_install_specs(serial, apk, allow_downgrade);
    let first = adb_install(adb, &streaming, timeout)
        .await
        .map_err(UpdateError::InstallFailed)?;
    if let Some(err) = fatal_from_output(&first.combined()) {
        return Err(err);
    }
    if first.success() {
        return Ok(InstallMethod::Adb);
    }
    tracing::warn!(output = %shorten(&first.combined()), "adb install -r 失败，尝试 --no-streaming");
    let second = adb_install(adb, &plain, timeout)
        .await
        .map_err(UpdateError::InstallFailed)?;
    if let Some(err) = fatal_from_output(&second.combined()) {
        return Err(err);
    }
    if second.success() {
        return Ok(InstallMethod::AdbNoStreaming);
    }
    Err(UpdateError::InstallFailed(format!(
        "adb install 失败：{}",
        shorten(&second.combined())
    )))
}

enum NativeAttempt {
    Fatal(UpdateError),
    Fallback(String),
}

async fn try_native(
    emulator: &EmulatorConfig,
    apk: &Path,
    timeout: Duration,
) -> Result<InstallMethod, NativeAttempt> {
    match emulator.kind {
        EmulatorKind::Mumu => {
            let cli = match resolve_mumu_cli(emulator) {
                Ok(path) => path,
                Err(err) => return Err(NativeAttempt::Fallback(err)),
            };
            let mut last = String::new();
            for spec in mumu_install_specs(&cli, emulator.vmindex, apk) {
                match run_spec(&spec, timeout).await {
                    Ok(output) => {
                        if let Some(err) = fatal_from_output(&output.combined()) {
                            return Err(NativeAttempt::Fatal(err));
                        }
                        if output.success() {
                            return Ok(InstallMethod::MumuCli);
                        }
                        last = shorten(&output.combined());
                    }
                    Err(err) => last = err,
                }
            }
            Err(NativeAttempt::Fallback(format!(
                "MuMu 安装命令失败：{last}"
            )))
        }
        EmulatorKind::Ldplayer => {
            let cli = match resolve_ld_cli(emulator) {
                Ok(path) => path,
                Err(err) => return Err(NativeAttempt::Fallback(err)),
            };
            let spec = ld_install_spec(&cli, emulator.vmindex, apk);
            match run_spec(&spec, timeout).await {
                Ok(output) => {
                    if let Some(err) = fatal_from_output(&output.combined()) {
                        return Err(NativeAttempt::Fatal(err));
                    }
                    if output.success() {
                        Ok(InstallMethod::LdplayerCli)
                    } else {
                        Err(NativeAttempt::Fallback(format!(
                            "雷电安装命令失败：{}",
                            shorten(&output.combined())
                        )))
                    }
                }
                Err(err) => Err(NativeAttempt::Fallback(err)),
            }
        }
        EmulatorKind::Custom => Err(NativeAttempt::Fallback(
            "自定义模拟器没有专用安装命令，直接使用 adb".into(),
        )),
    }
}

fn resolve_mumu_cli(emulator: &EmulatorConfig) -> Result<PathBuf, String> {
    if let Some(program) = emulator.start_program.as_ref() {
        let path = PathBuf::from(program);
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().contains("mumu"))
            && path.is_file()
        {
            return Ok(path);
        }
    }
    find_mumu_cli(emulator.install_dir.as_deref())
}

fn resolve_ld_cli(emulator: &EmulatorConfig) -> Result<PathBuf, String> {
    if let Some(program) = emulator.start_program.as_ref() {
        let path = PathBuf::from(program);
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().contains("ldconsole"))
            && path.is_file()
        {
            return Ok(path);
        }
    }
    find_ldconsole(emulator.install_dir.as_deref())
}

pub async fn launch_game(
    adb: &Path,
    serial: &str,
    emulator: Option<&EmulatorConfig>,
) -> Result<(), String> {
    if let Some(emulator) = emulator {
        let spec = match emulator.kind {
            EmulatorKind::Mumu => resolve_mumu_cli(emulator)
                .ok()
                .map(|cli| mumu_app_launch_spec(&cli, emulator.vmindex)),
            EmulatorKind::Ldplayer => resolve_ld_cli(emulator)
                .ok()
                .map(|cli| ld_app_launch_spec(&cli, emulator.vmindex)),
            EmulatorKind::Custom => None,
        };
        if let Some(spec) = spec {
            if let Ok(output) = run_spec(&spec, Duration::from_secs(60)).await {
                if output.success() {
                    return Ok(());
                }
                tracing::warn!(output = %shorten(&output.combined()), "模拟器启动游戏失败，改用 adb monkey");
            }
        }
    }
    let script = format!("monkey -p {PACKAGE_OFFICIAL} -c android.intent.category.LAUNCHER 1");
    let output = adb_install(
        adb,
        &["-s".into(), serial.into(), "shell".into(), script],
        Duration::from_secs(60),
    )
    .await?;
    if output.success() {
        Ok(())
    } else {
        Err(shorten(&output.combined()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mumu_install_falls_back_in_order_without_joining_args() {
        let cli = Path::new(r"C:\Program Files\Netease\MuMu Player 12\nx_main\mumu-cli.exe");
        let apk = Path::new(r"D:\缓存目录\方舟\arknights-hg-2791.apk");
        let specs = mumu_install_specs(cli, 0, apk);
        assert_eq!(specs.len(), 3);
        let apk_arg = apk.display().to_string();
        let owned = |items: &[&str]| {
            items
                .iter()
                .map(|item| (*item).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            specs[0].args,
            owned(&["control", "-v", "0", "app", "install", "-apk", &apk_arg])
        );
        assert_eq!(
            specs[1].args,
            owned(&["-v", "0", "app", "install", "-apk", &apk_arg])
        );
        assert_eq!(
            specs[2].args,
            owned(&["api", "-v", "0", "install_apk", &apk_arg])
        );
        assert!(specs.iter().all(|spec| spec.program == cli));
        let ld = ld_install_spec(Path::new(r"C:\leidian\LDPlayer9\ldconsole.exe"), 1, apk);
        assert_eq!(
            ld.args,
            owned(&["installapp", "--index", "1", "--filename", &apk_arg])
        );
    }

    #[test]
    fn adb_install_downgrade_flag_is_its_own_arg() {
        let [first, second] = adb_install_specs("127.0.0.1:16384", Path::new("a.apk"), true);
        assert!(first.iter().any(|arg| arg == "-d"));
        assert!(second
            .windows(2)
            .any(|pair| pair == ["--no-streaming", "-r"]));
        assert_eq!(first.last().map(String::as_str), Some("a.apk"));
    }
}
