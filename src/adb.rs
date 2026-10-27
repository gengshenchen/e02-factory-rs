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

/// 启动 `adb shell <cmd>` 但不等它退出（V3.6 麦克风回环用）。
/// 新版固件的 13/14 回环进程存活期间才有声音、不再自动退出；
/// 调用方拿到 Child 自行读输出、在人工应答后 kill 结束回环。
pub fn spawn_shell(cmd: &str) -> Result<std::process::Child> {
    Command::new(adb_path())
        .arg("shell")
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("执行 adb shell {cmd} 失败（adb 是否在 PATH？）"))
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

/// `adb push <local> <remote>`（写设备侧文件，WiFi 配网用）。
pub fn push(local: &Path, remote: &str) -> Result<()> {
    let out = exec::output(
        Command::new(adb_path()).arg("push").arg(local).arg(remote),
        exec::default_timeout(),
    )
    .with_context(|| format!("adb push {} 失败", local.display()))?;
    if !out.status.success() {
        bail!(
            "adb push {remote}: {}",
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

/// `adb devices`（在位监测线程专用，V3.8）：不受全局停止标志影响
/// （见 [`exec::output_no_abort`]），固定 5s 超时兜底 adb server 卡死。
pub fn devices_watch() -> Result<Vec<String>> {
    let out = exec::output_no_abort(
        Command::new(adb_path()).arg("devices"),
        Some(std::time::Duration::from_secs(5)),
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
