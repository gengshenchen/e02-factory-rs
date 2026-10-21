//! 报告中心（V3）：历史报告浏览——左侧列表（新→旧）+ 右侧内容，
//! 顶部今日统计与"打开报告文件夹"。

use crate::app::E02App;

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    if !app.report_open {
        return;
    }
    let mut open = true;
    let mut open_failed = false;
    egui::Window::new("报告中心")
        .open(&mut open)
        .collapsible(false)
        .default_size([820.0, 540.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(t) = &app.today {
                    ui.strong(format!(
                        "今日 {} 台: PASS {} / FAIL {}",
                        t.total, t.pass, t.fail
                    ));
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
            ui.separator();
            egui::SidePanel::left("report_list")
                .resizable(true)
                .default_width(230.0)
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if app.report_list.is_empty() {
                            ui.label("报告目录暂无报告");
                            ui.small(app.reports_dir.display().to_string());
                            return;
                        }
                        let sel = app.report_sel.clone();
                        let list = app.report_list.clone();
                        for p in list {
                            // 文件名 e02-<时间>-<SN>.txt → 显示去前后缀
                            let stem = p
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let name = stem
                                .strip_prefix("e02-")
                                .map(str::to_string)
                                .unwrap_or(stem);
                            let selected = sel.as_ref() == Some(&p);
                            if ui.selectable_label(selected, name).clicked() {
                                app.report_sel = Some(p);
                            }
                        }
                    });
                });
            egui::CentralPanel::default().show_inside(ui, |ui| match app.report_content() {
                Some(text) => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| ui.monospace(text));
                }
                None => {
                    ui.label("选择左侧报告查看内容");
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
