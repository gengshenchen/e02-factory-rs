//! 测试报告落盘（V2.3）：表头 + 每项固定行 + 汇总；文本 + JSON 双格式。
//!
//! 文本面向产线阅读，JSON 面向 MES/统计对接；同名成对出现在同一目录。

use crate::engine::{TestResult, Verdict};
use crate::protocol::DeviceInfo;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 报告元信息（表头与 JSON 顶层）。
#[derive(Debug, Serialize)]
pub struct ReportMeta {
    /// 工位名（来自 config）
    pub station: String,
    /// 序列开始（UTC，文件名同款格式）
    pub started_utc: String,
    pub elapsed_secs: u64,
}

/// 生成文本报告：表头 + 设备信息 + 每项固定一行（未测/跳过也写）+ 汇总。
pub fn render_text(meta: &ReportMeta, device: &DeviceInfo, results: &[TestResult]) -> String {
    let mut s = String::new();
    s.push_str("E02 工厂测试报告\n");
    s.push_str(&format!(
        "工位: {}    开始: {}    耗时: {:02}:{:02}:{:02}\n",
        meta.station,
        meta.started_utc,
        meta.elapsed_secs / 3600,
        meta.elapsed_secs % 3600 / 60,
        meta.elapsed_secs % 60
    ));
    s.push_str(&format!("sn-address:{}\n", device.sn));
    s.push_str(&format!("wifi-mac:{}\n", device.wifi_mac));
    s.push_str(&format!("bt-mac:{}\n", device.bt_mac));
    s.push_str(&format!("soft-ware:{}\n", device.t32_ver));
    s.push_str(&format!("hard-ware:{}\n", device.hardware_ver));
    s.push('\n');
    for r in results {
        match (r.verdict, r.detail.is_empty()) {
            (Verdict::Pending, _) => s.push_str(&format!("{}:未测\n", r.item.label())),
            (_, true) => s.push_str(&format!("{}:{}\n", r.item.label(), r.verdict.label())),
            (_, false) => s.push_str(&format!(
                "{}:{} {}\n",
                r.item.label(),
                r.verdict.label(),
                r.detail
            )),
        }
    }
    let (mut pass, mut fail, mut skip) = (0usize, 0usize, 0usize);
    for r in results {
        match r.verdict {
            Verdict::Pass | Verdict::ManualYes => pass += 1,
            Verdict::Fail | Verdict::ManualNo => fail += 1,
            Verdict::Pending | Verdict::Skipped => skip += 1,
        }
    }
    s.push_str(&format!(
        "\n汇总: {} 项，PASS {pass}，FAIL {fail}，跳过 {skip}\n",
        results.len()
    ));
    s
}

/// JSON 报告结构（与文本同源，MES 对接用）。
#[derive(Serialize)]
struct JsonReport<'a> {
    meta: &'a ReportMeta,
    device: &'a DeviceInfo,
    results: &'a [TestResult],
}

/// 写入报告对：`e02-<UTC>.txt` + 同名 `.json`；返回文本路径。
/// `sn_subdir`（SN 归档，V2.3）非空时落到 `dir/<sn>/` 子目录。
pub fn write_report(
    dir: &Path,
    sn_subdir: Option<&str>,
    meta: &ReportMeta,
    device: &DeviceInfo,
    results: &[TestResult],
) -> Option<PathBuf> {
    let dir = match sn_subdir {
        Some(sn) if !sn.is_empty() => dir.join(sanitize_sn(sn)),
        _ => dir.to_path_buf(),
    };
    std::fs::create_dir_all(&dir).ok()?;
    let stem = time_str(SystemTime::now());
    let txt = dir.join(format!("e02-{stem}.txt"));
    std::fs::write(&txt, render_text(meta, device, results)).ok()?;
    let json = dir.join(format!("e02-{stem}.json"));
    if let Ok(body) = serde_json::to_string_pretty(&JsonReport {
        meta,
        device,
        results,
    }) {
        let _ = std::fs::write(&json, body);
    }
    Some(txt)
}

/// SN 作子目录名：非 `[A-Za-z0-9._-]` 字符替换为 `_`（防路径穿越/非法文件名字符）。
fn sanitize_sn(sn: &str) -> String {
    let s: String = sn
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "unknown".into()
    } else {
        s
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
mod tests {
    use super::*;
    use crate::engine::TestItem;
    use crate::engine::Verdict;

    fn meta() -> ReportMeta {
        ReportMeta {
            station: "complete".into(),
            started_utc: "20261021-000000Z".into(),
            elapsed_secs: 155,
        }
    }

    #[test]
    fn renders_header_items_and_summary() {
        let device = DeviceInfo {
            sn: "002".into(),
            wifi_mac: "00:60:A7:0B:9C:F2".into(),
            t32_ver: "E02-1.0.1".into(),
            ..Default::default()
        };
        let mut r = TestResult::new(TestItem::SdCard);
        r.verdict = Verdict::Pass;
        let text = render_text(&meta(), &device, &[r, TestResult::new(TestItem::Keys)]);
        assert!(text.contains("工位: complete"));
        assert!(text.contains("耗时: 00:02:35"));
        assert!(text.contains("sn-address:002"));
        assert!(text.contains("SD 卡测试:PASS"));
        // 未测项也落行（V2.3：不再省略）
        assert!(text.contains("按键测试:未测"));
        assert!(text.contains("汇总: 2 项，PASS 1，FAIL 0，跳过 1"));
    }

    #[test]
    fn detail_lines_carry_reason() {
        let mut r = TestResult::new(TestItem::Wifi);
        r.verdict = Verdict::Fail;
        r.detail = "丢包 30%，avg 500ms（阈值 0% / 100ms）".into();
        let text = render_text(&meta(), &DeviceInfo::default(), &[r]);
        assert!(text.contains("WiFi 测试:FAIL 丢包 30%"));
    }

    #[test]
    fn time_str_known_timestamps() {
        assert_eq!(time_str(SystemTime::UNIX_EPOCH), "19700101-000000Z");
        assert_eq!(
            time_str(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
            "20231114-221320Z"
        );
    }

    #[test]
    fn write_report_pair_and_sn_archive() {
        let dir = std::env::temp_dir().join(format!("e02-report-{}", std::process::id()));
        let mut r = TestResult::new(TestItem::Wifi);
        r.verdict = Verdict::Pass;
        // 普通目录：txt + json 成对
        let path = write_report(&dir, None, &meta(), &DeviceInfo::default(), &[r.clone()]).unwrap();
        assert!(path.exists());
        assert!(path.extension().is_some_and(|e| e == "txt"));
        let json = path.with_extension("json");
        assert!(json.exists(), "JSON 报告应成对存在");
        // SN 归档：落到 sn 子目录
        let dev = DeviceInfo {
            sn: "SN123".into(),
            ..Default::default()
        };
        let p2 = write_report(&dir, Some(&dev.sn), &meta(), &dev, &[r]).unwrap();
        assert!(p2.starts_with(dir.join("SN123")));
        // SN 含路径字符 → 替换为 _（防目录穿越）
        let p3 = write_report(&dir, Some("../x"), &meta(), &dev, &[]).unwrap();
        assert!(p3.starts_with(dir.join(".._x")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
