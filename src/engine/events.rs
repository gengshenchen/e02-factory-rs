//! UI ↔ worker 线程事件协议。
//!
//! worker 阻塞执行 adb/ffmpeg 调用，通过 [`UiEvent`] 回传状态；
//! 人工判定经 [`UiResponse`] 由 UI 答回，worker 阻塞等待
//! （对应原程序 MessageBox 的同步语义）。

use crate::engine::TestItem;
use crate::protocol::DeviceInfo;
use std::path::PathBuf;

#[derive(Debug)]
pub enum UiEvent {
    /// 序列开始
    Started,
    /// 正在执行的项（spinner 标签用）
    Current(TestItem),
    /// 一项测试完成（可能带回设备信息）
    Result(crate::engine::TestResult, Option<DeviceInfo>),
    /// 请求人工判定（worker 阻塞等待应答）。title 区分同项内的多个阶段
    /// （如麦克风的主麦/副麦两段判定）。
    Manual { title: String, question: String },
    /// 按键引导流程状态：已检测到的键码（4=多功能 5=开机 6=音量）。
    /// `awaiting` 为真时 worker 阻塞等待 [`UiResponse::KeysRetry`]/[`UiResponse::KeysForce`]。
    KeysUpdate { detected: Vec<u8>, awaiting: bool },
    /// 拍照图片已拉回
    PhotoReady(PathBuf),
    /// 录像已拉回且解码验证通过（audio 为抽出的音轨 wav，None=无声/抽取失败）
    VideoReady {
        frames_dir: PathBuf,
        count: usize,
        has_audio: bool,
        audio: Option<PathBuf>,
    },
    /// 过程日志（底部日志面板实时滚动）
    Log(String),
    /// 序列结束（附报告路径）
    Finished { report: Option<PathBuf> },
}

#[derive(Debug)]
pub enum UiResponse {
    /// 人工判定结论（指示灯/MIC；强制改判亦复用）
    Manual(bool),
    /// 按键引导：重测一轮
    KeysRetry,
    /// 按键引导：强制改判（true=通过，false=失败）
    KeysForce(bool),
}
