//! 右侧面板（V3.4）：被测设备信息 + 整机结论 + 当日统计看板。
//!
//! 结论卡是操作员一眼要看的结果（合格绿/不合格红/进行中奶油），
//! 当日统计从顶栏挪来（顶栏只留标题/工位/进度/动作），填平右侧留白。

use crate::app::E02App;
use crate::engine::Verdict;
use crate::ui::theme;
use egui::Color32;

pub fn show(ui: &mut egui::Ui, app: &E02App) {
    ui.set_min_width(ui.available_width()); // 撑满面板宽度（Frame 随内容收缩）
    heading(ui, "被测设备");
    egui::Grid::new("device_info")
        .num_columns(2)
        .spacing([10.0, 8.0])
        .show(ui, |ui| {
            field(ui, "SN", &app.device.sn, true);
            field(ui, "WiFi MAC", &app.device.wifi_mac, false);
            field(ui, "蓝牙 MAC", &app.device.bt_mac, false);
            field(ui, "软件版本", &app.device.t32_ver, false);
            field(ui, "硬件版本", &app.device.hardware_ver, false);
            field(ui, "电池", &app.device.battery, false);
        });

    ui.add_space(14.0);
    heading(ui, "整机结论");
    result_card(ui, app);

    ui.add_space(14.0);
    heading(ui, "当日统计");
    if let Some(t) = &app.today {
        stats_card(ui, t);
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .strong()
            .size(16.0)
            .color(theme::INK),
    );
    ui.add_space(8.0);
}

/// 整机结论大状态卡：整宽色块 + 大字（强调靠底色，全系统唯一大红/大绿块）。
fn result_card(ui: &mut egui::Ui, app: &E02App) {
    let done: Vec<_> = app
        .results
        .iter()
        .filter(|r| !matches!(r.verdict, Verdict::Pending | Verdict::Skipped))
        .collect();
    let (text, fg, bg) = if app.busy {
        let p = app
            .progress
            .as_ref()
            .map(|p| format!("测试中 {}/{}", p.done, p.total))
            .unwrap_or_else(|| "测试中".to_string());
        (p, theme::INK, theme::SELECTED)
    } else if done.is_empty() {
        ("待测试".to_string(), theme::MUTED, theme::SOFT)
    } else {
        let fail = done.iter().filter(|r| !r.verdict.is_pass()).count();
        if fail == 0 {
            (
                format!("全部合格（{} 项）", done.len()),
                theme::OK_STRONG,
                theme::OK_BG,
            )
        } else {
            (format!("{fail} 项不合格"), theme::BAD, theme::BAD_BG)
        }
    };
    egui::Frame::default()
        .fill(bg)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(10.0, 9.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // 垂直布局 + 水平居中：宽度撑满卡面（宽有界，安全）
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(text).strong().size(15.0).color(fg));
            });
        });
}

/// 当日统计 2×2 看板卡（内容卡 CARD 底）：已测/合格率/合格/不合格。
fn stats_card(ui: &mut egui::Ui, t: &crate::report::DayStats) {
    let pass_color = if t.pass > 0 {
        theme::OK_STRONG
    } else {
        theme::MUTED
    };
    let fail_color = if t.fail > 0 { theme::BAD } else { theme::MUTED };
    let (rate, rate_color) = match (t.pass * 100).checked_div(t.total) {
        Some(pct) if t.fail == 0 => (format!("{pct}%"), theme::OK_STRONG),
        Some(pct) => (format!("{pct}%"), theme::WARN),
        None => ("-".to_string(), theme::MUTED),
    };
    egui::Frame::default()
        .fill(theme::CARD)
        .rounding(egui::Rounding::same(12.0))
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::Grid::new("today_stats")
                .num_columns(2)
                .spacing([8.0, 12.0])
                .show(ui, |ui| {
                    stat_cell(ui, &format!("{} 台", t.total), "已测", theme::INK);
                    stat_cell(ui, &rate, "合格率", rate_color);
                    stat_cell(ui, &t.pass.to_string(), "合格", pass_color);
                    stat_cell(ui, &t.fail.to_string(), "不合格", fail_color);
                });
        });
}

fn stat_cell(ui: &mut egui::Ui, value: &str, label: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(value).strong().size(17.0).color(color));
        ui.label(egui::RichText::new(label).small().color(theme::MUTED));
    });
    ui.end_row();
}

/// 一行字段：左侧灰色小标签，右侧等宽值（原程序只读 Edit 的等价物，
/// 升级为点击复制；SN 加粗——对机核对的第一眼字段）。
fn field(ui: &mut egui::Ui, name: &str, value: &str, strong: bool) {
    ui.label(egui::RichText::new(name).small().color(theme::MUTED));
    let empty = value.is_empty();
    let text = if empty { "-" } else { value };
    let mut rich =
        egui::RichText::new(text)
            .monospace()
            .color(if empty { theme::MUTED } else { theme::INK });
    if strong && !empty {
        rich = rich.strong();
    }
    let resp = ui.add(egui::Label::new(rich).sense(egui::Sense::click()));
    let resp = if empty {
        resp
    } else {
        resp.on_hover_text("点击复制")
    };
    if resp.clicked() && !empty {
        ui.output_mut(|w| w.copied_text = value.to_string());
    }
    ui.end_row();
}
