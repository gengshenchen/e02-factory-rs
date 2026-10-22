//! 视觉主题（V3.3，依据 Claude 设计规范
//! github.com/VoltAgent/awesome-design-md design-md/claude）。
//!
//! 三件套：暖米色画布 + 珊瑚橙主操作 + 暖黑产品面板（媒体井/日志）。
//! 深度靠色块分层而非阴影（color-block first, shadow rare）；发丝线
//! 描边读作"抬升一级"而非墨线；控件 8px / 窗口 12px 圆角。
//! 内嵌字体只有 ASCII+CJK：状态点画圆、徽章用色块，不用符号字形。

use crate::report::DayStats;
use egui::{Color32, Context, FontId, Margin, Rounding, Stroke, Style, TextStyle, Visuals};

/// 文字：标题暖墨（近黑，非纯黑）
pub const INK: Color32 = Color32::from_rgb(20, 20, 19);
/// 文字：正文
pub const BODY: Color32 = Color32::from_rgb(61, 61, 58);
/// 文字：次要/占位
pub const MUTED: Color32 = Color32::from_rgb(108, 106, 100);
/// 主操作珊瑚橙：唯一品牌色，只给主按钮/链接/进行中
pub const PRIMARY: Color32 = Color32::from_rgb(204, 120, 92);
/// 珊瑚橙按下态（按下变深，无额外悬停花样）
pub const PRIMARY_ACTIVE: Color32 = Color32::from_rgb(169, 88, 62);
/// 选中/进行中行底：奶油强一档
pub const SELECTED: Color32 = Color32::from_rgb(232, 224, 210);
/// 语义绿（PASS 状态点）
pub const OK: Color32 = Color32::from_rgb(93, 184, 114);
/// 语义绿深档：绿底白字按钮 / 深色文字用（保对比度）
pub const OK_STRONG: Color32 = Color32::from_rgb(61, 143, 82);
/// 绿色浅底（徽章）
pub const OK_BG: Color32 = Color32::from_rgb(224, 240, 228);
/// 语义红（FAIL/停止）
pub const BAD: Color32 = Color32::from_rgb(198, 69, 69);
/// 红色浅底
pub const BAD_BG: Color32 = Color32::from_rgb(243, 224, 224);
/// 警告琥珀（临期倒计时等）
pub const WARN: Color32 = Color32::from_rgb(212, 160, 23);
/// 画布：一切浅色面的底（暖米色，非纯白/冷灰）
pub const CANVAS: Color32 = Color32::from_rgb(250, 249, 245);
/// 内容卡：比画布深一档的奶油
pub const CARD: Color32 = Color32::from_rgb(239, 233, 222);
/// 悬停微沉
pub const SOFT: Color32 = Color32::from_rgb(245, 240, 232);
/// 发丝线（浅色面描边）
pub const HAIRLINE: Color32 = Color32::from_rgb(230, 223, 216);
/// 媒体井/日志：暖黑产品面板
pub const DARK: Color32 = Color32::from_rgb(24, 23, 21);
/// 暖黑上的抬升面（日志里的按钮）
pub const DARK_ELEVATED: Color32 = Color32::from_rgb(37, 35, 32);
/// 暖黑上的文字（奶油白，呼应画布）
pub const ON_DARK: Color32 = Color32::from_rgb(250, 249, 245);
/// 暖黑上的次要文字
pub const ON_DARK_SOFT: Color32 = Color32::from_rgb(160, 157, 150);
/// 暖黑上的状态强调（终端状态指示）
pub const TEAL: Color32 = Color32::from_rgb(93, 184, 166);

/// 应用全局主题（`E02App::new` 调用一次）。
pub fn apply(ctx: &Context) {
    // 字号层级 + 布局节奏（间距基调 4px，卡内留白慷慨）
    let mut style = Style {
        text_styles: [
            (TextStyle::Heading, FontId::proportional(20.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(14.5)),
            (TextStyle::Monospace, FontId::monospace(13.5)),
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
    // 下面统一设为画布色；媒体井/日志用显式 DARK Frame 覆盖
    v.window_fill = CANVAS;
    v.faint_bg_color = SOFT;
    v.extreme_bg_color = CARD; // 输入框/下拉底：比画布深一档
    v.hyperlink_color = PRIMARY;
    v.error_fg_color = BAD;
    v.warn_fg_color = WARN;
    v.selection.bg_fill = Color32::from_rgb(243, 230, 222); // 珊瑚 15% 叠画布
    v.selection.stroke = hairline(PRIMARY);
    v.popup_shadow = egui::epaint::Shadow::NONE;
    v.window_shadow = egui::epaint::Shadow::NONE;

    // 控件状态：画布底 + 发丝描边（= 次级按钮形态），悬停微沉，按下珊瑚深档
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = CANVAS;
    w.noninteractive.fg_stroke = hairline(BODY);
    w.noninteractive.bg_stroke = hairline(HAIRLINE);
    w.inactive.bg_fill = CANVAS;
    w.inactive.fg_stroke = hairline(BODY);
    w.inactive.bg_stroke = hairline(HAIRLINE);
    w.hovered.bg_fill = SOFT;
    w.hovered.fg_stroke = hairline(INK);
    w.hovered.bg_stroke = hairline(Color32::from_rgb(208, 199, 188));
    w.active.bg_fill = PRIMARY_ACTIVE;
    w.active.fg_stroke = hairline(Color32::WHITE);
    w.active.bg_stroke = hairline(PRIMARY_ACTIVE);
    w.open.bg_fill = CARD;
    w.open.fg_stroke = hairline(INK);
    w.open.bg_stroke = hairline(HAIRLINE);

    // 圆角分级：控件 8，窗口 12
    let r = Rounding::same(8.0);
    w.noninteractive.rounding = r;
    w.inactive.rounding = r;
    w.hovered.rounding = r;
    w.active.rounding = r;
    w.open.rounding = r;
    style.visuals.window_rounding = Rounding::same(12.0);
    style.visuals.menu_rounding = Rounding::same(10.0);

    ctx.set_style(style);
}

/// 1px 发丝描边。宽度字面量显式 f32：1.93+ 编译器对泛型上下文里的
/// 浮点回退字面量告警（float-literal-f32-fallback），收口到一处。
pub fn hairline(color: Color32) -> Stroke {
    Stroke::new(1.0_f32, color)
}

/// 状态圆点（避免字形符号：直接画圆）。
pub fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// 胶囊徽章：浅底色块 + 同系深色文字（今日统计/整机结果用）。
pub fn badge(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::default()
        .fill(bg)
        .rounding(Rounding::same(999.0))
        .inner_margin(Margin::symmetric(10.0, 3.0))
        .show(ui, |ui| {
            ui.colored_label(fg, egui::RichText::new(text).size(13.0).strong());
        });
}

/// 主按钮（珊瑚橙实底白字）：开始全测等唯一主动作，全系统仅此一处用色。
pub fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .color(Color32::WHITE)
            .strong()
            .size(16.0),
    )
    .fill(PRIMARY)
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
pub fn today_badges(ui: &mut egui::Ui, t: &DayStats) {
    badge(ui, &format!("今日 {} 台", t.total), INK, CARD);
    let (pf, pb) = if t.pass > 0 {
        (OK_STRONG, OK_BG)
    } else {
        (MUTED, SOFT)
    };
    badge(ui, &format!("PASS {}", t.pass), pf, pb);
    let (ff, fb) = if t.fail > 0 {
        (BAD, BAD_BG)
    } else {
        (MUTED, SOFT)
    };
    badge(ui, &format!("FAIL {}", t.fail), ff, fb);
}
