//! 按键引导弹窗（V2.1）：提示操作员按键、回显检测进度、未收齐时重测/改判。
//! 弹窗期间 worker 阻塞等待应答（对应指示灯/MIC 的模态语义）。

use crate::app::E02App;
use crate::protocol::key_name;

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    let Some(prompt) = &app.keys_prompt else {
        return;
    };
    let detected = prompt.detected.clone();
    let awaiting = prompt.awaiting;
    let required = app.config.keys.required.clone();
    egui::Window::new("按键测试")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.label("请依次按下以下按键：");
            ui.add_space(6.0);
            for &code in required.iter() {
                let ok = detected.contains(&code);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        if ok {
                            egui::Color32::GREEN
                        } else {
                            egui::Color32::GRAY
                        },
                        if ok { "已按下" } else { "待按下" },
                    );
                    ui.label(key_name(code));
                });
            }
            ui.add_space(6.0);
            if awaiting {
                ui.colored_label(egui::Color32::RED, "未收齐，请选择：");
                ui.horizontal(|ui| {
                    if ui.button("重新测试").clicked() {
                        if let Some(p) = app.keys_prompt.take() {
                            p.retry();
                        }
                    }
                    if ui.button("强制通过").clicked() {
                        if let Some(p) = app.keys_prompt.take() {
                            p.force(true);
                        }
                    }
                    if ui.button("判定失败").clicked() {
                        if let Some(p) = app.keys_prompt.take() {
                            p.force(false);
                        }
                    }
                });
            } else {
                ui.label("检测中...");
            }
        });
}
