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
    /// 请求人工判定（worker 阻塞等待应答）
    Manual { item: TestItem, question: String },
    /// 拍照图片已拉回
    PhotoReady(PathBuf),
    /// 录像已拉回且解码验证通过
    VideoReady {
        frames_dir: PathBuf,
        count: usize,
        has_audio: bool,
    },
    /// 序列结束（附报告路径）
    Finished { report: Option<PathBuf> },
}

#[derive(Debug)]
pub enum UiResponse {
    /// 人工判定结论
    Manual(bool),
}
