//! 底部日志面板（V3.2 深底终端风）：标题行兼作状态条，
//! 正文等宽字体自动滚到最新。

use crate::app::E02App;
use crate::ui::theme;
use egui::Color32;

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    // 深底下的控件配色（幽灵按钮）
    let w = &mut ui.visuals_mut().widgets;
    w.inactive.bg_fill = Color32::TRANSPARENT;
    w.inactive.fg_stroke = egui::Stroke::new(1.0, theme::DARK_INK);
    w.hovered.bg_fill = Color32::from_white_alpha(12);
    w.hovered.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("过程日志")
                .strong()
                .color(theme::DARK_INK),
        );
        if app.busy {
            ui.spinner();
            ui.label(
                egui::RichText::new(format!(
                    "正在测试：{}",
                    app.current.map(|i| i.label()).unwrap_or_default()
                ))
                .color(theme::ACCENT_ON_DARK),
            );
        } else {
            ui.label(egui::RichText::new("就绪").color(theme::DARK_INK_2));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("清空").clicked() {
                app.log.clear();
            }
        });
    });
    ui.add_space(6.0);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show_rows(
            ui,
            ui.text_style_height(&egui::TextStyle::Monospace),
            app.log.len(),
            |ui, range| {
                for line in app.log.range(range) {
                    ui.label(
                        egui::RichText::new(line.as_str())
                            .monospace()
                            .color(theme::DARK_INK),
                    );
                }
            },
        );
}
