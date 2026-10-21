//! CLI 入口（M1）：无界面对真机跑自动判定测试。
//!
//! ```text
//! e02-factory-rs                  # 无参数 → 图形界面
//! e02-factory-rs list             # 列出测试项
//! e02-factory-rs devices          # 列出 adb 在线设备
//! e02-factory-rs test info wifi   # 跑指定测试项（空格分隔多个）
//! e02-factory-rs all              # 跑全部（人工/媒体项自动跳过）
//! ```
//! 可选 `--report <文件>`：附带 T32.txt 风格文本报告。
//! 退出码：有 FAIL 为 1，参数错误为 2，其余 0。

use crate::adb;
use crate::engine::{runner, TestItem, Verdict};
use crate::report;

pub fn run(args: &[String]) -> i32 {
    let mut report_path: Option<String> = None;
    let mut rest: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--report" {
            match it.next() {
                Some(p) => report_path = Some(p.clone()),
                None => {
                    eprintln!("--report 缺少文件参数");
                    return 2;
                }
            }
        } else {
            rest.push(a.as_str());
        }
    }

    match rest.first().copied() {
        Some("help") | Some("-h") | Some("--help") => {
            print_usage();
            0
        }
        Some("list") => {
            list_items();
            0
        }
        Some("devices") => devices_cmd(),
        Some("test") if rest.len() >= 2 => test_cmd(&rest[1..], report_path),
        Some("all") => test_cmd(&[], report_path),
        _ => {
            print_usage();
            2
        }
    }
}

fn print_usage() {
    println!(
        "用法: e02-factory-rs [命令]\n\
         \x20 无参数            图形界面\n\
         \x20 list              列出测试项\n\
         \x20 devices           列出 adb 在线设备\n\
         \x20 test <项...>      跑指定测试项\n\
         \x20 all               跑全部（人工/媒体项跳过）\n\
         \x20 可选 --report <文件>"
    );
}

fn list_items() {
    for item in TestItem::ALL {
        println!("{:<8} {}", item.slug(), item.label());
    }
}

fn devices_cmd() -> i32 {
    match adb::devices() {
        Ok(devs) => {
            if devs.is_empty() {
                println!("无在线设备");
            }
            for d in devs {
                println!("{d}");
            }
            0
        }
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn test_cmd(slugs: &[&str], report_path: Option<String>) -> i32 {
    let items = match resolve_items(slugs) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    match adb::devices() {
        Ok(devs) if !devs.is_empty() => println!("在线设备: {}", devs.join(", ")),
        Ok(_) => eprintln!("警告：无 adb 在线设备"),
        Err(e) => eprintln!("警告：{e:#}"),
    }

    let batch = runner::run_batch(&items);
    let (mut pass, mut fail, mut skip) = (0usize, 0usize, 0usize);
    for r in &batch.results {
        match r.verdict {
            Verdict::Pass | Verdict::ManualYes => pass += 1,
            Verdict::Fail | Verdict::ManualNo => fail += 1,
            Verdict::Pending | Verdict::Skipped => skip += 1,
        }
        println!("[{}] {} {}", r.verdict.label(), r.item.label(), r.detail);
    }
    println!("\n通过 {pass} / 失败 {fail} / 跳过 {skip}");

    if let Some(p) = report_path {
        match std::fs::write(&p, report::render_text(&batch.device, &batch.results)) {
            Ok(()) => println!("报告已写入 {p}"),
            Err(e) => eprintln!("写报告失败: {e}"),
        }
    }
    i32::from(fail > 0)
}

/// 空列表 → 全部测试项；未知 slug 报错。
fn resolve_items(slugs: &[&str]) -> Result<Vec<TestItem>, String> {
    if slugs.is_empty() {
        return Ok(TestItem::ALL.to_vec());
    }
    let mut unknown = Vec::new();
    let items = slugs
        .iter()
        .filter_map(|s| match TestItem::from_slug(s) {
            Some(item) => Some(item),
            None => {
                unknown.push(*s);
                None
            }
        })
        .collect();
    if unknown.is_empty() {
        Ok(items)
    } else {
        Err(format!(
            "未知测试项: {}（可用项见 `e02-factory-rs list`）",
            unknown.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_empty_means_all() {
        assert_eq!(resolve_items(&[]).unwrap(), TestItem::ALL.to_vec());
    }

    #[test]
    fn resolve_reports_unknown_slugs() {
        assert!(resolve_items(&["info", "nope"])
            .unwrap_err()
            .contains("nope"));
        assert_eq!(resolve_items(&["wifi", "info"]).unwrap().len(), 2);
    }
}
