//! 人工判定模态窗（原 MessageBox YES/NO 的跨平台等价物）。
//!
//! worker 线程经 channel 发出 [`UiEvent::Manual`] 并阻塞等待，
//! 本模块在 UI 线程弹窗收集"合格/不合格"后经 [`ManualAsk::answer`]
//! 写回应答 channel，worker 继续执行后续测试项。

use crate::app::E02App;

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    let Some(ask) = &app.manual else {
        return;
    };
    let title = ask.title.clone();
    let question = ask.question.clone();
    egui::Window::new(&title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.add_space(6.0);
            ui.label(&question);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("合格").clicked() {
                    if let Some(ask) = app.manual.take() {
                        ask.answer(true);
                    }
                }
                if ui.button("不合格").clicked() {
                    if let Some(ask) = app.manual.take() {
                        ask.answer(false);
                    }
                }
            });
        });
}
