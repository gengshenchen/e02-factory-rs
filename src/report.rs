//! 测试报告落盘与统计（V3 重设计）。
//!
//! 一机一测一份 txt：文件名 `e02-<本地时间>-<SN>.txt`，SN 在设备信息项
//! 解析到后改名（旧文件删除）；内容含整机总判定（结果: PASS/FAIL）与
//! 每项耗时。报告中心（今日统计 + 历史浏览）解析同格式——格式由本
//! 模块单向生成，自产自销，不再附带 JSON（V2 预留的 MES 对接暂无需求）。

use crate::config::ModelKind;
use crate::engine::{TestItem, TestResult, Verdict};
use crate::protocol::DeviceInfo;
use chrono::{DateTime, Local, Utc};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 报告元信息（表头）。
#[derive(Debug)]
pub struct ReportMeta {
    /// 工位名（来自 config）
    pub station: String,
    /// 产品型号（V3.14：标题/文件名前缀按型号）
    pub model: ModelKind,
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
    s.push_str(&format!("{} 工厂测试报告\n", meta.model.label()));
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
    // E02E（eSIM 型）专属身份字段（V3.14）：读到才落行，E02 报告不出现
    if !device.imei.is_empty() {
        s.push_str(&format!("imei:{}\n", device.imei));
    }
    if !device.ccid.is_empty() {
        s.push_str(&format!("ccid:{}\n", device.ccid));
    }
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
    sanitize(&s)
}

/// 报告文本清洗（V3.7）：设备串口输出会夹带控制字符（真机 WiFi 行
/// `d\x11!` 一类 DC1/EOT 垃圾），直接落盘在报告中心/外部查看器呈乱码。
/// 保留换行，其余控制字符（\r、DEL、ANSI 转义等）一律滤除；
/// 报告中心读取旧文件时同样过一遍。
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect()
}

/// 写入报告，返回路径。文件名规则（V3.13 设备身份改蓝牙 MAC；V3.2 同机
/// 重测更新同一份的语义保留）：
/// - 蓝牙 MAC 已知：`e02-<日期>-<工位>-<蓝牙MAC>.txt`（FQC §8 加工位段：
///   同机同日跨模组/整机工位各自保留，旧命名互覆盖是丢数据）——同机同日
///   同工位一份，重测/单项补测覆盖更新（SN 未烧写全厂默认 00001，不能
///   再作键，多台会互踩）；
/// - 蓝牙 MAC 未知：`e02-<日期>-<工位>-<时分秒>-unknown.txt` —— 带时间戳不互踩；
/// - 绑定冲突（MAC 绑定判 FAIL）：追加 `-冲突<时分秒>`，冲突机不覆盖
///   台账原机的报告。
///
/// 蓝牙 MAC 中途解析到后文件名变化，旧文件由调用方（worker）删除。
pub fn write_report(
    dir: &Path,
    meta: &ReportMeta,
    device: &DeviceInfo,
    results: &[TestResult],
    finished: bool,
) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(file_name(meta, device, results));
    std::fs::write(&path, render_text(meta, device, results, finished)).ok()?;
    Some(path)
}

/// 报告文件名（meta.started 为 `yyyymmdd-hhmmss`，长度不足时整体兜底）。
fn file_name(meta: &ReportMeta, device: &DeviceInfo, results: &[TestResult]) -> String {
    let ts = &meta.started;
    let (day, hms) = if ts.len() == 15 {
        (&ts[..8], &ts[9..])
    } else {
        (ts.as_str(), "unknown")
    };
    let station = sanitize_token(&meta.station);
    let tail = if device.bt_mac.is_empty() {
        format!("{hms}-unknown")
    } else {
        let mac = sanitize_token(&device.bt_mac);
        if binding_conflict(results) {
            format!("{mac}-冲突{hms}")
        } else {
            mac
        }
    };
    format!("{}-{day}-{station}-{tail}.txt", meta.model.file_prefix())
}

/// 绑定冲突（V3.13）：MAC 绑定行判 FAIL（同址不同对），报告文件名加后缀。
fn binding_conflict(results: &[TestResult]) -> bool {
    results
        .iter()
        .any(|r| r.item == TestItem::Binding && r.verdict == Verdict::Fail)
}

/// 文件名成分清洗：非 `[A-Za-z0-9._-]` 字符替换为 `_`
/// （防路径穿越/非法文件名字符；MAC 冒号也在替换之列）。
fn sanitize_token(s: &str) -> String {
    let s: String = s
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

/// 扫描 `dir` 下文件名为 `<prefix>-<day>-*.txt`（e02-/e02e- 均计）的报告，
/// 按"结果:"行统计。解析不到"结果:"行的旧版/中断报告计入 total 不计入
/// pass/fail。
///
/// FQC §8 工位分组：按报告内容 `工位:` 行归组（新报告文件名带工位段、
/// 旧报告无工位段，`工位:` 行两种都有——scan 本就逐文件读，零额外开销），
/// 只统计 `station` 工位；出厂报告（ship- 前缀）不计入测试统计。
pub fn scan_stats(dir: &Path, day: &str, station: &str) -> DayStats {
    let mut s = DayStats::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return s;
    };
    let day_mark = format!("-{day}-");
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        // V3.14：型号前缀族（e02w- / e02e- 及历史 e02-）都计入；ship- 不属于测试统计
        if !name.starts_with("e02") || !name.contains(&day_mark) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        if report_station_of(&text).as_deref() != Some(station) {
            continue;
        }
        s.total += 1;
        if let Some(line) = text.lines().find(|l| l.starts_with("结果: ")) {
            match line.trim_end() {
                "结果: PASS" => s.pass += 1,
                "结果: FAIL" => s.fail += 1,
                _ => {}
            }
        }
    }
    s
}

/// 报告内容里的工位（`工位: complete    开始: ...` 行，取首个分词）。
/// 解析不到（极老版本报告）返回 None，由调用方决定去留。
pub fn report_station_of(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.starts_with("工位: "))?;
    let v = line["工位: ".len()..].split_whitespace().next()?;
    Some(v.to_string())
}

/// 出厂报告统计（FQC §8：出厂页独立统计——今日写号 N / 成功 x / 失败 y）。
pub fn scan_ship_stats(dir: &Path, day: &str) -> DayStats {
    let mut s = DayStats::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return s;
    };
    let day_mark = format!("-{day}-");
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("ship-") || !name.contains(&day_mark) {
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

/// 报告列表（新→旧），最多 `limit` 条。V3.14：e02-/e02e-（型号前缀族）
/// 都列出；FQC §7 起含 ship- 出厂报告。
///
/// FQC §8 排序修复：此前按完整文件名字典序倒排，引入 `ship-` 后
/// `'s' > 'e'` 会让全部出货报告永久压在测试报告之前、挤爆截断上限——
/// 改为提取文件名日期段（第 2 段）统一倒排，同日以 mtime 细分；日期段
/// 解析不到的极老文件沉底（mtime 兜底参与排序）。
pub fn list_reports(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut names: Vec<(String, u64, String)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| (n.starts_with("e02") || n.starts_with("ship-")) && n.ends_with(".txt"))
                .map(|n| {
                    let date = n
                        .split('-')
                        .nth(1)
                        .filter(|d| d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()))
                        .unwrap_or("00000000")
                        .to_string();
                    // 纳秒精度：同日多份报告按真实落盘先后细分（秒级会把
                    // 连续测试的先后序打成平局，落入 read_dir 的不确定序）
                    let mtime = std::fs::metadata(dir.join(&n))
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    (date, mtime, n)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    names
        .into_iter()
        .take(limit)
        .map(|(_, _, n)| dir.join(n))
        .collect()
}

/// 出厂报告文件名（FQC §7）：`ship-<日期>-<蓝牙MAC>.txt`，同机同日重跑
/// 覆盖最新；蓝牙 MAC 未读到兜底 `ship-<日期>-<时分秒>-unknown.txt`。
pub fn ship_file_name(day: &str, bt_mac: &str, hms: &str) -> String {
    if bt_mac.trim().is_empty() {
        format!("ship-{day}-{hms}-unknown.txt")
    } else {
        format!("ship-{day}-{}.txt", sanitize_token(bt_mac))
    }
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

    #[test]
    fn sanitize_strips_device_control_garbage() {
        // 真机样例（V3.7）：WiFi 输出夹带 DC1(\x11)/EOT(\x04) 串口垃圾
        let dirty = "d\u{11}!\nSending discover...\r\nd\u{4}$";
        assert_eq!(sanitize(dirty), "d!\nSending discover...\nd$");
        // 正常报告文本（含全角标点）不受影响
        let clean = "结果: FAIL\n汇总: 11 项（60s）";
        assert_eq!(sanitize(clean), clean);
    }

    fn meta() -> ReportMeta {
        ReportMeta {
            station: "complete".into(),
            model: crate::config::ModelKind::E02w,
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
    fn filename_keys_on_bt_mac() {
        let dir = std::env::temp_dir().join(format!("e02-report-{}", std::process::id()));
        let r = result(TestItem::Wifi, Verdict::Pass);
        // 蓝牙 MAC 未解析到 → 日期+时分秒+unknown（多台未知机不互踩）
        let p1 = write_report(
            &dir,
            &meta(),
            &DeviceInfo::default(),
            std::slice::from_ref(&r),
            true,
        )
        .unwrap();
        assert!(p1.ends_with("e02w-20261022-complete-103012-unknown.txt"));
        // 蓝牙 MAC 已知 → 同机同日一份（V3.13 键改蓝牙 MAC；SN 仅留证不作键）
        let dev = DeviceInfo {
            bt_mac: "0012A3B4C5D6".into(),
            sn: "SN 123".into(),
            ..Default::default()
        };
        let p2 = write_report(&dir, &meta(), &dev, std::slice::from_ref(&r), true).unwrap();
        assert!(p2.ends_with("e02w-20261022-complete-0012A3B4C5D6.txt"));
        // 同机同日再写 → 同一路径（重测覆盖更新）
        let p3 = write_report(&dir, &meta(), &dev, &[], true).unwrap();
        assert_eq!(p2, p3);
        // 绑定冲突 → 追加 -冲突+时分秒，冲突机不覆盖原机报告
        let mut bind = TestResult::new(TestItem::Binding);
        bind.verdict = Verdict::Fail;
        let p4 = write_report(&dir, &meta(), &dev, &[r, bind], true).unwrap();
        assert!(p4.ends_with("e02w-20261022-complete-0012A3B4C5D6-冲突103012.txt"));
        // MAC 含冒号/路径字符 → 替换为 _（防目录穿越/非法文件名）
        let p5 = write_report(
            &dir,
            &meta(),
            &DeviceInfo {
                bt_mac: "00:11/../x".into(),
                ..Default::default()
            },
            &[],
            true,
        )
        .unwrap();
        assert!(p5.ends_with("e02w-20261022-complete-00_11_.._x.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_stats_and_listing_by_day() {
        let dir = std::env::temp_dir().join(format!("e02-stats-{}", std::process::id()));
        // 当日三台（两 PASS 一 FAIL，蓝牙 MAC 各异），他日一台不计数
        for (bt, verdict) in [
            ("BT1", Verdict::Pass),
            ("BT2", Verdict::Pass),
            ("BT3", Verdict::Fail),
        ] {
            let dev = DeviceInfo {
                bt_mac: bt.into(),
                ..Default::default()
            };
            let r = result(TestItem::Wifi, verdict);
            write_report(&dir, &meta(), &dev, std::slice::from_ref(&r), true);
        }
        let other_day = ReportMeta {
            started: "20261021-080000".into(),
            ..meta()
        };
        write_report(
            &dir,
            &other_day,
            &DeviceInfo {
                bt_mac: "BT9".into(),
                ..Default::default()
            },
            &[result(TestItem::Wifi, Verdict::Pass)],
            true,
        );
        // FQC §8：模组工位一台——同日跨工位各自保留、各自统计
        let module_meta = ReportMeta {
            station: "module".into(),
            ..meta()
        };
        write_report(
            &dir,
            &module_meta,
            &DeviceInfo {
                bt_mac: "BT4".into(),
                ..Default::default()
            },
            &[result(TestItem::Wifi, Verdict::Pass)],
            true,
        );
        let s = scan_stats(&dir, "20261022", "complete");
        assert_eq!(
            s,
            DayStats {
                total: 3,
                pass: 2,
                fail: 1
            }
        );
        let s = scan_stats(&dir, "20261022", "module");
        assert_eq!(s.total, 1);
        // 列表新→旧：日期段倒排 + 同日 mtime 纳秒细分（最后写的 BT4 在前），
        // 他日 BT9 沉底；最多取 2
        let list = list_reports(&dir, 2);
        assert_eq!(list.len(), 2);
        assert!(list[0].ends_with("e02w-20261022-module-BT4.txt"));
        assert!(list[1].ends_with("e02w-20261022-complete-BT3.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FQC §8 排序修复：ship- 与 e02w-/e02- 混排时按日期段统一倒排，
    /// 不再被 's' > 'e' 字典序压顶。
    #[test]
    fn ship_reports_sort_by_date_not_lexical() {
        let dir = std::env::temp_dir().join(format!("e02-ship-sort-{}", std::process::id()));
        let r = result(TestItem::Wifi, Verdict::Pass);
        // 昨天的 ship 报告 + 今天的测试报告：今天的在前
        let p_ship = dir.join(ship_file_name_for_test("20261021", "SHIPMAC"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&p_ship, "E02W 出厂写号报告\n结果: PASS\n工位: ship\n").unwrap();
        write_report(
            &dir,
            &meta(),
            &DeviceInfo {
                bt_mac: "BT1".into(),
                ..Default::default()
            },
            std::slice::from_ref(&r),
            true,
        )
        .unwrap();
        let list = list_reports(&dir, 10);
        assert_eq!(list.len(), 2);
        assert!(
            list[0].ends_with("e02w-20261022-complete-BT1.txt"),
            "今日测试报告应在前: {list:?}"
        );
        assert!(list[1].ends_with("ship-20261021-SHIPMAC.txt"));
        // 今日统计不含 ship
        let s = scan_stats(&dir, "20261021", "complete");
        assert_eq!(s.total, 0, "ship 报告不计入测试统计");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 测试桩：直接以固定 MAC 落一个 ship 文件名（复用 ship_file_name 逻辑）。
    fn ship_file_name_for_test(day: &str, mac: &str) -> String {
        ship_file_name(day, mac, "080000")
    }

    #[test]
    fn ship_file_name_shapes() {
        assert_eq!(
            ship_file_name("20261124", "0012A3B4C5D6", "103012"),
            "ship-20261124-0012A3B4C5D6.txt"
        );
        assert_eq!(
            ship_file_name("20261124", "", "103012"),
            "ship-20261124-103012-unknown.txt"
        );
        assert_eq!(
            ship_file_name("20261124", "00:11", "103012"),
            "ship-20261124-00_11.txt"
        );
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
