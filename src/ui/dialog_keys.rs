//! 按键引导弹窗（V2.1，V3.2 视觉强化）：提示按键、回显进度、未收齐时重测/改判。
//! 弹窗期间 worker 阻塞等待应答（对应指示灯/MIC 的模态语义）。

use crate::app::E02App;
use crate::protocol::key_name;
use crate::ui::theme;
use std::time::Instant;

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    let Some(prompt) = &app.keys_prompt else {
        return;
    };
    let detected = prompt.detected.clone();
    let awaiting = prompt.awaiting;
    let deadline = prompt.deadline;
    let required = app.config.keys.required.clone();
    egui::Window::new("按键测试")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.label(
                egui::RichText::new("请依次按下以下按键：")
                    .size(16.0)
                    .color(theme::INK),
            );
            ui.add_space(8.0);
            for &code in required.iter() {
                let ok = detected.contains(&code);
                ui.horizontal(|ui| {
                    theme::dot(ui, if ok { theme::OK } else { theme::LINE });
                    ui.add_space(7.0);
                    let name = egui::RichText::new(key_name(code));
                    let name = if ok { name.strong() } else { name };
                    ui.label(name.color(if ok { theme::INK } else { theme::INK_2 }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(if ok { "已按下" } else { "待按下" })
                                .small()
                                .strong()
                                .color(if ok { theme::OK } else { theme::INK_3 }),
                        );
                    });
                });
            }
            ui.add_space(8.0);
            if awaiting {
                if let Some(dl) = deadline {
                    let left = dl.saturating_duration_since(Instant::now()).as_secs();
                    let countdown =
                        egui::RichText::new(format!("{left} 秒未应答将自动判定为失败")).small();
                    let countdown = if left <= 10 {
                        countdown.strong()
                    } else {
                        countdown
                    };
                    ui.label(countdown.color(if left <= 10 { theme::BAD } else { theme::INK_3 }));
                }
                ui.label(
                    egui::RichText::new("未收齐按键，请选择：")
                        .small()
                        .color(theme::BAD),
                );
                ui.add_space(6.0);
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
                    if ui.add(theme::danger("判定失败")).clicked() {
                        if let Some(p) = app.keys_prompt.take() {
                            p.force(false);
                        }
                    }
                });
            } else {
                ui.label(egui::RichText::new("检测中...").small().color(theme::INK_3));
            }
        });
}
