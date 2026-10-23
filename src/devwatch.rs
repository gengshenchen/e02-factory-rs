//! 设备在位监测（V3.8）：被测耳机经 USB 连接本机（adb 可见），所有
//! 测试项都依赖这个连接——插入才允许开测，测试中拔出立即中止。
//!
//! 后台线程周期跑 `adb devices`（只认 `device` 状态，offline/unauthorized
//! 不算在线），状态变化经通道通知 UI 并请求重绘。查询失败（adb 未装、
//! server 未起等）保持上一状态、不当作拔出——防抖，也让全局停止标志
//! 误伤不到监测（查询走 [`crate::exec::output_no_abort`]）。
//!
//! 注意：产线固件序列号恒为 123456 一类固定值，插入检测靠"状态沿"
//! （未连接→已连接）而非序列号比对，同号换机也能触发。

use crate::adb;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// 轮询周期（毫秒）：拔出到界面反馈的延迟上界 ≈ 周期 + 一次查询耗时。
const POLL_MS: u64 = 1500;

/// 在位状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevState {
    /// 无设备（或均非 device 状态：offline/unauthorized/未授权）
    Disconnected,
    /// 恰好一台在线，携带序列号
    Online(String),
    /// 多台在线：adb 命令将因目标不明确而失败，视作不可测
    Multiple,
}

/// 在线序列号列表 → 在位状态（纯函数）。
pub fn classify(serials: &[String]) -> DevState {
    match serials {
        [] => DevState::Disconnected,
        [one] => DevState::Online(one.clone()),
        _ => DevState::Multiple,
    }
}

/// 一次轮询推进状态机：`Some(next)` = 状态变化需通知 UI；
/// 查询失败返回 `None`（保持上一状态，不当作拔出）。
fn step(last: &DevState, poll: Result<Vec<String>, ()>) -> Option<DevState> {
    match poll {
        Ok(list) => {
            let next = classify(&list);
            (next != *last).then_some(next)
        }
        Err(()) => None,
    }
}

/// 启动监测线程，返回状态事件通道。线程随进程存活，无需停止句柄。
pub fn spawn(ctx: &egui::Context) -> Receiver<DevState> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let mut last = DevState::Disconnected;
        loop {
            std::thread::sleep(Duration::from_millis(POLL_MS));
            let poll = adb::devices_watch().map_err(|_| ());
            if let Some(next) = step(&last, poll) {
                if tx.send(next.clone()).is_err() {
                    return; // UI 已退出
                }
                ctx.request_repaint();
                last = next;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_online_count() {
        assert_eq!(classify(&[]), DevState::Disconnected);
        assert_eq!(
            classify(&["123456".into()]),
            DevState::Online("123456".into())
        );
        assert_eq!(classify(&["a".into(), "b".into()]), DevState::Multiple);
    }

    #[test]
    fn step_reports_changes_and_ignores_errors() {
        let online = DevState::Online("123456".into());
        // 查询失败：保持状态（adb 未装/全局停止均不应误判为拔出）
        assert_eq!(step(&online, Err(())), None);
        // 状态未变：不重复通知
        assert_eq!(step(&online, Ok(vec!["123456".into()])), None);
        // 拔出 / 插入 / 变多台
        assert_eq!(step(&online, Ok(vec![])), Some(DevState::Disconnected));
        assert_eq!(
            step(&DevState::Disconnected, Ok(vec!["123456".into()])),
            Some(DevState::Online("123456".into()))
        );
        assert_eq!(
            step(&DevState::Disconnected, Ok(vec!["a".into(), "b".into()])),
            Some(DevState::Multiple)
        );
    }
}
