//! 中央媒体区：拍照图片显示 / 录像抽帧回放（深底"屏幕井"）。
//!
//! 人工判定条（V3.11）：录像/拍照等人工项待判定时不再弹居中窗
//! （正压住要判的画面/照片），改为井底内嵌判定条——媒体照常显示，
//! 操作员对着内容点 合格/不合格（快捷键 P/F）。

use crate::app::{E02App, MediaState};
use crate::ui::theme;
use std::time::Instant;

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    // 判定条先占住井底，余下空间画媒体（判定期间内容完整可见）
    if app.manual.is_some() {
        egui::TopBottomPanel::bottom("judge_bar")
            .frame(egui::Frame::default().inner_margin(egui::Margin {
                left: 8.0,
                right: 8.0,
                top: 0.0,
                bottom: 8.0,
            }))
            .show_inside(ui, |ui| judge_bar(ui, app));
    }
    match &app.media {
        MediaState::Note(note) => {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new(note.as_str()).color(theme::ON_DARK));
            });
        }
        MediaState::Photo { texture, size } => {
            draw_fit(ui, texture, *size);
        }
        MediaState::Video {
            frames,
            sizes,
            idx,
            has_audio,
            ..
        } => {
            draw_fit(ui, &frames[*idx], sizes[*idx]);
            ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "录像循环回放 {}/{}{}",
                        idx + 1,
                        frames.len(),
                        if *has_audio {
                            "（含音频流）"
                        } else {
                            "（无音频流）"
                        }
                    ))
                    .small()
                    .color(theme::ON_DARK_SOFT),
                );
            });
        }
    }
}

/// 等比缩放居中绘制（对应原 CImage StretchBlt 的等比逻辑）。
fn draw_fit(ui: &mut egui::Ui, texture: &egui::TextureHandle, size: [f32; 2]) {
    let avail = ui.available_size();
    if size[0] <= 0.0 || size[1] <= 0.0 {
        return;
    }
    let scale = (avail.x / size[0]).min(avail.y / size[1]);
    let target = egui::Vec2::new(size[0] * scale, size[1] * scale);
    ui.centered_and_justified(|ui| {
        ui.add(egui::Image::new(texture).fit_to_exact_size(target));
    });
}

/// 井底人工判定条（V3.11，替代原居中模态窗）：左问题+倒计时、
/// 右合格/不合格大按钮；P/F 快捷键与按钮等效。答完即收，worker 继续。
fn judge_bar(ui: &mut egui::Ui, app: &mut E02App) {
    let Some(ask) = &app.manual else {
        return;
    };
    let (title, question, deadline) = (ask.title.clone(), ask.question.clone(), ask.deadline);
    egui::Frame::default()
        .fill(theme::DARK_ELEVATED)
        .rounding(egui::Rounding::same(10.0))
        .inner_margin(egui::Margin::symmetric(14.0, 8.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{title}："))
                                .strong()
                                .color(theme::ON_DARK),
                        );
                        ui.label(egui::RichText::new(&question).color(theme::ON_DARK));
                    });
                    if let Some(dl) = deadline {
                        let left = dl.saturating_duration_since(Instant::now()).as_secs();
                        let cd = egui::RichText::new(format!("{left} 秒未应答自动判不合格"))
                            .small()
                            .color(if left <= 10 {
                                theme::BAD
                            } else {
                                theme::ON_DARK_SOFT
                            });
                        ui.label(if left <= 10 { cd.strong() } else { cd });
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("不合格")
                                    .color(egui::Color32::WHITE)
                                    .strong()
                                    .size(15.0),
                            )
                            .fill(theme::BAD)
                            .stroke(egui::Stroke::NONE)
                            .min_size(egui::vec2(118.0, 40.0)),
                        )
                        .clicked()
                    {
                        answer(app, false);
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("合格")
                                    .color(egui::Color32::WHITE)
                                    .strong()
                                    .size(15.0),
                            )
                            .fill(theme::OK_STRONG)
                            .stroke(egui::Stroke::NONE)
                            .min_size(egui::vec2(118.0, 40.0)),
                        )
                        .clicked()
                    {
                        answer(app, true);
                    }
                });
            });
        });
    // P=合格 / F=不合格（编辑框捕获键盘时不抢键）
    let key =
        |k: egui::Key| ui.ctx().input(|i| i.key_pressed(k)) && !ui.ctx().wants_keyboard_input();
    if key(egui::Key::P) {
        answer(app, true);
    } else if key(egui::Key::F) {
        answer(app, false);
    }
}

fn answer(app: &mut E02App, yes: bool) {
    if let Some(ask) = app.manual.take() {
        ask.answer(yes);
    }
}
