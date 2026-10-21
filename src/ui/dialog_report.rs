//! 测试报告查看窗口（V2.3）：展示最近一次生成的报告文本，可打开所在文件夹。

use crate::app::E02App;
use std::path::Path;

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    if !app.report_open {
        return;
    }
    let Some(path) = app.last_report.clone() else {
        app.report_open = false;
        return;
    };
    let mut open = true;
    let mut open_failed = false;
    egui::Window::new("测试报告")
        .open(&mut open)
        .collapsible(false)
        .default_size([520.0, 480.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(path.display().to_string());
                if ui.button("打开所在文件夹").clicked() && !open_folder(&path) {
                    open_failed = true;
                }
            });
            ui.separator();
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| format!("报告读取失败: {e}"));
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.monospace(text);
                });
        });
    app.report_open = open;
    if open_failed {
        let dir = path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        app.push_log(format!("无法调起文件管理器，请手动打开: {dir}"));
    }
}

/// 用系统文件管理器打开报告所在目录（Windows explorer / Linux xdg-open）。
/// 返回是否成功启动进程（不等待结果，explorer 成功时退出码也非 0）。
fn open_folder(path: &Path) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
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
