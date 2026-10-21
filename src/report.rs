//! 测试报告落盘与统计（V3 重设计）。
//!
//! 一机一测一份 txt：文件名 `e02-<本地时间>-<SN>.txt`，SN 在设备信息项
//! 解析到后改名（旧文件删除）；内容含整机总判定（结果: PASS/FAIL）与
//! 每项耗时。报告中心（今日统计 + 历史浏览）解析同格式——格式由本
//! 模块单向生成，自产自销，不再附带 JSON（V2 预留的 MES 对接暂无需求）。

use crate::engine::{TestResult, Verdict};
use crate::protocol::DeviceInfo;
use chrono::{DateTime, Local, Utc};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 报告元信息（表头）。
#[derive(Debug)]
pub struct ReportMeta {
    /// 工位名（来自 config）
    pub station: String,
    /// 序列开始（本地时间，文件名同款格式）
    pub started: String,
    pub elapsed_secs: u64,
}

/// 整机总判定：任一 FAIL 即 FAIL；进行中未定；全跳过记"跳过"。
pub fn overall(results: &[TestResult], finished: bool) -> &'static str {
    let (pass, fail) = tally(results);
    if !finished {
        "进行中"
    } else if fail > 0 {
        "FAIL"
    } else if pass > 0 {
        "PASS"
    } else {
        "跳过"
    }
}

fn tally(results: &[TestResult]) -> (usize, usize) {
    let mut pass = 0;
    let mut fail = 0;
    for r in results {
        match r.verdict {
            Verdict::Pass | Verdict::ManualYes => pass += 1,
            Verdict::Fail | Verdict::ManualNo => fail += 1,
            Verdict::Pending | Verdict::Skipped => {}
        }
    }
    (pass, fail)
}

/// 生成文本报告：表头（含总判定）+ 设备信息 + 每项固定一行（未测/跳过也写，
/// 带单项耗时）+ 汇总。
pub fn render_text(
    meta: &ReportMeta,
    device: &DeviceInfo,
    results: &[TestResult],
    finished: bool,
) -> String {
    let mut s = String::new();
    s.push_str("E02 工厂测试报告\n");
    s.push_str(&format!("结果: {}\n", overall(results, finished)));
    s.push_str(&format!(
        "工位: {}    开始: {}    耗时: {:02}:{:02}:{:02}\n",
        meta.station,
        meta.started,
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
        s.push_str(&format!("{}:{}", r.item.label(), r.verdict.label()));
        if !r.detail.is_empty() {
            s.push(' ');
            s.push_str(&r.detail);
        }
        if r.secs > 0 {
            s.push_str(&format!("（{}s）", r.secs));
        }
        s.push('\n');
    }
    let (pass, fail) = tally(results);
    let skip = results.len() - pass - fail;
    s.push_str(&format!(
        "\n汇总: {} 项，PASS {pass}，FAIL {fail}，跳过 {skip}\n",
        results.len()
    ));
    s
}

/// 写入报告文件 `e02-<stem>-<SN|unknown>.txt`，返回路径。
/// 序列内边测边写复用同一 stem；SN 中途解析到后文件名变化，
/// 由调用方（worker）删除旧文件。
pub fn write_report(
    dir: &Path,
    meta: &ReportMeta,
    device: &DeviceInfo,
    results: &[TestResult],
    stem: &str,
    finished: bool,
) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let sn = if device.sn.is_empty() {
        "unknown".to_string()
    } else {
        sanitize_sn(&device.sn)
    };
    let path = dir.join(format!("e02-{stem}-{sn}.txt"));
    std::fs::write(&path, render_text(meta, device, results, finished)).ok()?;
    Some(path)
}

/// SN 作文件名成分：非 `[A-Za-z0-9._-]` 字符替换为 `_`（防路径穿越/非法文件名字符）。
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

/// 报告历史目录内某一统计日的产出（今日看板 / 报告中心用）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DayStats {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
}

/// 扫描 `dir` 下文件名为 `e02-<day>-*.txt` 的报告，按"结果:"行统计。
/// 解析不到"结果:"行的旧版/中断报告计入 total 不计入 pass/fail。
pub fn scan_stats(dir: &Path, day: &str) -> DayStats {
    let mut s = DayStats::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return s;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(&format!("e02-{day}-")) || !name.ends_with(".txt") {
            continue;
        }
        s.total += 1;
        if let Ok(text) = std::fs::read_to_string(e.path()) {
            if let Some(line) = text.lines().find(|l| l.starts_with("结果: ")) {
                match line.trim_end() {
                    "结果: PASS" => s.pass += 1,
                    "结果: FAIL" => s.fail += 1,
                    _ => {}
                }
            }
        }
    }
    s
}

/// 报告列表（新→旧，文件名时间戳前缀字典序倒排即时间倒序），最多 `limit` 条。
pub fn list_reports(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("e02-") && n.ends_with(".txt"))
                .collect()
        })
        .unwrap_or_default();
    names.sort_unstable();
    names.reverse();
    names.into_iter().take(limit).map(|n| dir.join(n)).collect()
}

/// 当前本地时间戳（日志用）。
pub fn now_hms() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

/// 今日日期（报告统计分组键，本地时区）。
pub fn today_str() -> String {
    Local::now().format("%Y%m%d").to_string()
}

/// 本地时间戳（报告文件名用）。
pub fn time_str(t: SystemTime) -> String {
    to_local(t).format("%Y%m%d-%H%M%S").to_string()
}

fn to_local(t: SystemTime) -> DateTime<Local> {
    DateTime::<Utc>::from(t).with_timezone(&Local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TestItem;

    fn meta() -> ReportMeta {
        ReportMeta {
            station: "complete".into(),
            started: "20261022-103012".into(),
            elapsed_secs: 155,
        }
    }

    fn result(item: TestItem, verdict: Verdict) -> TestResult {
        let mut r = TestResult::new(item);
        r.verdict = verdict;
        r
    }

    #[test]
    fn renders_result_line_items_and_summary() {
        let device = DeviceInfo {
            sn: "002".into(),
            wifi_mac: "00:60:A7:0B:9C:F2".into(),
            t32_ver: "E02-1.0.1".into(),
            ..Default::default()
        };
        let mut pass = result(TestItem::SdCard, Verdict::Pass);
        pass.secs = 3;
        let text = render_text(
            &meta(),
            &device,
            &[pass, TestResult::new(TestItem::Keys)],
            true,
        );
        assert!(text.contains("结果: PASS"));
        assert!(text.contains("工位: complete"));
        assert!(text.contains("sn-address:002"));
        assert!(text.contains("SD 卡测试:PASS（3s）"));
        // 未测项也落行（V2.3 起不省略）
        assert!(text.contains("按键测试:未测"));
        assert!(text.contains("汇总: 2 项，PASS 1，FAIL 0，跳过 1"));
        // 进行中报告总判定未定
        let ongoing = render_text(&meta(), &device, &[], false);
        assert!(ongoing.contains("结果: 进行中"));
    }

    #[test]
    fn overall_reflects_any_fail() {
        let rs = [
            result(TestItem::Wifi, Verdict::Pass),
            result(TestItem::Keys, Verdict::ManualNo),
        ];
        assert_eq!(overall(&rs, true), "FAIL");
        assert_eq!(overall(&rs, false), "进行中");
        assert_eq!(overall(&[], true), "跳过");
    }

    #[test]
    fn filename_carries_sn_and_renames() {
        let dir = std::env::temp_dir().join(format!("e02-report-{}", std::process::id()));
        let r = result(TestItem::Wifi, Verdict::Pass);
        // SN 未解析到 → unknown
        let p1 = write_report(
            &dir,
            &meta(),
            &DeviceInfo::default(),
            std::slice::from_ref(&r),
            "t1",
            true,
        )
        .unwrap();
        assert!(p1.ends_with("e02-t1-unknown.txt"));
        assert!(p1.exists());
        // SN 解析到 → 新文件名（调用方删旧文件）
        let dev = DeviceInfo {
            sn: "SN 123".into(),
            ..Default::default()
        };
        let p2 = write_report(&dir, &meta(), &dev, &[r], "t1", true).unwrap();
        assert!(p2.ends_with("e02-t1-SN_123.txt"));
        assert!(p2.exists());
        // SN 含路径字符 → 替换为 _（防目录穿越）
        let p3 = write_report(
            &dir,
            &meta(),
            &DeviceInfo {
                sn: "../x".into(),
                ..Default::default()
            },
            &[],
            "t2",
            true,
        )
        .unwrap();
        assert!(p3.ends_with("e02-t2-.._x.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_stats_and_listing_by_day() {
        let dir = std::env::temp_dir().join(format!("e02-stats-{}", std::process::id()));
        let dev = DeviceInfo {
            sn: "SN1".into(),
            ..Default::default()
        };
        // 当日两 PASS 一 FAIL，他日一条不计数
        write_report(
            &dir,
            &meta(),
            &dev,
            &[result(TestItem::Wifi, Verdict::Pass)],
            "20261022-080000",
            true,
        );
        write_report(
            &dir,
            &meta(),
            &dev,
            &[result(TestItem::Wifi, Verdict::Pass)],
            "20261022-090000",
            true,
        );
        let mut fail_r = result(TestItem::Wifi, Verdict::Fail);
        fail_r.detail = "丢包".into();
        write_report(&dir, &meta(), &dev, &[fail_r], "20261022-100000", true);
        write_report(
            &dir,
            &meta(),
            &dev,
            &[result(TestItem::Wifi, Verdict::Pass)],
            "20261021-080000",
            true,
        );
        let s = scan_stats(&dir, "20261022");
        assert_eq!(
            s,
            DayStats {
                total: 3,
                pass: 2,
                fail: 1
            }
        );
        // 列表新→旧，仅当日与他日共 4 条，最多取 2
        let list = list_reports(&dir, 2);
        assert_eq!(list.len(), 2);
        assert!(list[0].ends_with("e02-20261022-100000-SN1.txt"));
        assert!(list[1].ends_with("e02-20261022-090000-SN1.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn time_str_shape() {
        // 本地时区相关不做精确断言，只验格式（15 字符 yyyymmdd-hhmmss）
        let s = time_str(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000));
        assert_eq!(s.len(), 15);
        assert_eq!(s.as_bytes()[8], b'-');
        assert!(s.starts_with("202"));
    }
}
