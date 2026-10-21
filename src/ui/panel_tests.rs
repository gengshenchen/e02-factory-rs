//! 左侧测试面板：测试项列表 + 一键全测（M2 接入引擎后启用单项触发）。

use crate::app::E02App;
use crate::engine::Verdict;
use egui::Color32;

/// 判定结果颜色（绿 PASS / 红 FAIL，对应原程序 OnCtlColor 换色）。
pub fn verdict_color(verdict: Verdict) -> Color32 {
    match verdict {
        Verdict::Pending => Color32::GRAY,
        Verdict::Pass | Verdict::ManualYes => Color32::GREEN,
        Verdict::Fail | Verdict::ManualNo => Color32::RED,
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    ui.heading("测试项");
    ui.add_space(6.0);

    // M2 接入测试引擎后启用
    let _ = ui.add_enabled(false, egui::Button::new("一键全测"));
    ui.separator();

    for result in &mut app.results {
        ui.horizontal(|ui| {
            let color = verdict_color(result.verdict);
            ui.colored_label(color, "●");
            ui.label(result.item.label());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.colored_label(color, result.verdict.label());
            });
        });
        if !result.detail.is_empty() {
            ui.small(&result.detail);
        }
    }

    ui.separator();
    // 骨架自验入口：验证人工判定模态窗（M5 改由引擎经 channel 触发）
    if ui.button("演示：人工判定弹窗").clicked() {
        app.manual = Some(crate::ui::dialog_manual::ManualAsk {
            title: "指示灯测试".into(),
            question: "指示灯闪烁是否正常？".into(),
        });
    }
}
