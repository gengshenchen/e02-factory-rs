//! 测试引擎数据模型：测试项定义、判定状态与结果。
//! M2 起接入 worker 线程编排（命令分发、结果回传）。

pub mod events;
pub mod runner;
pub mod worker;

/// 单个测试项的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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

    /// 报告用语（存档件沿用 PASS/FAIL，与统计解析和历史报告兼容）。
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Pending => "未测",
            Verdict::Pass | Verdict::ManualYes => "PASS",
            Verdict::Fail | Verdict::ManualNo => "FAIL",
            Verdict::Skipped => "跳过",
        }
    }

    /// 界面用语（检验规范：合格/不合格）。
    pub fn ui_label(self) -> &'static str {
        match self {
            Verdict::Pending => "待测",
            Verdict::Pass | Verdict::ManualYes => "合格",
            Verdict::Fail | Verdict::ManualNo => "不合格",
            Verdict::Skipped => "跳过",
        }
    }
}

/// 测试项：与 camera-factory 子命令对应（V3.5：充电并入电池、陀螺仪走 17）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum TestItem {
    Video,
    Photo,
    DeviceInfo,
    Wifi,
    SdCard,
    Led,
    BtMac,
    Battery,
    MicSpeaker,
    Keys,
    Gyro,
}

impl TestItem {
    /// 侧栏顺序对应原对话框按钮排列 + 陀螺仪。
    pub const ALL: [TestItem; 11] = [
        TestItem::Video,
        TestItem::Photo,
        TestItem::DeviceInfo,
        TestItem::Wifi,
        TestItem::SdCard,
        TestItem::Led,
        TestItem::BtMac,
        TestItem::Battery,
        TestItem::MicSpeaker,
        TestItem::Keys,
        TestItem::Gyro,
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
            TestItem::Battery => "电池测试",
            TestItem::MicSpeaker => "麦克风测试",
            TestItem::Keys => "按键测试",
            TestItem::Gyro => "陀螺仪测试",
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
            TestItem::Battery => "battery",
            TestItem::MicSpeaker => "mic",
            TestItem::Keys => "keys",
            TestItem::Gyro => "gyro",
        }
    }

    pub fn from_slug(s: &str) -> Option<TestItem> {
        TestItem::ALL.iter().find(|i| i.slug() == s).copied()
    }

    /// 人工参与项（V3.5）：需要操作员看/听/按键——录像与拍照由设备返回值
    /// 先自动判定，成功后拉回媒体由人工确认画面/声音质量，故同属人工项。
    /// 序列执行时自动项先行、这些殿后，操作员可在自动项期间离手。
    /// （命令号映射见 protocol::CameraFactory；指示灯已改纯人工判定不发指令。）
    pub fn is_manual(self) -> bool {
        matches!(
            self,
            TestItem::Video
                | TestItem::Photo
                | TestItem::Led
                | TestItem::MicSpeaker
                | TestItem::Keys
        )
    }
}

/// 一条测试结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TestResult {
    pub item: TestItem,
    pub verdict: Verdict,
    /// 附加信息：电压值、失败原因等
    pub detail: String,
    /// 本项耗时（秒，报告用；0 = 未计时）
    pub secs: u64,
}

impl TestResult {
    pub fn new(item: TestItem) -> Self {
        Self {
            item,
            verdict: Verdict::Pending,
            detail: String::new(),
            secs: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_items_have_distinct_labels_and_slugs() {
        let mut labels: Vec<_> = TestItem::ALL.map(|i| i.label()).to_vec();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), TestItem::ALL.len());

        let mut slugs: Vec<_> = TestItem::ALL.map(|i| i.slug()).to_vec();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), TestItem::ALL.len());
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

    #[test]
    fn manual_items_are_the_five_operator_items() {
        let manual: Vec<TestItem> = TestItem::ALL
            .iter()
            .copied()
            .filter(|i| i.is_manual())
            .collect();
        assert_eq!(
            manual,
            vec![
                TestItem::Video,
                TestItem::Photo,
                TestItem::Led,
                TestItem::MicSpeaker,
                TestItem::Keys
            ]
        );
    }
}
