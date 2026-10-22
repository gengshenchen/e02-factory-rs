//! 人工判定模态窗（原 MessageBox YES/NO 的跨平台等价物，V3.2 视觉强化）。
//!
//! worker 线程经 channel 发出 [`UiEvent::Manual`] 并阻塞等待，
//! 本模块在 UI 线程弹窗收集"合格/不合格"后经 [`ManualAsk::answer`]
//! 写回应答 channel，worker 继续执行后续测试项。

use crate::app::E02App;
use crate::ui::theme;
use egui::Color32;
use std::time::Instant;

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    let Some(ask) = &app.manual else {
        return;
    };
    let title = ask.title.clone();
    let question = ask.question.clone();
    let deadline = ask.deadline;
    egui::Window::new(&title)
        .collapsible(false)
        .resizable(false)
        // 右上角落位：录像/拍照人工判定时媒体区回放不被弹窗遮挡（V3.5）
        .anchor(egui::Align2::RIGHT_TOP, [-24.0, 96.0])
        .show(ctx, |ui| {
            ui.set_min_width(380.0);
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&question).size(17.0).color(theme::INK));
            if let Some(dl) = deadline {
                let left = dl.saturating_duration_since(Instant::now()).as_secs();
                ui.add_space(4.0);
                let countdown =
                    egui::RichText::new(format!("{left} 秒未应答将自动判定为不合格")).small();
                let countdown = if left <= 10 {
                    countdown.strong()
                } else {
                    countdown
                };
                ui.label(countdown.color(if left <= 10 { theme::BAD } else { theme::MUTED }));
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let ok = egui::Button::new(
                    egui::RichText::new("合格")
                        .color(Color32::WHITE)
                        .strong()
                        .size(16.0),
                )
                .fill(theme::OK_STRONG)
                .stroke(egui::Stroke::NONE)
                .min_size(egui::vec2(150.0, 44.0));
                if ui.add(ok).clicked() {
                    if let Some(ask) = app.manual.take() {
                        ask.answer(true);
                    }
                }
                let bad = egui::Button::new(
                    egui::RichText::new("不合格")
                        .color(Color32::WHITE)
                        .strong()
                        .size(16.0),
                )
                .fill(theme::BAD)
                .stroke(egui::Stroke::NONE)
                .min_size(egui::vec2(150.0, 44.0));
                if ui.add(bad).clicked() {
                    if let Some(ask) = app.manual.take() {
                        ask.answer(false);
                    }
                }
            });
        });
}
