//! 左侧测试面板：单项按钮 + 一键全测（对应原对话框按钮区）。

use crate::app::E02App;
use crate::engine::Verdict;
use egui::Color32;

/// 判定结果颜色（绿 PASS / 红 FAIL，对应原程序 OnCtlColor 换色）。
pub fn verdict_color(verdict: Verdict) -> Color32 {
    match verdict {
        Verdict::Pending => Color32::GRAY,
        Verdict::Skipped => Color32::DARK_GRAY,
        Verdict::Pass | Verdict::ManualYes => Color32::GREEN,
        Verdict::Fail | Verdict::ManualNo => Color32::RED,
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    ui.heading("测试项");
    ui.add_space(6.0);

    if ui
        .add_enabled(!app.busy, egui::Button::new("一键全测"))
        .on_hover_text("按工位配置执行全部启用项（顺序同原程序）")
        .clicked()
    {
        app.start_run(app.batch_items());
    }
    ui.separator();

    let active = app.config.active_items();
    for i in 0..app.results.len() {
        let (item, verdict, detail) = {
            let r = &app.results[i];
            (r.item, r.verdict, r.detail.clone())
        };
        let in_profile = active.contains(&item);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !app.busy && in_profile,
                    egui::Button::new(format!("▶ {}", item.label())),
                )
                .clicked()
            {
                app.start_run(vec![item]);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if in_profile {
                    ui.colored_label(verdict_color(verdict), verdict.label());
                } else {
                    ui.colored_label(Color32::DARK_GRAY, "未启用");
                }
            });
        });
        if !detail.is_empty() && in_profile {
            ui.small(&detail);
        }
    }
}
