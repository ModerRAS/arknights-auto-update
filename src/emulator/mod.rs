mod ldplayer;
mod mumu;

pub use ldplayer::{find_ldconsole, ldplayer_adb_port};
pub use mumu::{extract_adb_port, find_mumu_cli, mumu_adb_port_guess};

use std::path::PathBuf;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::adb::{adb_connect, adb_devices, AdbInfo};
use crate::config::{EmulatorConfig, EmulatorKind};
use crate::error::UpdateError;
use crate::installer::{ld_launch_spec, mumu_launch_spec};
use crate::process_util::{run_command, CommandSpec};

/// 雷电 index 0 的端口。用户给定公式：`5554 + index * 2`。
pub fn expected_adb_port(emulator: &EmulatorConfig) -> Option<u16> {
    if let Some(port) = emulator.adb_port {
        return Some(port);
    }
    match emulator.kind {
        EmulatorKind::Ldplayer => Some(ldplayer_adb_port(emulator.vmindex)),
        EmulatorKind::Mumu => Some(mumu_adb_port_guess(emulator.vmindex)),
        EmulatorKind::Custom => None,
    }
}

pub fn expected_serial(emulator: &EmulatorConfig) -> Option<String> {
    if let Some(serial) = emulator
        .expected_serial
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Some(serial.to_string());
    }
    expected_adb_port(emulator).map(|port| format!("127.0.0.1:{port}"))
}

pub fn build_start_command(emulator: &EmulatorConfig) -> Result<CommandSpec, UpdateError> {
    if let Some(program) = emulator
        .start_program
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(CommandSpec::new(program, emulator.start_args.clone()));
    }
    match emulator.kind {
        EmulatorKind::Mumu => {
            let cli = find_mumu_cli(emulator.install_dir.as_deref())
                .map_err(|err| UpdateError::DeviceOffline(format!("无法拉起 MuMu：{err}")))?;
            Ok(mumu_launch_spec(&cli, emulator.vmindex))
        }
        EmulatorKind::Ldplayer => {
            let cli = find_ldconsole(emulator.install_dir.as_deref())
                .map_err(|err| UpdateError::DeviceOffline(format!("无法拉起雷电：{err}")))?;
            Ok(ld_launch_spec(&cli, emulator.vmindex))
        }
        EmulatorKind::Custom => Err(UpdateError::DeviceOffline(format!(
            "自定义模拟器「{}」没有填写启动程序",
            emulator.name
        ))),
    }
}

pub async fn launch_emulator(emulator: &EmulatorConfig) -> Result<(), UpdateError> {
    let spec = build_start_command(emulator)?;
    tracing::info!(
        program = %spec.program.display(),
        args = ?spec.args,
        name = %emulator.name,
        "拉起模拟器"
    );
    let output = run_command(&spec, Duration::from_secs(120))
        .await
        .map_err(|err| UpdateError::DeviceOffline(err.to_string()))?;
    if !output.success() {
        // 假设：部分模拟器 CLI 拉起后仍返回非 0，只要进程能启动就继续等 adb。
        tracing::warn!(
            code = ?output.status,
            stderr = %output.stderr_str(),
            "启动命令返回非零，仍继续等待设备上线"
        );
    }
    Ok(())
}

pub async fn wait_until_online(
    adb: &AdbInfo,
    emulator: &EmulatorConfig,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<String, UpdateError> {
    let serial = expected_serial(emulator).ok_or_else(|| {
        UpdateError::DeviceOffline(format!(
            "模拟器「{}」没有 expected_serial 或 adb 端口，无法等待上线",
            emulator.name
        ))
    })?;
    let started = std::time::Instant::now();
    loop {
        if cancel.is_cancelled() {
            return Err(UpdateError::Cancelled);
        }
        if started.elapsed() > timeout {
            return Err(UpdateError::DeviceOffline(format!(
                "等待 {serial} 上线超过 {} 秒",
                timeout.as_secs()
            )));
        }
        if let Err(err) = adb_connect(&adb.path, &serial).await {
            tracing::debug!(error = %err, serial, "adb connect 尚未成功");
        }
        match adb_devices(&adb.path).await {
            Ok(devices) => {
                if devices
                    .iter()
                    .any(|device| device.serial == serial && device.state == "device")
                {
                    return Ok(serial);
                }
            }
            Err(err) => tracing::debug!(error = %err, "枚举 adb 设备失败，继续等待"),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

pub fn emulator_adb_override(emulator: &EmulatorConfig) -> Option<PathBuf> {
    emulator
        .adb_path
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EmulatorKind;

    fn emu(kind: EmulatorKind, index: u32) -> EmulatorConfig {
        EmulatorConfig {
            id: "id".into(),
            name: "n".into(),
            kind,
            vmindex: index,
            ..EmulatorConfig::default()
        }
    }

    #[test]
    fn ports_follow_documented_formulas() {
        assert_eq!(ldplayer_adb_port(0), 5554);
        assert_eq!(ldplayer_adb_port(1), 5556);
        assert_eq!(mumu_adb_port_guess(0), 16384);
        assert_eq!(mumu_adb_port_guess(1), 16416);
        assert_eq!(
            expected_serial(&emu(EmulatorKind::Ldplayer, 2)).as_deref(),
            Some("127.0.0.1:5558")
        );
    }

    #[test]
    fn explicit_start_command_is_not_rewritten() {
        let mut emulator = emu(EmulatorKind::Mumu, 0);
        emulator.start_program = Some(r"D:\模拟器\mumu-cli.exe".into());
        emulator.start_args = vec!["control".into(), "-v".into(), "3".into(), "launch".into()];
        let spec = build_start_command(&emulator).expect("spec");
        assert_eq!(spec.program, PathBuf::from(r"D:\模拟器\mumu-cli.exe"));
        assert_eq!(spec.args, vec!["control", "-v", "3", "launch"]);
    }
}
