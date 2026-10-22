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

/// 当日统计 2×2 看板：四块等宽内容卡（CARD 底 + 8px 圆角，色块表抬升）。
/// 不用 Grid：格子宽度随内容收缩，会把两列挤到左侧、卡右大片留白；
/// 显式按半宽分配瓦片，值/标签横向居中。
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
    // 两列等宽：预取半宽再下发给瓦片（留 2px 余量防 horizontal 溢出换行）
    let w = (ui.available_width() - 10.0) / 2.0;
    ui.horizontal(|ui| {
        stat_tile(ui, w, &format!("{} 台", t.total), "已测", theme::INK);
        stat_tile(ui, w, &rate, "合格率", rate_color);
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        stat_tile(ui, w, &t.pass.to_string(), "合格", pass_color);
        stat_tile(ui, w, &t.fail.to_string(), "不合格", fail_color);
    });
}

/// 等宽统计瓦片：set_width 钉死内容宽（min+max），Center 居中才有界可依。
fn stat_tile(ui: &mut egui::Ui, width: f32, value: &str, label: &str, color: Color32) {
    egui::Frame::default()
        .fill(theme::CARD)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(10.0, 9.0))
        .show(ui, |ui| {
            ui.set_width(width - 20.0); // 减左右内边距，瓦片总宽=width
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(value).strong().size(17.0).color(color));
                ui.add_space(2.0);
                ui.label(egui::RichText::new(label).small().color(theme::MUTED));
            });
        });
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
