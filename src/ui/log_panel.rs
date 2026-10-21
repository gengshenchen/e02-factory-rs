//! 底部日志面板：worker 逐步回传，等宽字体自动滚到最新（V2.1）。

use crate::app::E02App;

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    ui.horizontal(|ui| {
        ui.strong("日志");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("清空").clicked() {
                app.log.clear();
            }
        });
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show_rows(
            ui,
            ui.text_style_height(&egui::TextStyle::Monospace),
            app.log.len(),
            |ui, range| {
                for line in app.log.range(range) {
                    ui.monospace(line);
                }
            },
        );
}
