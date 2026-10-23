//! 子进程统一执行入口：带看门狗超时（V2.3 真机反馈：固件命令可能永久阻塞）。
//!
//! 超时强杀本地子进程并返回 `TimedOut` 错误；设备侧命令是否随之终止取决于
//! adb 会话，不保证——但对序列来说本地已解锁，可继续下一项。
//!
//! 注意：输出在进程退出后统一读取，子进程输出超过管道缓冲（~64K）时
//! 多余部分会被截断；本工具的命令输出均为短文本，不做并发读管道。

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::time::{Duration, Instant};

/// 子进程输出解码：优先严格 UTF-8，失败则按 GBK 回退
/// （产线固件部分输出为 GBK，lossy 转码会出现乱码，V3.6）。
pub fn decode_out(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let (s, _, had_errors) = encoding_rs::GBK.decode(bytes);
            if had_errors {
                String::from_utf8_lossy(bytes).into_owned()
            } else {
                s.into_owned()
            }
        }
    }
}

/// 全局默认超时（毫秒）；0 = 不限制。由序列入口（GUI worker / CLI）设置。
static DEFAULT_MS: AtomicU64 = AtomicU64::new(0);

/// 全局停止请求（V3 产线反馈：测试过程要能打断）。
/// GUI 停止按钮置位；`run_sequence` 入口清零，开始下一轮前不残留。
static ABORT: AtomicBool = AtomicBool::new(false);

/// 停止原因（V3.8 设备在位监测）：决定中止后各项 detail 文案与看门狗
/// 强杀消息——操作员手停 vs 设备拔出中止，worker/报告按此落字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopReason {
    #[default]
    User,
    DeviceGone,
}

impl StopReason {
    /// 跳过项/中止路径的 detail 文案（进报告）。
    pub fn text(self) -> &'static str {
        match self {
            StopReason::User => "手动结束测试",
            StopReason::DeviceGone => "设备已拔出",
        }
    }

    /// 看门狗强杀子进程时的错误消息。
    fn kill_msg(self) -> &'static str {
        match self {
            StopReason::User => "已手动停止，强制终止",
            StopReason::DeviceGone => "设备已拔出，强制终止",
        }
    }
}

/// 与 ABORT 配套的原因标记（0=User 1=DeviceGone）
static ABORT_REASON: AtomicU8 = AtomicU8::new(0);

/// 请求停止：正在执行的子进程立即强杀，worker 循环把剩余项记跳过。
pub fn request_abort(reason: StopReason) {
    ABORT_REASON.store(reason as u8, Ordering::Relaxed);
    ABORT.store(true, Ordering::Relaxed);
}

pub fn abort_requested() -> bool {
    ABORT.load(Ordering::Relaxed)
}

/// 当前停止原因（停止位置位后有效；未置位按手动停止）。
pub fn abort_reason() -> StopReason {
    match ABORT_REASON.load(Ordering::Relaxed) {
        1 => StopReason::DeviceGone,
        _ => StopReason::User,
    }
}

pub fn clear_abort() {
    ABORT.store(false, Ordering::Relaxed);
    ABORT_REASON.store(StopReason::User as u8, Ordering::Relaxed);
}

/// exec 与 worker 测试共用此锁串行执行：停止标志是全局静态，
/// 并行测试会互相干扰（一方置停止位，另一方子进程被误杀）。
/// 两个测试模块都是 unix-only（假 adb 桩），此锁同样 unix-only。
#[cfg(all(unix, test))]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn set_default_timeout(d: Option<Duration>) {
    DEFAULT_MS.store(
        d.map(|t| t.as_millis() as u64).unwrap_or(0),
        Ordering::Relaxed,
    );
}

pub fn default_timeout() -> Option<Duration> {
    match DEFAULT_MS.load(Ordering::Relaxed) {
        0 => None,
        ms => Some(Duration::from_millis(ms)),
    }
}

/// 执行命令并收集输出；超时强杀子进程，返回 `ErrorKind::TimedOut` 错误。
/// 停止请求（[`request_abort`]）同样立即强杀，返回 `ErrorKind::Interrupted`。
///
/// 无论是否设了超时都走看门狗轮询（V3.6）：此前"0=不限制"直接
/// `cmd.output()` 阻塞，停止请求在无超时配置下永远不生效。
pub fn output(cmd: &mut Command, timeout: Option<Duration>) -> std::io::Result<Output> {
    run(cmd, timeout, true)
}

/// 同 [`output`] 但不受全局停止标志影响（V3.8 设备在位监测）：
/// 停止测试置位 ABORT 后，普通查询会被当作停止请求误杀，监测从此
/// 冻结、插拔不再被感知；超时兜底仍然生效。
pub fn output_no_abort(cmd: &mut Command, timeout: Option<Duration>) -> std::io::Result<Output> {
    run(cmd, timeout, false)
}

fn run(
    cmd: &mut Command,
    timeout: Option<Duration>,
    respect_abort: bool,
) -> std::io::Result<Output> {
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Output {
                status,
                stdout: read_all(&mut child.stdout),
                stderr: read_all(&mut child.stderr),
            });
        }
        let stop = if respect_abort && abort_requested() {
            Some((
                abort_reason().kill_msg().to_string(),
                std::io::ErrorKind::Interrupted,
            ))
        } else if deadline.is_some_and(|d| Instant::now() >= d) {
            Some((
                format!(
                    "执行超时（{}s）已强制终止",
                    timeout.map(|t| t.as_secs()).unwrap_or(0)
                ),
                std::io::ErrorKind::TimedOut,
            ))
        } else {
            None
        };
        if let Some((reason, kind)) = stop {
            let program = cmd.get_program().to_string_lossy().into_owned();
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(kind, format!("{reason}: {program}")));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_all<R: Read>(pipe: &mut Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(r) = pipe.as_mut() {
        let _ = r.read_to_end(&mut buf);
    }
    buf
}

#[cfg(all(unix, test))] // sh 桩仅 unix 测试可用
mod tests {
    use super::*;

    #[test]
    fn kills_on_timeout() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5"]);
        let t = Instant::now();
        let r = output(&mut cmd, Some(Duration::from_millis(300)));
        assert!(r.is_err());
        assert!(t.elapsed() < Duration::from_secs(3), "应在超时点附近返回");
    }

    #[test]
    fn no_timeout_runs_to_completion() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo hi"]);
        let out = output(&mut cmd, None).unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }

    #[test]
    fn timeout_output_is_captured() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 超时前已写入管道的输出仍可读到
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo partial; sleep 5"]);
        let r = output(&mut cmd, Some(Duration::from_millis(400)));
        assert!(r.is_err());
    }

    #[test]
    fn abort_reason_selects_kill_message() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        request_abort(StopReason::DeviceGone);
        assert_eq!(abort_reason(), StopReason::DeviceGone);
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5"]);
        let err = output(&mut cmd, None).unwrap_err();
        assert!(
            err.to_string().contains("设备已拔出"),
            "消息应随原因变化: {err}"
        );
        clear_abort();
    }

    #[test]
    fn no_abort_variant_ignores_stop_flag() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        request_abort(StopReason::User);
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo hi"]);
        let out = output_no_abort(&mut cmd, Some(Duration::from_secs(5))).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
        clear_abort();
    }
}
