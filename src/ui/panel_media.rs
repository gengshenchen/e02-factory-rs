//! 中央媒体区：拍照图片显示 / 录像抽帧回放（对应原 CImage 与 VLC 播放区域）。

use crate::app::{E02App, MediaState};

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    match &app.media {
        MediaState::Note(note) => {
            ui.centered_and_justified(|ui| {
                ui.label(note);
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
                ui.small(format!(
                    "录像回放 {}/{}{}",
                    idx + 1,
                    frames.len(),
                    if *has_audio { "（含音频流）" } else { "" }
                ));
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
