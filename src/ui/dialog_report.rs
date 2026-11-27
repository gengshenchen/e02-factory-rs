//! 报告中心（V3，V3.16+ 按工位筛选与分组统计）：
//! 顶部工位筛选选项卡（全部 / 整机 / 模组 / 出厂）+ 对应工位今日看板 +
//! 左侧历史报告列表 + 右侧报告正文预览。

use crate::app::E02App;
use crate::report::{self, DayStats};
use crate::ui::theme;
use egui::{Color32, Rounding, Sense};
use std::path::Path;

/// 报告中心筛选维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReportFilter {
    #[default]
    All,
    Complete,
    Module,
    Ship,
}

fn filter_tab(ui: &mut egui::Ui, active: bool, text: &str) -> bool {
    let (bg, fg) = if active {
        (theme::SELECTED, theme::INK)
    } else {
        (Color32::TRANSPARENT, theme::MUTED)
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(13.5).color(fg))
            .fill(bg)
            .stroke(egui::Stroke::NONE),
    )
    .clicked()
}

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    if !app.report_open {
        return;
    }
    let mut open = true;
    let mut open_failed = false;
    let center = ctx.screen_rect().center();
    egui::Window::new("报告中心")
        .open(&mut open)
        .collapsible(false)
        .default_size([880.0, 580.0])
        .default_pos(center - egui::vec2(440.0, 290.0))
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (f, label) in [
                    (ReportFilter::All, "全部"),
                    (ReportFilter::Complete, "整机"),
                    (ReportFilter::Module, "模组"),
                    (ReportFilter::Ship, "出厂写号"),
                ] {
                    if filter_tab(ui, app.report_filter == f, label) && app.report_filter != f {
                        app.report_filter = f;
                    }
                }
                ui.separator();

                let (scope_label, stats) = stats_for_filter(app);
                theme::today_badges(ui, scope_label, &stats);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.button("刷新").clicked() {
                        app.refresh_stats();
                        app.refresh_ship_stats();
                        app.refresh_reports();
                    }
                    if ui.button("打开报告文件夹").clicked() && !open_folder(&app.reports_dir)
                    {
                        open_failed = true;
                    }
                });
            });
            ui.add_space(6.0);

            let filtered: Vec<_> = app
                .report_list
                .iter()
                .filter(|p| matches_filter(p, app.report_filter))
                .cloned()
                .collect();

            // 筛选切换后若原选中不在当前结果集中，自动切到当前第一条
            if !filtered.is_empty()
                && app
                    .report_sel
                    .as_ref()
                    .is_none_or(|sel| !filtered.contains(sel))
            {
                app.report_sel = filtered.first().cloned();
            }

            egui::SidePanel::left("report_list")
                .resizable(true)
                .default_width(250.0)
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if filtered.is_empty() {
                            ui.label(egui::RichText::new("当前筛选下暂无报告").color(theme::MUTED));
                            ui.small(
                                egui::RichText::new(app.reports_dir.display().to_string())
                                    .color(theme::MUTED),
                            );
                            return;
                        }
                        let sel = app.report_sel.clone();
                        for p in filtered {
                            let stem = p
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let selected = sel.as_ref() == Some(&p);
                            let resp = egui::Frame::default()
                                .fill(if selected {
                                    theme::SELECTED
                                } else {
                                    Color32::TRANSPARENT
                                })
                                .rounding(Rounding::same(8.0))
                                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                                .show(ui, |ui| {
                                    let name = egui::RichText::new(display_name(&stem));
                                    let name = if selected { name.strong() } else { name };
                                    ui.label(name.color(if selected {
                                        theme::INK
                                    } else {
                                        theme::BODY
                                    }));
                                })
                                .response
                                .interact(Sense::click());
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                ui.painter().rect_filled(
                                    resp.rect,
                                    Rounding::same(8.0),
                                    Color32::from_black_alpha(5),
                                );
                            }
                            if resp.clicked() {
                                app.report_sel = Some(p);
                            }
                        }
                    });
                });
            egui::CentralPanel::default()
                .frame(egui::Frame::default().fill(theme::CANVAS))
                .show_inside(ui, |ui| match app.report_content() {
                    Some(text) => {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(text).monospace().color(theme::BODY));
                            });
                    }
                    None => {
                        ui.centered_and_justified(|ui| {
                            ui.label(
                                egui::RichText::new("选择左侧报告查看内容").color(theme::MUTED),
                            );
                        });
                    }
                });
        });
    app.report_open = open;
    if open_failed {
        app.push_log(format!(
            "无法调起文件管理器，请手动打开: {}",
            app.reports_dir.display()
        ));
    }
}

fn stats_for_filter(app: &E02App) -> (&'static str, DayStats) {
    let day = report::today_str();
    match app.report_filter {
        ReportFilter::Ship => (
            "今日出厂",
            app.ship
                .today
                .as_ref()
                .map(|t| DayStats {
                    total: t.total,
                    pass: t.pass,
                    fail: t.fail,
                })
                .unwrap_or_else(|| report::scan_ship_stats(&app.reports_dir, &day)),
        ),
        ReportFilter::Complete => (
            "今日整机",
            report::scan_stats(&app.reports_dir, &day, "complete"),
        ),
        ReportFilter::Module => (
            "今日模组",
            report::scan_stats(&app.reports_dir, &day, "module"),
        ),
        ReportFilter::All => {
            if app.config.is_ship_station() {
                (
                    "今日出厂",
                    app.ship
                        .today
                        .as_ref()
                        .map(|t| DayStats {
                            total: t.total,
                            pass: t.pass,
                            fail: t.fail,
                        })
                        .unwrap_or_default(),
                )
            } else {
                let label = match app.config.station.as_str() {
                    "module" => "今日模组",
                    "complete" => "今日整机",
                    _ => "今日当前工位",
                };
                (
                    label,
                    app.today
                        .as_ref()
                        .map(|t| DayStats {
                            total: t.total,
                            pass: t.pass,
                            fail: t.fail,
                        })
                        .unwrap_or_default(),
                )
            }
        }
    }
}

fn matches_filter(path: &Path, filter: ReportFilter) -> bool {
    if filter == ReportFilter::All {
        return true;
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if filter == ReportFilter::Ship {
        return name.starts_with("ship-");
    }
    if name.starts_with("ship-") {
        return false;
    }
    let want = match filter {
        ReportFilter::Complete => "complete",
        ReportFilter::Module => "module",
        _ => unreachable!(),
    };
    if name.contains(&format!("-{want}-")) {
        return true;
    }
    if name.contains("-complete-") || name.contains("-module-") {
        return false;
    }
    // 无工位段的旧版历史报告按文件内 `工位:` 行归类
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| report::report_station_of(&t))
        .as_deref()
        == Some(want)
}

/// 列表显示名（FQC §7/§8 适配）：
/// - 测试报告（新）：`e02-<yyyymmdd>-<工位>-<蓝牙MAC|hhmmss-unknown|MAC-冲突hhmmss>`
/// - 出厂报告：`ship-<yyyymmdd>-<蓝牙MAC|hhmmss-unknown>`
/// - 旧版测试报告（无工位段）：`e02-<yyyymmdd>-<SN|hhmmss-unknown>`
fn display_name(stem: &str) -> String {
    let (ship, rest) = if let Some(r) = stem.strip_prefix("ship-") {
        (true, r)
    } else if let Some(r) = stem.strip_prefix("e02e-") {
        (false, r)
    } else if let Some(r) = stem.strip_prefix("e02w-") {
        (false, r)
    } else if let Some(r) = stem.strip_prefix("e02-") {
        (false, r)
    } else {
        return stem.to_string();
    };
    let segs: Vec<&str> = rest.split('-').collect();
    let Some((&day, tail_segs)) = segs.split_first() else {
        return stem.to_string();
    };
    if day.len() != 8 || !day.bytes().all(|b| b.is_ascii_digit()) {
        return stem.to_string();
    }
    let date = format!("{}-{}", &day[4..6], &day[6..8]);
    let old_unknown_shape = tail_segs.len() == 2
        && tail_segs[1] == "unknown"
        && tail_segs[0].len() == 6
        && tail_segs[0].bytes().all(|b| b.is_ascii_digit());
    let (station_label, tail) = if ship || tail_segs.len() < 2 || old_unknown_shape {
        let label = if ship { "出厂 " } else { "" };
        (label.to_string(), tail_segs.join("-"))
    } else {
        let label = match tail_segs[0] {
            "module" => "模组 ".to_string(),
            "complete" => "整机 ".to_string(),
            s => format!("{s} "),
        };
        (label, tail_segs[1..].join("-"))
    };
    let prefix = format!("{date} {station_label}");
    if let Some(t) = tail.strip_suffix("-unknown") {
        if t.len() == 6 && t.bytes().all(|b| b.is_ascii_digit()) {
            return format!("{prefix}{}:{}:{}（无MAC）", &t[..2], &t[2..4], &t[4..6]);
        }
    }
    if let Some((mac, _ts)) = tail.split_once("-冲突") {
        return format!("{prefix}{mac}·冲突");
    }
    format!("{prefix}{tail}")
}

fn open_folder(dir: &Path) -> bool {
    crate::ui::panel_ship::open_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn display_name_new_station_and_ship_prefixes() {
        assert_eq!(
            display_name("e02w-20261124-complete-0012A3B4C5D6"),
            "11-24 整机 0012A3B4C5D6"
        );
        assert_eq!(
            display_name("e02-20261124-complete-0012A3B4C5D6"),
            "11-24 整机 0012A3B4C5D6"
        );
        assert_eq!(display_name("e02e-20261124-module-BT4"), "11-24 模组 BT4");
        assert_eq!(
            display_name("e02-20261124-complete-103012-unknown"),
            "11-24 整机 10:30:12（无MAC）"
        );
        assert_eq!(
            display_name("e02-20261124-complete-0012A3B4C5D6-冲突103012"),
            "11-24 整机 0012A3B4C5D6·冲突"
        );
        assert_eq!(
            display_name("ship-20261124-0012A3B4C5D6"),
            "11-24 出厂 0012A3B4C5D6"
        );
        assert_eq!(
            display_name("ship-20261124-103012-unknown"),
            "11-24 出厂 10:30:12（无MAC）"
        );
    }

    #[test]
    fn display_name_legacy_shapes() {
        assert_eq!(display_name("e02-20261022-SN_123"), "10-22 SN_123");
        assert_eq!(
            display_name("e02-20261022-103012-unknown"),
            "10-22 10:30:12（无MAC）"
        );
        assert_eq!(display_name("readme"), "readme");
    }
}
