//! 应用状态与窗口装配。

use crate::engine::{TestItem, TestResult};
use crate::protocol::DeviceInfo;
use crate::ui;

/// 内嵌中文字体：Droid Sans Fallback（Apache-2.0，完整 CJK 覆盖，3.9MB）。
/// M6 打包阶段如需更精致字形，可换 Noto Sans SC 子集。
const CJK_FONT: &[u8] = include_bytes!("../assets/fonts/DroidSansFallbackFull.ttf");

/// 应用全局状态。
pub struct E02App {
    /// 被测设备信息（原 Deviceinfo 结构）
    pub device: DeviceInfo,
    /// 各测试项结果，顺序与左侧面板一致
    pub results: Vec<TestResult>,
    /// 待处理的人工判定请求（M5 起由 worker 线程触发）
    pub manual: Option<ui::dialog_manual::ManualAsk>,
    /// 中央媒体区占位说明（M3 图片 / M4 视频替换）
    pub media_note: String,
}

impl E02App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        Self {
            device: DeviceInfo::default(),
            results: TestItem::ALL.map(TestResult::new).to_vec(),
            manual: None,
            media_note: "媒体区占位：M3 接入拍照图片，M4 接入视频播放".into(),
        }
    }
}

/// 注册中文字体到 egui，并设为 Proportional 家族首位。
fn install_cjk_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("droid-cjk".into(), egui::FontData::from_static(CJK_FONT));
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "droid-cjk".into());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("droid-cjk".into());
    ctx.set_fonts(fonts);
}

impl eframe::App for E02App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.heading("E02 工厂测试工具");
            ui.add_space(2.0);
        });
        egui::SidePanel::left("test_panel")
            .resizable(false)
            .default_width(216.0)
            .show(ctx, |ui| ui::panel_tests::show(ui, self));
        egui::SidePanel::right("device_panel")
            .resizable(false)
            .default_width(300.0)
            .show(ctx, |ui| ui::panel_device::show(ui, self));
        egui::CentralPanel::default().show(ctx, |ui| ui::panel_media::show(ui, self));
        // 人工判定模态窗（指示灯 / MIC·喇叭），有请求时才显示
        ui::dialog_manual::show(ctx, self);
    }
}
