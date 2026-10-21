//! 子进程统一执行入口：带看门狗超时（V2.3 真机反馈：固件命令可能永久阻塞）。
//!
//! 超时强杀本地子进程并返回 `TimedOut` 错误；设备侧命令是否随之终止取决于
//! adb 会话，不保证——但对序列来说本地已解锁，可继续下一项。
//!
//! 注意：输出在进程退出后统一读取，子进程输出超过管道缓冲（~64K）时
//! 多余部分会被截断；本工具的命令输出均为短文本，不做并发读管道。

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 全局默认超时（毫秒）；0 = 不限制。由序列入口（GUI worker / CLI）设置。
static DEFAULT_MS: AtomicU64 = AtomicU64::new(0);

/// 全局停止请求（V3 产线反馈：测试过程要能打断）。
/// GUI 停止按钮置位；`run_sequence` 入口清零，开始下一轮前不残留。
static ABORT: AtomicBool = AtomicBool::new(false);

/// 请求停止：正在执行的子进程立即强杀，worker 循环把剩余项记跳过。
pub fn request_abort() {
    ABORT.store(true, Ordering::Relaxed);
}

pub fn abort_requested() -> bool {
    ABORT.load(Ordering::Relaxed)
}

pub fn clear_abort() {
    ABORT.store(false, Ordering::Relaxed);
}

/// exec 与 worker 测试共用此锁串行执行：停止标志是全局静态，
/// 并行测试会互相干扰（一方置停止位，另一方子进程被误杀）。
#[cfg(test)]
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
pub fn output(cmd: &mut Command, timeout: Option<Duration>) -> std::io::Result<Output> {
    let Some(timeout) = timeout else {
        return cmd.output();
    };
    let deadline = Instant::now() + timeout;
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Output {
                status,
                stdout: read_all(&mut child.stdout),
                stderr: read_all(&mut child.stderr),
            });
        }
        let stop = if abort_requested() {
            Some((
                "已手动停止，强制终止".to_string(),
                std::io::ErrorKind::Interrupted,
            ))
        } else if Instant::now() >= deadline {
            Some((
                format!("执行超时（{}s）已强制终止", timeout.as_secs()),
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
}
