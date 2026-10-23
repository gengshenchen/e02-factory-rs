//! 人工判定模态窗（原 MessageBox YES/NO 的跨平台等价物，V3.2 视觉强化）。
//!
//! worker 线程经 channel 发出 [`UiEvent::Manual`] 并阻塞等待，
//! 本模块在 UI 线程弹窗收集"合格/不合格"后经 [`ManualAsk::answer`]
//! 写回应答 channel，worker 继续执行后续测试项。
//!
//! V3.11 起只负责非媒体判定项（指示灯、麦克风听感——判定对象不在
//! 媒体井，弹窗不挡内容）；录像/拍照走媒体井底判定条（panel_media）。

use crate::app::E02App;
use crate::ui::theme;
use egui::Color32;
use std::time::Instant;

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    let Some(ask) = &app.manual else {
        return;
    };
    if ask.in_media {
        return; // 录像/拍照判定在媒体井底条（panel_media），不弹窗
    }
    let title = ask.title.clone();
    let question = ask.question.clone();
    let deadline = ask.deadline;
    // 居中初始位（V3.6）：不用 anchor——锚定窗不可拖动；
    // default_pos 只在首次出现生效，操作员可随后拖到顺手位置
    let center = ctx.screen_rect().center();
    egui::Window::new(&title)
        .collapsible(false)
        .resizable(false)
        .default_pos(center - egui::vec2(210.0, 110.0))
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
