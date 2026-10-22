//! adb 封装：进程调用与输出解析。
//!
//! M1 阶段由 worker 线程调用（阻塞式）；解析函数为纯函数，可单测。
//! adb CLI 在 Windows / Linux 行为一致，测试机上本就安装。

use crate::exec;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// adb 可执行文件路径；环境变量 `ADB` 可覆盖（非常规安装路径/测试桩）。
fn adb_path() -> String {
    std::env::var("ADB").unwrap_or_else(|_| "adb".into())
}

/// 执行 `adb shell <cmd>`，返回去除首尾空白与 `\r` 的 stdout。
/// 受全局默认超时约束（见 [`exec::set_default_timeout`]）。
pub fn shell(cmd: &str) -> Result<String> {
    let out = exec::output(
        Command::new(adb_path()).arg("shell").arg(cmd),
        exec::default_timeout(),
    )
    .with_context(|| format!("执行 adb shell {cmd} 失败（adb 是否在 PATH？）"))?;
    if !out.status.success() {
        bail!(
            "adb shell {cmd} 退出码 {:?}: {}",
            out.status.code(),
            exec::decode_out(&out.stderr).trim()
        );
    }
    Ok(exec::decode_out(&out.stdout).trim().replace('\r', ""))
}

/// 执行 `adb shell <cmd>` 并流式逐行回调（按键实时回显用），返回完整输出。
/// 与 [`shell`] 的差别：不因退出码非零报错——按键监听窗口的判定依据是
/// 输出内容而非退出码；超时/停止语义同 [`exec::output`]。
pub fn shell_stream(cmd: &str, on_line: impl FnMut(&str)) -> Result<String> {
    exec::stream_lines(
        Command::new(adb_path()).arg("shell").arg(cmd),
        exec::default_timeout(),
        on_line,
    )
    .with_context(|| format!("执行 adb shell {cmd} 失败（adb 是否在 PATH？）"))
    .map(|out| out.trim().replace('\r', ""))
}

/// `adb pull <remote> <local>`。
pub fn pull(remote: &str, local: &Path) -> Result<()> {
    let out = exec::output(
        Command::new(adb_path()).args(["pull", remote]).arg(local),
        exec::default_timeout(),
    )
    .with_context(|| format!("adb pull {remote} 失败"))?;
    if !out.status.success() {
        bail!(
            "adb pull {remote}: {}",
            exec::decode_out(&out.stderr).trim()
        );
    }
    Ok(())
}

/// 解析 `adb devices` 输出为在线设备序列号列表（纯函数）。
///
/// 样例输入：
/// ```text
/// List of devices attached
/// 0123456789ABCDEF\tdevice
/// emulator-5554\toffline
/// ```
pub fn parse_devices_output(output: &str) -> Vec<String> {
    output
        .lines()
        .skip(1) // 表头 "List of devices attached"
        .filter(|l| l.ends_with("device"))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// 当前在线设备序列号列表。
pub fn devices() -> Result<Vec<String>> {
    let out = exec::output(
        Command::new(adb_path()).arg("devices"),
        exec::default_timeout(),
    )
    .context("执行 adb devices 失败")?;
    Ok(parse_devices_output(&exec::decode_out(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_devices_filters_state() {
        let raw =
            "List of devices attached\n0123456789ABCDEF\tdevice\nemulator-5554\toffline\n\t\n";
        assert_eq!(parse_devices_output(raw), ["0123456789ABCDEF"]);
    }

    #[test]
    fn parse_devices_empty() {
        assert!(parse_devices_output("List of devices attached\n").is_empty());
    }
}
