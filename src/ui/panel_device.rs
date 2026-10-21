//! 右侧面板：设备信息只读字段（对应原对话框 Edit 控件）+ 总体结果。

use crate::app::E02App;
use crate::engine::Verdict;
use egui::Color32;

pub fn show(ui: &mut egui::Ui, app: &E02App) {
    ui.heading("设备信息");
    ui.add_space(6.0);
    egui::Grid::new("device_info")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            field(ui, "SN", &app.device.sn);
            field(ui, "WiFi MAC", &app.device.wifi_mac);
            field(ui, "蓝牙 MAC", &app.device.bt_mac);
            field(ui, "软件版本", &app.device.t32_ver);
            field(ui, "硬件版本", &app.device.hardware_ver);
            field(ui, "电池", &app.device.battery);
        });

    ui.add_space(12.0);
    ui.separator();
    ui.heading("总体结果");
    ui.add_space(6.0);

    let done: Vec<_> = app
        .results
        .iter()
        .filter(|r| r.verdict != Verdict::Pending)
        .collect();
    if done.is_empty() {
        ui.colored_label(Color32::GRAY, "等待测试");
    } else {
        let fail = done.iter().filter(|r| !r.verdict.is_pass()).count();
        if fail == 0 {
            ui.colored_label(Color32::GREEN, format!("已测 {} 项，全部通过", done.len()));
        } else {
            ui.colored_label(
                Color32::RED,
                format!("已测 {} 项，{fail} 项失败", done.len()),
            );
        }
    }
}

fn field(ui: &mut egui::Ui, name: &str, value: &str) {
    ui.label(name);
    // 原程序为只读 Edit 控件；空值显示占位符
    let text = if value.is_empty() { "-" } else { value };
    ui.monospace(text);
    ui.end_row();
}
