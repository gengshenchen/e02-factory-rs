//! 人工判定模态窗（原 MessageBox YES/NO 的跨平台等价物）。
//!
//! M5 语义：worker 线程经 channel 发出 [`ManualAsk`] 并阻塞等待应答，
//! 本模块在 UI 线程弹窗收集"合格/不合格"后写回应答 channel，测试继续。

use crate::app::E02App;

/// 一次人工判定请求（M5 将附带应答 channel 与关联测试项）。
#[derive(Debug, Clone)]
pub struct ManualAsk {
    pub title: String,
    pub question: String,
}

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    let Some(ask) = app.manual.clone() else {
        return;
    };
    egui::Window::new(&ask.title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.add_space(6.0);
            ui.label(&ask.question);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let yes = ui.button("合格");
                let no = ui.button("不合格");
                // M0 骨架：仅关闭弹窗并记录；M5 把判定写回引擎
                if yes.clicked() {
                    tracing::info!("人工判定 [{}]：合格", ask.title);
                    app.manual = None;
                } else if no.clicked() {
                    tracing::info!("人工判定 [{}]：不合格", ask.title);
                    app.manual = None;
                }
            });
        });
}
