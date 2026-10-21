//! 测试报告落盘：T32.txt 风格文本 + 带时间戳文件名。

use crate::engine::TestResult;
use crate::protocol::DeviceInfo;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

/// 当前 UTC 时:分:秒（日志时间戳用）。
pub fn now_hms() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rem = secs.rem_euclid(86400);
    format!("{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// 把报告写入目录，文件名 `e02-YYYYMMDD-HHMMSSZ.txt`；返回路径。
pub fn write_report(dir: &Path, device: &DeviceInfo, results: &[TestResult]) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("e02-{}.txt", time_str(SystemTime::now())));
    std::fs::write(&path, render_text(device, results)).ok()?;
    Some(path)
}

/// UTC 时间戳（报告文件名用）。
pub fn time_str(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// 儒略日 → 公历（Howard Hinnant civil_from_days 算法）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod time_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn time_str_known_timestamps() {
        assert_eq!(time_str(SystemTime::UNIX_EPOCH), "19700101-000000Z");
        assert_eq!(
            time_str(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            "20231114-221320Z"
        );
    }

    #[test]
    fn write_report_creates_file() {
        let dir = std::env::temp_dir().join(format!("e02-report-{}", std::process::id()));
        let mut r = TestResult::new(crate::engine::TestItem::Wifi);
        r.verdict = crate::engine::Verdict::Pass;
        let path = write_report(&dir, &DeviceInfo::default(), &[r]).unwrap();
        assert!(path.exists());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("WiFi 测试:PASS"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
