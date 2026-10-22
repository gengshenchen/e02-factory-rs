//! 底部日志面板（V3.2 深底终端风）：标题行兼作状态条，
//! 正文等宽字体自动滚到最新。

use crate::app::E02App;
use crate::ui::theme;
use egui::Color32;

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    // 深底上的次级按钮形态：抬升面底色 + 奶油白字（button-secondary-on-dark）
    let w = &mut ui.visuals_mut().widgets;
    w.inactive.bg_fill = theme::DARK_ELEVATED;
    w.inactive.fg_stroke = theme::hairline(theme::ON_DARK);
    w.inactive.bg_stroke = egui::Stroke::NONE;
    w.hovered.bg_fill = Color32::from_white_alpha(14);
    w.hovered.fg_stroke = theme::hairline(Color32::WHITE);
    w.hovered.bg_stroke = egui::Stroke::NONE;

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("过程日志")
                .strong()
                .color(theme::ON_DARK),
        );
        if app.busy {
            ui.spinner();
            ui.label(
                egui::RichText::new(format!(
                    "测试中：{}",
                    app.current.map(|i| i.label()).unwrap_or_default()
                ))
                .color(theme::TEAL),
            );
        } else {
            ui.label(egui::RichText::new("就绪").color(theme::ON_DARK_SOFT));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("清空").clicked() {
                app.log.clear();
            }
            if ui.small_button("复制日志").clicked() {
                let text = app.log.iter().cloned().collect::<Vec<_>>().join("\n");
                ui.ctx().copy_text(text);
                app.push_log("日志已复制到剪贴板");
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
                            .color(theme::ON_DARK),
                    );
                }
            },
        );
}
