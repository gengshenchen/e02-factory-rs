//! 报告中心（V3，V3.2 视觉强化）：历史报告浏览——左侧列表 + 右侧内容，
//! 顶部今日看板与"打开报告文件夹"。

use crate::app::E02App;
use crate::ui::theme;
use egui::{Color32, Rounding, Sense};

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    if !app.report_open {
        return;
    }
    let mut open = true;
    let mut open_failed = false;
    // 居中初始位（V3.6）：锚定窗不可拖动
    let center = ctx.screen_rect().center();
    egui::Window::new("报告中心")
        .open(&mut open)
        .collapsible(false)
        .default_size([860.0, 560.0])
        .default_pos(center - egui::vec2(430.0, 280.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(t) = &app.today {
                    theme::today_badges(ui, t);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("刷新").clicked() {
                        app.refresh_stats();
                        app.refresh_reports();
                    }
                    if ui.button("打开报告文件夹").clicked() && !open_folder(&app.reports_dir)
                    {
                        open_failed = true;
                    }
                });
            });
            ui.add_space(6.0);
            egui::SidePanel::left("report_list")
                .resizable(true)
                .default_width(240.0)
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if app.report_list.is_empty() {
                            ui.label(egui::RichText::new("报告目录暂无报告").color(theme::MUTED));
                            ui.small(
                                egui::RichText::new(app.reports_dir.display().to_string())
                                    .color(theme::MUTED),
                            );
                            return;
                        }
                        let sel = app.report_sel.clone();
                        let list = app.report_list.clone();
                        for p in list {
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

/// 列表显示名：`e02-<yyyymmdd>-<SN|hhmmss-unknown>` → `10-22 SN_123` / `10-22 10:30:12（无SN）`。
fn display_name(stem: &str) -> String {
    let Some(rest) = stem.strip_prefix("e02-") else {
        return stem.to_string();
    };
    let (day, tail) = rest.split_at(rest.len().min(8));
    if day.len() != 8 || !day.bytes().all(|b| b.is_ascii_digit()) {
        return rest.to_string();
    }
    let date = format!("{}-{}", &day[4..6], &day[6..8]);
    if let Some(t) = tail.strip_suffix("-unknown") {
        if t.len() == 6 && t.bytes().all(|b| b.is_ascii_digit()) {
            return format!("{date} {}:{}:{}（无SN）", &t[..2], &t[2..4], &t[4..6]);
        }
    }
    format!("{date} {tail}")
}

/// 用系统文件管理器打开报告目录（Windows explorer / Linux xdg-open）。
/// 返回是否成功启动进程（不等待结果，explorer 成功时退出码也非 0）。
fn open_folder(dir: &std::path::Path) -> bool {
    if cfg!(windows) {
        std::process::Command::new("explorer")
            .arg(dir)
            .spawn()
            .is_ok()
    } else {
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .is_ok()
    }
}
