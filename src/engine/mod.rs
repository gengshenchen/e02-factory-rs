//! 测试引擎数据模型：测试项定义、判定状态与结果。
//! M2 起接入 worker 线程编排（命令分发、结果回传）。

pub mod runner;

use crate::protocol::CameraFactory;
use std::time::SystemTime;

/// 单个测试项的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 未执行
    Pending,
    /// 自动判定通过
    Pass,
    /// 自动判定失败
    Fail,
    /// 人工判定：合格
    ManualYes,
    /// 人工判定：不合格
    ManualNo,
    /// 跳过（headless 下的人工判定/媒体项等）
    Skipped,
}

impl Verdict {
    pub fn is_pass(self) -> bool {
        matches!(self, Verdict::Pass | Verdict::ManualYes)
    }

    /// 结果文字（绿/红显示，对应原程序 Printtestinfo）。
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Pending => "未测",
            Verdict::Pass | Verdict::ManualYes => "PASS",
            Verdict::Fail | Verdict::ManualNo => "FAIL",
            Verdict::Skipped => "跳过",
        }
    }
}

/// 测试项，与 camera-factory 子命令一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestItem {
    Video,
    Photo,
    DeviceInfo,
    Wifi,
    SdCard,
    Led,
    BtMac,
    Charger,
    Battery,
    MicSpeaker,
    Keys,
}

impl TestItem {
    /// 侧栏顺序对应原对话框按钮排列。
    pub const ALL: [TestItem; 11] = [
        TestItem::Video,
        TestItem::Photo,
        TestItem::DeviceInfo,
        TestItem::Wifi,
        TestItem::SdCard,
        TestItem::Led,
        TestItem::BtMac,
        TestItem::Charger,
        TestItem::Battery,
        TestItem::MicSpeaker,
        TestItem::Keys,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TestItem::Video => "录像测试",
            TestItem::Photo => "拍照测试",
            TestItem::DeviceInfo => "设备信息",
            TestItem::Wifi => "WiFi 测试",
            TestItem::SdCard => "SD 卡测试",
            TestItem::Led => "指示灯测试",
            TestItem::BtMac => "蓝牙 MAC",
            TestItem::Charger => "充电测试",
            TestItem::Battery => "电池测试",
            TestItem::MicSpeaker => "MIC/喇叭",
            TestItem::Keys => "按键测试",
        }
    }

    /// CLI 用的短名（`e02-factory-rs test info sdcard`）。
    pub fn slug(self) -> &'static str {
        match self {
            TestItem::Video => "video",
            TestItem::Photo => "photo",
            TestItem::DeviceInfo => "info",
            TestItem::Wifi => "wifi",
            TestItem::SdCard => "sdcard",
            TestItem::Led => "led",
            TestItem::BtMac => "btmac",
            TestItem::Charger => "charger",
            TestItem::Battery => "battery",
            TestItem::MicSpeaker => "mic",
            TestItem::Keys => "keys",
        }
    }

    pub fn from_slug(s: &str) -> Option<TestItem> {
        TestItem::ALL.iter().find(|i| i.slug() == s).copied()
    }

    /// 对应的设备侧测试命令。
    pub fn factory_cmd(self) -> CameraFactory {
        match self {
            TestItem::Video => CameraFactory::Video,
            TestItem::Photo => CameraFactory::Photo,
            TestItem::DeviceInfo => CameraFactory::DeviceInfo,
            TestItem::Wifi => CameraFactory::Wifi,
            TestItem::SdCard => CameraFactory::SdCard,
            TestItem::Led => CameraFactory::Led,
            TestItem::BtMac => CameraFactory::BtMac,
            TestItem::Charger => CameraFactory::Charger,
            TestItem::Battery => CameraFactory::Battery,
            TestItem::MicSpeaker => CameraFactory::MicSpeaker,
            TestItem::Keys => CameraFactory::Keys,
        }
    }
}

/// 一条测试结果。
#[derive(Debug, Clone)]
pub struct TestResult {
    pub item: TestItem,
    pub verdict: Verdict,
    /// 附加信息：电压值、失败原因等
    pub detail: String,
    pub at: SystemTime,
}

impl TestResult {
    pub fn new(item: TestItem) -> Self {
        Self {
            item,
            verdict: Verdict::Pending,
            detail: String::new(),
            at: SystemTime::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_items_have_distinct_labels_and_cmds() {
        let mut labels: Vec<_> = TestItem::ALL.map(|i| i.label()).to_vec();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), TestItem::ALL.len());

        let mut cmds: Vec<_> = TestItem::ALL.map(|i| i.factory_cmd() as u8).to_vec();
        cmds.sort_unstable();
        cmds.dedup();
        assert_eq!(cmds.len(), TestItem::ALL.len());
    }

    #[test]
    fn slug_roundtrip() {
        for item in TestItem::ALL {
            assert_eq!(TestItem::from_slug(item.slug()), Some(item));
        }
        assert_eq!(TestItem::from_slug("nope"), None);
    }

    #[test]
    fn verdict_pass_semantics() {
        assert!(Verdict::ManualYes.is_pass());
        assert!(!Verdict::ManualNo.is_pass());
        assert!(!Verdict::Pending.is_pass());
    }
}
