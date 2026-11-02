use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;
use tokio::process::Command;

/// 外部程序调用。只保存程序路径和参数数组，禁止拼成一条命令字符串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl CommandSpec {
    pub fn new(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// 仅用于日志展示，不会交给 shell。
    pub fn display(&self) -> String {
        let mut parts = Vec::with_capacity(1 + self.args.len());
        parts.push(self.program.display().to_string());
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

#[derive(Debug)]
pub struct CommandOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }

    pub fn combined(&self) -> String {
        let mut text = self.stdout_str();
        let err = self.stderr_str();
        if !err.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&err);
        }
        text
    }
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("无法启动 {program}：{source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("等待 {program} 结束失败：{source}")]
    Wait {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{program} 执行超过 {secs}s")]
    Timeout { program: String, secs: u64 },
}

const MAX_OUTPUT: usize = 2 * 1024 * 1024;

fn truncate(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() > MAX_OUTPUT {
        bytes.truncate(MAX_OUTPUT);
    }
    bytes
}

pub async fn run_command(
    spec: &CommandSpec,
    timeout: Duration,
) -> Result<CommandOutput, CommandError> {
    let program = spec.program.display().to_string();
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // 假设：Windows 下隐藏子进程控制台窗口，且不经过 cmd.exe。
    // tokio::process::Command 在 Windows 上自带 creation_flags，不需要 std 的 CommandExt。
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let child = command.spawn().map_err(|source| CommandError::Spawn {
        program: program.clone(),
        source,
    })?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(CommandOutput {
            status: output.status.code(),
            stdout: truncate(output.stdout),
            stderr: truncate(output.stderr),
        }),
        Ok(Err(source)) => Err(CommandError::Wait { program, source }),
        Err(_elapsed) => Err(CommandError::Timeout {
            program,
            secs: timeout.as_secs().max(1),
        }),
    }
}

pub fn program_exists(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_keeps_spaced_program_out_of_args() {
        let spec = CommandSpec::new(
            r"C:\Program Files\Netease\MuMu Player 12\nx_main\mumu-cli.exe",
            ["control", "-v", "0", "launch"],
        );
        let program = spec.program.display().to_string();
        assert!(program.ends_with("mumu-cli.exe"));
        assert!(program.contains("Program Files"));
        assert_eq!(spec.args, ["control", "-v", "0", "launch"]);
        assert!(!spec.args.iter().any(|arg| arg.contains(' ')));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn args_with_spaces_and_cjk_stay_intact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("echo-args.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").expect("write script");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let spec = CommandSpec::new(
            &script,
            [
                "含 空格",
                "中文参数",
                r"C:\Program Files\Netease\测试\adb.exe",
            ],
        );
        let output = run_command(&spec, Duration::from_secs(5))
            .await
            .expect("run");
        assert!(output.success());
        assert_eq!(
            output.stdout_str(),
            "含 空格\n中文参数\nC:\\Program Files\\Netease\\测试\\adb.exe\n"
        );
    }
}
