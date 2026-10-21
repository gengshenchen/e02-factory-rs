//! adb 封装：进程调用与输出解析。
//!
//! M1 阶段由 worker 线程调用（阻塞式）；解析函数为纯函数，可单测。
//! adb CLI 在 Windows / Linux 行为一致，测试机上本就安装。

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// adb 可执行文件路径；环境变量 `ADB` 可覆盖（非常规安装路径/测试桩）。
fn adb_path() -> String {
    std::env::var("ADB").unwrap_or_else(|_| "adb".into())
}

/// 执行 `adb shell <cmd>`，返回去除首尾空白与 `\r` 的 stdout。
pub fn shell(cmd: &str) -> Result<String> {
    let out = Command::new(adb_path())
        .arg("shell")
        .arg(cmd)
        .output()
        .with_context(|| format!("执行 adb shell {cmd} 失败（adb 是否在 PATH？）"))?;
    if !out.status.success() {
        bail!(
            "adb shell {cmd} 退出码 {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim()
        .replace('\r', ""))
}

/// `adb pull <remote> <local>`。
pub fn pull(remote: &str, local: &Path) -> Result<()> {
    let out = Command::new(adb_path())
        .args(["pull", remote])
        .arg(local)
        .output()
        .with_context(|| format!("adb pull {remote} 失败"))?;
    if !out.status.success() {
        bail!(
            "adb pull {remote}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
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
    let out = Command::new(adb_path())
        .arg("devices")
        .output()
        .context("执行 adb devices 失败")?;
    Ok(parse_devices_output(&String::from_utf8_lossy(&out.stdout)))
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
