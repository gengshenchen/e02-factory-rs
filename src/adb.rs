//! adb 封装：进程调用与输出解析。
//!
//! M1 阶段由 worker 线程调用（阻塞式）；解析函数为纯函数，可单测。
//! adb CLI 在 Windows / Linux 行为一致，测试机上本就安装。

use crate::exec;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// 被测设备序列号（V3.15）：测试开始时锁定，此后所有 adb 子命令注入
/// `ANDROID_SERIAL` 定向到这台设备——测试中途电脑上插入/出现其他 adb
/// 设备时，adb 不再报 "more than one device/emulator" 打断测试
/// （真机 18:24 案：第二台设备中途上线 13 秒，SD 卡/电池/录像被打断）。
/// None = 不定向（单设备默认行为，`adb devices` 等列举不受影响）。
static TARGET: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 设定/清除被测设备定向。
pub fn set_target(serial: Option<String>) {
    *TARGET.lock().unwrap_or_else(|e| e.into_inner()) = serial;
}

/// 当前被测设备序列号。
pub fn target() -> Option<String> {
    TARGET.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 测试序列作用域的设备定向：构造时锁定、Drop 时恢复之前的值，
/// panic 也不泄漏（worker 线程 panic 后下一次运行不受影响）。
pub struct TargetGuard(Option<String>);

impl TargetGuard {
    pub fn new(serial: Option<String>) -> Self {
        let prev = target();
        set_target(serial);
        Self(prev)
    }
}

impl Drop for TargetGuard {
    fn drop(&mut self) {
        set_target(self.0.take());
    }
}

/// 给设备选择类 adb 命令注入定向环境（逐命令注入，不污染进程环境，
/// 不影响 `adb devices` 列举与其他 adb 调用方）。
fn directed(cmd: &mut Command) -> &mut Command {
    if let Some(s) = target() {
        cmd.env("ANDROID_SERIAL", s);
    }
    cmd
}

/// adb 可执行文件路径；环境变量 `ADB` 可覆盖（非常规安装路径/测试桩）。
fn adb_path() -> String {
    std::env::var("ADB").unwrap_or_else(|_| "adb".into())
}

/// 执行 `adb shell <cmd>`，返回去除首尾空白与 `\r` 的 stdout。
/// 受全局默认超时约束（见 [`exec::set_default_timeout`]）。
pub fn shell(cmd: &str) -> Result<String> {
    shell_inner(cmd, exec::default_timeout())
}

/// 同 [`shell`] 但自带专用超时（iperf3 客户端按测试时长收紧，
/// 不吃 60s 单项超时：瞬断/被拦时白等一分钟才被杀）。
pub fn shell_timeout(cmd: &str, timeout: std::time::Duration) -> Result<String> {
    shell_inner(cmd, Some(timeout))
}

fn shell_inner(cmd: &str, timeout: Option<std::time::Duration>) -> Result<String> {
    let mut c = Command::new(adb_path());
    let out = match exec::output(directed(&mut c).arg("shell").arg(cmd), timeout) {
        Ok(o) => o,
        // NotFound 才提示 PATH（真机 18:44 超时错误也挂着这提示，误导排查）
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("执行 adb shell {cmd} 失败（adb 是否在 PATH？可设 ADB 环境变量指定）")
        }
        Err(e) => return Err(anyhow::Error::from(e).context(format!("执行 adb shell {cmd} 失败"))),
    };
    if !out.status.success() {
        bail!(
            "adb shell {cmd} 退出码 {:?}: {}",
            out.status.code(),
            exec::decode_out(&out.stderr).trim()
        );
    }
    Ok(exec::decode_out(&out.stdout).trim().replace('\r', ""))
}

/// 启动 `adb shell <cmd>` 但不等它退出（V3.6 麦克风回环 / V3.6.3 按键
/// 监听用）。stdin 也接管管道：13/14/15 固件端以 `ret=getchar();` 等任意
/// 输入退出（V3.15.3），结束时由调用方往 stdin 写一个字符让固件自己走完。
pub fn spawn_shell(cmd: &str) -> Result<std::process::Child> {
    let mut c = Command::new(adb_path());
    directed(&mut c)
        .arg("shell")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    exec::hide_window(&mut c); // 长会话（麦克风/按键）不得挂终端窗口
    c.spawn()
        .with_context(|| format!("执行 adb shell {cmd} 失败（adb 是否在 PATH？）"))
}

/// `adb pull <remote> <local>`。
pub fn pull(remote: &str, local: &Path) -> Result<()> {
    let mut c = Command::new(adb_path());
    let out = exec::output(
        directed(&mut c).args(["pull", remote]).arg(local),
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
    let mut c = Command::new(adb_path());
    let out = exec::output(
        directed(&mut c).arg("push").arg(local).arg(remote),
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
