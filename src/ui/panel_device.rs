//! 右侧面板：设备信息字段（点击复制）+ 整机结果徽章。

use crate::app::E02App;
use crate::engine::Verdict;
use crate::ui::theme;

pub fn show(ui: &mut egui::Ui, app: &E02App) {
    ui.set_min_width(ui.available_width()); // 撑满面板宽度（Frame 随内容收缩）
    ui.label(
        egui::RichText::new("设备信息")
            .strong()
            .size(16.0)
            .color(theme::INK),
    );
    ui.add_space(8.0);
    egui::Grid::new("device_info")
        .num_columns(2)
        .spacing([10.0, 8.0])
        .show(ui, |ui| {
            field(ui, "SN", &app.device.sn);
            field(ui, "WiFi MAC", &app.device.wifi_mac);
            field(ui, "蓝牙 MAC", &app.device.bt_mac);
            field(ui, "软件版本", &app.device.t32_ver);
            field(ui, "硬件版本", &app.device.hardware_ver);
            field(ui, "电池", &app.device.battery);
        });

    ui.add_space(14.0);
    ui.label(
        egui::RichText::new("整机结果")
            .strong()
            .size(16.0)
            .color(theme::INK),
    );
    ui.add_space(6.0);

    let done: Vec<_> = app
        .results
        .iter()
        .filter(|r| !matches!(r.verdict, Verdict::Pending | Verdict::Skipped))
        .collect();
    if done.is_empty() {
        theme::badge(ui, "等待测试", theme::INK_3, theme::BG);
    } else {
        let fail = done.iter().filter(|r| !r.verdict.is_pass()).count();
        if fail == 0 {
            theme::badge(
                ui,
                &format!("已测 {} 项，全部通过", done.len()),
                theme::OK,
                theme::OK_BG,
            );
        } else {
            theme::badge(
                ui,
                &format!("已测 {} 项，{fail} 项失败", done.len()),
                theme::BAD,
                theme::BAD_BG,
            );
        }
    }
}

/// 一行字段：左侧灰色小标签，右侧等宽值（原程序只读 Edit 的等价物，
/// 升级为点击复制）。
fn field(ui: &mut egui::Ui, name: &str, value: &str) {
    ui.label(egui::RichText::new(name).small().color(theme::INK_3));
    let empty = value.is_empty();
    let text = if empty { "-" } else { value };
    let resp = ui.add(
        egui::Label::new(egui::RichText::new(text).monospace().color(if empty {
            theme::INK_3
        } else {
            theme::INK
        }))
        .sense(egui::Sense::click()),
    );
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
