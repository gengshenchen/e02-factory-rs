//! 视觉主题（V3.2）：一套克制的浅色设计令牌。
//!
//! 近黑墨色文字层级 + 单一品牌蓝 + 语义色（绿/红/灰），白色卡片浮在
//! 浅灰底上，控件与窗口统一圆角；媒体区与日志用深底突出内容。
//! 内嵌字体只有 ASCII+CJK，因此不做装饰字形——状态点用绘制圆点，
//! 徽章用色块 Frame，避免豆腐块。

use egui::{Color32, Context, FontId, Margin, Rounding, Stroke, Style, TextStyle, Visuals};

/// 文字：标题近黑
pub const INK: Color32 = Color32::from_rgb(23, 28, 38);
/// 文字：正文
pub const INK_2: Color32 = Color32::from_rgb(66, 74, 90);
/// 文字：次要/占位
pub const INK_3: Color32 = Color32::from_rgb(146, 153, 166);
/// 品牌蓝（主操作/进行中/选中）
pub const ACCENT: Color32 = Color32::from_rgb(32, 94, 210);
/// 浅蓝底（选中行/进行中行）
pub const ACCENT_BG: Color32 = Color32::from_rgb(226, 236, 252);
/// 语义绿（PASS）
pub const OK: Color32 = Color32::from_rgb(24, 135, 78);
/// 绿色浅底（徽章）
pub const OK_BG: Color32 = Color32::from_rgb(222, 242, 230);
/// 语义红（FAIL/停止）
pub const BAD: Color32 = Color32::from_rgb(205, 38, 49);
/// 红色浅底
pub const BAD_BG: Color32 = Color32::from_rgb(252, 228, 230);
/// 应用底色（面板背景）
pub const BG: Color32 = Color32::from_rgb(242, 244, 247);
/// 卡片面（白色）
pub const SURFACE: Color32 = Color32::from_rgb(255, 255, 255);
/// 发丝线
pub const LINE: Color32 = Color32::from_rgb(226, 230, 237);
/// 媒体区/日志深底
pub const DARK: Color32 = Color32::from_rgb(22, 26, 34);
/// 深底上的文字
pub const DARK_INK: Color32 = Color32::from_rgb(172, 181, 195);
/// 深底上的次要文字
pub const DARK_INK_2: Color32 = Color32::from_rgb(108, 117, 131);
/// 深底上的品牌蓝（日志状态高亮用）
pub const ACCENT_ON_DARK: Color32 = Color32::from_rgb(124, 162, 247);

/// 应用全局主题（`E02App::new` 调用一次）。
pub fn apply(ctx: &Context) {
    // 字号层级（默认偏小偏挤，整体上调并拉开差距）+ 布局节奏（留白大一档）
    let mut style = Style {
        text_styles: [
            (TextStyle::Heading, FontId::proportional(21.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(15.0)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
            (TextStyle::Small, FontId::proportional(12.5)),
        ]
        .into(),
        spacing: egui::Spacing {
            item_spacing: egui::vec2(8.0, 6.0),
            button_padding: egui::vec2(14.0, 7.0),
            window_margin: Margin::same(14.0),
            ..Default::default()
        },
        ..Default::default()
    };

    let v = &mut style.visuals;
    *v = Visuals::light();
    // 0.29 面板底色取 widgets.noninteractive.bg_fill（无独立 panel_bg 字段），
    // 下面统一设为白色；中央媒体区/日志用显式 DARK Frame 覆盖
    v.window_fill = SURFACE;
    v.extreme_bg_color = Color32::from_rgb(249, 250, 252); // 输入框/下拉底
    v.faint_bg_color = Color32::from_rgb(247, 248, 250);
    v.hyperlink_color = ACCENT;
    v.error_fg_color = BAD;
    v.warn_fg_color = Color32::from_rgb(170, 112, 8);
    v.selection.bg_fill = ACCENT_BG;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.popup_shadow = egui::epaint::Shadow::NONE;
    v.window_shadow = egui::epaint::Shadow::NONE;

    // 控件状态：白底浅描边，悬停微沉，激活品牌蓝
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.fg_stroke = Stroke::new(1.0, INK_2);
    w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    w.inactive.bg_fill = SURFACE;
    w.inactive.fg_stroke = Stroke::new(1.0, INK_2);
    w.inactive.bg_stroke = Stroke::new(1.0, LINE);
    w.hovered.bg_fill = Color32::from_rgb(244, 246, 250);
    w.hovered.fg_stroke = Stroke::new(1.0, INK);
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(203, 211, 224));
    w.active.bg_fill = ACCENT;
    w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.open.bg_fill = SURFACE;
    w.open.fg_stroke = Stroke::new(1.0, INK);
    w.open.bg_stroke = Stroke::new(1.0, Color32::from_rgb(203, 211, 224));

    // 统一圆角：控件 7，窗口 12
    let r = Rounding::same(7.0);
    w.noninteractive.rounding = r;
    w.inactive.rounding = r;
    w.hovered.rounding = r;
    w.active.rounding = r;
    w.open.rounding = r;
    style.visuals.window_rounding = Rounding::same(12.0);
    style.visuals.menu_rounding = Rounding::same(10.0);

    ctx.set_style(style);
}

/// 状态圆点（避免字形符号：直接画圆）。
pub fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// 胶囊徽章：浅底色块 + 同系深色文字（PASS/FAIL/今日统计用）。
pub fn badge(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::default()
        .fill(bg)
        .rounding(Rounding::same(999.0))
        .inner_margin(Margin::symmetric(10.0, 3.0))
        .show(ui, |ui| {
            ui.colored_label(fg, egui::RichText::new(text).size(13.0).strong());
        });
}

/// 主按钮（品牌蓝实底白字）：开始全测等唯一主动作。
pub fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .color(Color32::WHITE)
            .strong()
            .size(16.0),
    )
    .fill(ACCENT)
    .stroke(Stroke::NONE)
}

/// 危险按钮（红实底白字）：停止测试。
pub fn danger(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .color(Color32::WHITE)
            .strong()
            .size(15.0),
    )
    .fill(BAD)
    .stroke(Stroke::NONE)
}

/// 今日看板徽章组（顶栏与报告中心共用）。
pub fn today_badges(ui: &mut egui::Ui, t: &crate::report::DayStats) {
    badge(ui, &format!("今日 {} 台", t.total), INK_2, BG);
    let (pf, pb) = if t.pass > 0 { (OK, OK_BG) } else { (INK_3, BG) };
    badge(ui, &format!("PASS {}", t.pass), pf, pb);
    let (ff, fb) = if t.fail > 0 {
        (BAD, BAD_BG)
    } else {
        (INK_3, BG)
    };
    badge(ui, &format!("FAIL {}", t.fail), ff, fb);
}
