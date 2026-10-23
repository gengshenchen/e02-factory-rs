//! 左侧测试面板（V3.4）：开始测试主按钮 + 自动/人工分组清单。
//!
//! 行即按钮：整行可点，圆点表状态，进行中行奶油底+珊瑚强调，未启用项置灰。
//! 矮窗口下清单区可滚动（按钮与标题钉在滚动区外），底部显示启用计数。
//!
//! 坑（V3.3.1 教训）：水平布局 + 垂直 Center 对齐会把子项撑满剩余高度；
//! 侧面板/ScrollArea 内容高度无界，行内一律 Align::Min。

use crate::app::E02App;
use crate::engine::{TestItem, Verdict};
use crate::ui::theme;
use egui::{Color32, Rounding, Sense, Stroke};

/// 判定语义色（绿合格 / 红不合格 / 灰待测；绿取深档保小字对比度）。
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

    let run = theme::primary("开始测试").min_size(egui::vec2(ui.available_width(), 44.0));
    // 在位门槛（V3.8）：未连接/多台都禁用（start_run 内同款兜底）
    let online = app.device_online();
    let disabled_hint = if app.busy {
        "测试进行中，可点右上角停止测试"
    } else if !online {
        "设备未连接：请插入 USB 线"
    } else {
        "按工位配置执行全部启用项（自动项先行）"
    };
    if ui
        .add_enabled(!app.busy && online, run)
        .on_hover_text("按工位配置执行全部启用项（自动项先行）")
        .on_disabled_hover_text(disabled_hint)
        .clicked()
    {
        app.start_run(app.batch_items());
    }
    if !online && !app.busy {
        ui.label(
            egui::RichText::new("设备未连接，插入 USB 后开始")
                .small()
                .color(theme::MUTED),
        );
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

    // 清单区滚动：高度封顶到面板可视底（面板内容高度无界，需显式限制，
    // 否则 ScrollArea 长到内容高度，超出部分照旧被裁）。底部留启用计数行。
    let max_h = (ui.clip_rect().bottom() - ui.cursor().min.y - 30.0).max(100.0);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(max_h)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0; // 行距压紧：12 项一屏放下（1280×837）
            section(ui, "自动测试");
            for &(item, verdict, enabled, running) in &autos {
                row(ui, app, item, verdict, enabled, running);
            }
            section(ui, "人工测试");
            for &(item, verdict, enabled, running) in &manuals {
                row(ui, app, item, verdict, enabled, running);
            }
        });

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(format!(
            "共 {} 项，已启用 {}",
            TestItem::ALL.len(),
            active.len()
        ))
        .small()
        .color(theme::MUTED),
    );
}

fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(text)
            .small()
            .strong()
            .color(theme::MUTED),
    );
    ui.add_space(3.0);
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
        ("未启用", theme::MUTED)
    } else if running {
        ("进行中", theme::PRIMARY_ACTIVE)
    } else {
        (verdict.ui_label(), verdict_color(verdict))
    };
    let resp = egui::Frame::default()
        .fill(if running {
            theme::SELECTED
        } else {
            Color32::TRANSPARENT
        })
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
        .stroke(Stroke::NONE)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::dot(ui, dot_color);
                ui.add_space(7.0);
                let name = egui::RichText::new(item.label());
                let name = if strong { name.strong() } else { name };
                ui.label(name.color(label_color));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
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
