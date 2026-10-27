//! 设置窗口（V2.2 起，V3.10 按设计规范重排）：工位档案、测试项勾选、
//! WiFi 深度、按键参数、超时、连接。
//!
//! 布局语言（awesome-design-md design-md/claude）：分区一律内容卡
//! （CARD 色块、12px 圆角、color-block first），卡内输入件回退画布底
//! 加发丝描边（canvas-on-card），工位/深度用分段选项卡（category-tab），
//! 按键与超时各占一卡纵排（V3.10.2，真机反馈两列不直观）。任何改动即时写回
//! exe 同目录 config.toml。

use crate::app::E02App;
use crate::config::{self, IperfDir, WifiDepth};
use crate::engine::TestItem;
use crate::protocol::key_name;
use crate::ui::theme;

/// 小号说明文字（规范 caption：12.5px 次要色）。
fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme::MUTED));
}

/// 卡标题 + 右侧可选徽章（启用项数等）。
fn card_head(ui: &mut egui::Ui, title: &str, badge: Option<String>) {
    ui.horizontal(|ui| {
        theme::card_title(ui, title);
        if let Some(t) = badge {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                theme::badge(ui, &t, theme::INK, theme::SELECTED);
            });
        }
    });
}

/// 卡内起手：输入件回退画布底 + 内容抬到卡全宽。egui 的 Frame 只按
/// 内容 min_rect 收缩绘制（见 egui containers/frame.rs Prepared::paint），
/// 不显式抬 min 宽的话卡会缩成内容自然宽（真机上两列/网格卡参差不齐）。
fn card_begin(ui: &mut egui::Ui) {
    theme::card_inputs(ui);
    ui.set_min_width(ui.available_width());
}

/// 分段选项（规范 category-tab）：未选中透明底 + 次要色文字，
/// 选中奶油强底 + 暖墨文字（圆角随全局 8px）。
fn tab(ui: &mut egui::Ui, active: bool, text: &str) -> bool {
    let (bg, fg) = if active {
        (theme::SELECTED, theme::INK)
    } else {
        (egui::Color32::TRANSPARENT, theme::MUTED)
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(14.0).color(fg))
            .fill(bg)
            .stroke(egui::Stroke::NONE),
    )
    .clicked()
}

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    if !app.settings_open {
        return;
    }
    let mut open = true;
    let mut changed = false;
    // 居中初始位（V3.6）：锚定窗不可拖动
    let center = ctx.screen_rect().center();
    // 窗口定尺寸（V3.10.1）：不能用自动尺寸——egui Window 默认双向
    // 滚动，内容宽无界：卡会各自收缩换行、两列卡按漂移的可用宽拆分，
    // 内容超屏高时真机上整窗被撑满全屏。改为固定尺寸 + 关窗滚动 +
    // 显式纵向滚动区（内容宽有界 = 撑满卡，纵向超出滚动）。
    egui::Window::new("设置")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .vscroll(false)
        .fixed_size([620.0, 700.0])
        .default_pos(center - egui::vec2(310.0, 370.0))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width()); // 内容不窄于视口（滚动底完整）
                    ui.add_space(4.0);

                    // —— 工位档案：分段选择 + 启用徽章 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        card_head(
                            ui,
                            "工位档案",
                            Some(format!("启用 {} 项", app.config.active_items().len())),
                        );
                        ui.add_space(4.0);
                        let names: Vec<String> = app.config.profiles.keys().cloned().collect();
                        let station_before = app.config.station.clone();
                        ui.horizontal_wrapped(|ui| {
                            for n in &names {
                                if tab(ui, *n == station_before, n) && n != &station_before {
                                    app.config.station = n.clone();
                                }
                            }
                        });
                        changed |= app.config.station != station_before;
                    });

                    ui.add_space(12.0);

                    // —— 测试项：三列勾选 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        card_head(ui, "测试项", None);
                        ui.add_space(2.0);
                        caption(ui, "勾选写入当前工位");
                        ui.add_space(4.0);
                        if let Some(profile) = app.config.profiles.get_mut(&app.config.station) {
                            egui::Grid::new("items_grid")
                                .num_columns(3)
                                .spacing([24.0, 8.0])
                                .show(ui, |ui| {
                                    for (idx, item) in TestItem::ALL.iter().enumerate() {
                                        let slug = item.slug().to_string();
                                        let mut on = profile.items.contains(&slug);
                                        if ui.checkbox(&mut on, item.label()).changed() {
                                            if on {
                                                profile.items.push(slug);
                                            } else {
                                                profile.items.retain(|s| s != &slug);
                                            }
                                            changed = true;
                                        }
                                        if idx % 3 == 2 {
                                            ui.end_row();
                                        }
                                    }
                                });
                        }
                    });

                    ui.add_space(12.0);

                    // —— WiFi 测试深度 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        card_head(ui, "WiFi 测试深度", None);
                        ui.add_space(4.0);
                        let depth_before = app.config.wifi.depth;
                        ui.horizontal(|ui| {
                            if tab(ui, app.config.wifi.depth == WifiDepth::Driver, "驱动") {
                                app.config.wifi.depth = WifiDepth::Driver;
                            }
                            if tab(
                                ui,
                                app.config.wifi.depth == WifiDepth::Throughput,
                                "驱动 + 速率",
                            ) {
                                app.config.wifi.depth = WifiDepth::Throughput;
                            }
                        });
                        changed |= app.config.wifi.depth != depth_before;
                        if app.config.wifi.depth == WifiDepth::Throughput {
                            ui.add_space(6.0);
                            let t = &mut app.config.wifi.throughput;
                            // 方向（V3.12 分段选项卡）+ 单向时长
                            ui.horizontal(|ui| {
                                if tab(ui, t.direction == IperfDir::Up, "上行") {
                                    t.direction = IperfDir::Up;
                                    changed = true;
                                }
                                if tab(ui, t.direction == IperfDir::Down, "下行") {
                                    t.direction = IperfDir::Down;
                                    changed = true;
                                }
                                if tab(ui, t.direction == IperfDir::Both, "双向") {
                                    t.direction = IperfDir::Both;
                                    changed = true;
                                }
                                ui.separator();
                                ui.label("时长");
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut t.duration_secs)
                                            .suffix("s")
                                            .range(2..=30),
                                    )
                                    .changed();
                            });
                            // 阈值：上/下吞吐 + TCP 重传
                            ui.horizontal(|ui| {
                                ui.label("≥↑");
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut t.min_mbps_up)
                                            .suffix("Mbps")
                                            .range(0.0..=300.0),
                                    )
                                    .changed();
                                ui.separator();
                                ui.label("≥↓");
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut t.min_mbps_down)
                                            .suffix("Mbps")
                                            .range(0.0..=300.0),
                                    )
                                    .changed();
                                ui.separator();
                                ui.label("重传 ≤");
                                changed |= ui
                                    .add(egui::DragValue::new(&mut t.max_retr).range(0..=10000))
                                    .changed();
                            });
                            ui.horizontal(|ui| {
                                changed |= ui
                                    .checkbox(&mut t.retest, "边缘失败自动复测一次")
                                    .changed();
                                ui.separator();
                                ui.label("端口");
                                changed |= ui
                                    .add(egui::DragValue::new(&mut t.server_port).range(0..=65535))
                                    .changed();
                            });
                            ui.add_space(6.0);
                            // 产线 SSID：耳机 MIPS/uClibc 固件无 Android cmd wifi，
                            // 未入网时推送 wpa_supplicant.conf 并重连
                            ui.horizontal(|ui| {
                                ui.label("产线 SSID");
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut app.config.wifi.ssid)
                                            .hint_text("耳机已入网则留空")
                                            .desired_width(150.0),
                                    )
                                    .changed();
                                ui.separator();
                                ui.label("密码");
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut app.config.wifi.psk)
                                            .hint_text("开放网络留空")
                                            .desired_width(110.0),
                                    )
                                    .changed();
                            });
                            ui.add_space(2.0);
                            caption(
                                ui,
                                "iperf3 上/下行吞吐（窗口中位数）与 TCP 重传阈值；0=不判。                                 需固件集成 iperf3，PC 与设备同网段；未入网时按产线 SSID 推送配置并重连",
                            );
                        }
                    });

                    ui.add_space(12.0);

                    // —— 按键测试 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        card_head(ui, "按键测试", None);
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            for code in [4u8, 5, 6] {
                                let mut on = app.config.keys.required.contains(&code);
                                if ui.checkbox(&mut on, key_name(code)).changed() {
                                    if on {
                                        if !app.config.keys.required.contains(&code) {
                                            app.config.keys.required.push(code);
                                        }
                                    } else {
                                        app.config.keys.required.retain(|c| *c != code);
                                    }
                                    changed = true;
                                }
                            }
                        });
                        ui.add_space(4.0);
                        caption(ui, "固件回读格式 key 4/5/6，收齐必测键自动通过");
                    });

                    ui.add_space(12.0);

                    // —— 超时（一行两组，风格同 WiFi 速率行）——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        card_head(ui, "超时", None);
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("单项命令");
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut app.config.timeouts.item_secs)
                                        .suffix("s")
                                        .range(0..=600),
                                )
                                .changed();
                            ui.separator();
                            ui.label("人工应答");
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut app.config.timeouts.manual_secs)
                                        .suffix("s")
                                        .range(0..=600),
                                )
                                .changed();
                        });
                        ui.add_space(2.0);
                        caption(ui, "0 = 不限制；超时判 FAIL 并继续");
                    });

                    ui.add_space(12.0);

                    // —— 连接 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        card_head(ui, "连接", None);
                        ui.add_space(4.0);
                        changed |= ui
                            .checkbox(&mut app.config.conn.auto_start, "插入设备后自动开始全测")
                            .changed();
                        ui.add_space(2.0);
                        caption(ui, "检测到 USB 设备上线即开始；测试中拔出立即中止");
                    });
                    ui.add_space(4.0);
                });
        });
    app.settings_open = open;
    // 即时保存（V3.7）：改动即落盘 config.toml，成功静默、失败记日志
    if changed {
        let saved = app.config.to_toml().and_then(|t| {
            std::fs::write(config::default_path(), t).map_err(|e| anyhow::anyhow!("写入失败: {e}"))
        });
        if let Err(e) = saved {
            app.push_log(format!("配置保存失败: {e}"));
        }
    }
}
