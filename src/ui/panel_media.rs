//! 中央媒体区：M3 显示拍照图片，M4 嵌入 libmpv 视频播放。M0 为占位说明。

use crate::app::E02App;

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    ui.centered_and_justified(|ui| {
        ui.label(&app.media_note);
    });
}
