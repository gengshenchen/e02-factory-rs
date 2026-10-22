//! 左侧测试面板（V3.2 重排）：主按钮一键全测 + 自动/人工分组。
//!
//! 行即按钮：整行可点，圆点表状态，进行中行奶油底+珊瑚强调，未启用项置灰。

use crate::app::E02App;
use crate::engine::{TestItem, Verdict};
use crate::ui::theme;
use egui::{Color32, Rounding, Sense, Stroke};

/// 判定语义色（绿通过 / 红失败 / 灰未测；绿取深档保小字对比度）。
pub fn verdict_color(verdict: Verdict) -> Color32 {
    match verdict {
        Verdict::Pending | Verdict::Skipped => theme::MUTED,
        Verdict::Pass | Verdict::ManualYes => theme::OK_STRONG,
        Verdict::Fail | Verdict::ManualNo => theme::BAD,
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    ui.set_min_width(ui.available_width()); // 撑满面板宽度（Frame 随内容收缩）
    ui.label(
        egui::RichText::new("测试项")
            .strong()
            .size(16.0)
            .color(theme::INK),
    );
    ui.add_space(8.0);

    let run = theme::primary("一键全测").min_size(egui::vec2(ui.available_width(), 44.0));
    if ui
        .add_enabled(!app.busy, run)
        .on_hover_text("按工位配置执行全部启用项（自动项先行）")
        .on_disabled_hover_text("测试进行中，可点右上角停止")
        .clicked()
    {
        app.start_run(app.batch_items());
    }

    let active = app.config.active_items();
    // 状态快照，避免行渲染借用与点击修改 app 冲突
    let rows: Vec<(TestItem, Verdict, bool, bool)> = app
        .results
        .iter()
        .map(|r| {
            (
                r.item,
                r.verdict,
                active.contains(&r.item),
                app.current == Some(r.item),
            )
        })
        .collect();
    let (autos, manuals): (Vec<_>, Vec<_>) = rows.iter().partition(|(i, ..)| !i.is_manual());

    section(ui, "自动测试");
    for &(item, verdict, enabled, running) in &autos {
        row(ui, app, item, verdict, enabled, running);
    }
    section(ui, "人工测试");
    for &(item, verdict, enabled, running) in &manuals {
        row(ui, app, item, verdict, enabled, running);
    }
}

fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(12.0);
    ui.label(
        egui::RichText::new(text)
            .small()
            .strong()
            .color(theme::MUTED),
    );
    ui.add_space(4.0);
}

/// 单项一行：圆点 + 名称 + 右侧状态字；整行点击触发单项重测。
fn row(
    ui: &mut egui::Ui,
    app: &mut E02App,
    item: TestItem,
    verdict: Verdict,
    enabled: bool,
    running: bool,
) {
    let (dot_color, label_color, strong) = if !enabled {
        (theme::HAIRLINE, theme::MUTED, false)
    } else if running {
        // 进行中：奶油底 + 珊瑚圆点，文字保持墨色（强调靠底色不靠字色）
        (theme::PRIMARY, theme::INK, true)
    } else {
        (verdict_color(verdict), theme::INK, false)
    };
    let (status_text, status_color) = if !enabled {
        ("未启用".to_string(), theme::MUTED)
    } else if running {
        ("进行中".to_string(), theme::PRIMARY_ACTIVE)
    } else {
        (verdict.label().to_string(), verdict_color(verdict))
    };
    let resp = egui::Frame::default()
        .fill(if running {
            theme::SELECTED
        } else {
            Color32::TRANSPARENT
        })
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(8.0, 7.0))
        .stroke(Stroke::NONE)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                theme::dot(ui, dot_color);
                ui.add_space(7.0);
                let name = egui::RichText::new(item.label());
                let name = if strong { name.strong() } else { name };
                ui.label(name.color(label_color));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(status_text)
                            .small()
                            .strong()
                            .color(status_color),
                    );
                });
            });
        })
        .response
        .interact(Sense::click());
    if !enabled {
        resp.on_hover_text("未在当前工位启用，可在设置中开启");
        return;
    }
    if app.busy {
        return;
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(resp.rect, Rounding::same(8.0), Color32::from_black_alpha(5));
    }
    if resp.clicked() {
        app.start_run(vec![item]);
    }
}
