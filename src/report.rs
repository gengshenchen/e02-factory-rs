//! 测试报告落盘（M5 完整实现 JSON + 归档；M0 先提供文本渲染）。

use crate::engine::TestResult;
use crate::protocol::DeviceInfo;

/// 生成 T32.txt 风格的文本报告：设备信息 + 各测试项判定。
pub fn render_text(device: &DeviceInfo, results: &[TestResult]) -> String {
    let mut s = String::new();
    s.push_str(&format!("sn-address:{}\n", device.sn));
    s.push_str(&format!("wifi-mac:{}\n", device.wifi_mac));
    s.push_str(&format!("bt-mac:{}\n", device.bt_mac));
    s.push_str(&format!("soft-ware:{}\n", device.t32_ver));
    s.push_str(&format!("hard-ware:{}\n", device.hardware_ver));
    s.push('\n');
    for r in results {
        if !matches!(
            r.verdict,
            crate::engine::Verdict::Pending | crate::engine::Verdict::Skipped
        ) {
            s.push_str(&format!(
                "{}:{} {}\n",
                r.item.label(),
                r.verdict.label(),
                r.detail
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TestItem;

    #[test]
    fn renders_device_info_and_results() {
        let device = DeviceInfo {
            sn: "002".into(),
            wifi_mac: "00:60:A7:0B:9C:F2".into(),
            t32_ver: "E02-1.0.1".into(),
            ..Default::default()
        };
        let mut r = TestResult::new(TestItem::SdCard);
        r.verdict = crate::engine::Verdict::Pass;
        let text = render_text(&device, &[r]);
        assert!(text.contains("sn-address:002"));
        assert!(text.contains("wifi-mac:00:60:A7:0B:9C:F2"));
        assert!(text.contains("SD 卡测试:PASS"));
    }

    #[test]
    fn pending_items_are_omitted() {
        let text = render_text(&DeviceInfo::default(), &[TestResult::new(TestItem::Keys)]);
        assert!(!text.contains("按键测试"));
        assert_eq!(text.trim_end().lines().count(), 5); // 仅 5 行设备信息
    }
}
