//! 设置窗口（V2.2 起，V3.7 重排 + 即时保存）：工位档案、测试项勾选、
//! WiFi 深度、按键参数、超时。任何改动即时写回 exe 同目录 config.toml
//! （无需手动点保存）；改动即时生效（每次测试线程取当前配置快照）。

use crate::app::E02App;
use crate::config::{self, WifiDepth};
use crate::engine::TestItem;
use crate::protocol::key_name;
use crate::ui::theme;

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    if !app.settings_open {
        return;
    }
    let mut open = true;
    let mut changed = false;
    // 居中初始位（V3.6）：锚定窗不可拖动
    let center = ctx.screen_rect().center();
    egui::Window::new("设置")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(center - egui::vec2(250.0, 330.0))
        .show(ctx, |ui| {
            ui.set_min_width(500.0);

            // —— 工位档案 ——
            theme::section(ui, "工位档案");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("当前工位");
                let station_before = app.config.station.clone();
                let names: Vec<String> = app.config.profiles.keys().cloned().collect();
                egui::ComboBox::from_id_salt("station")
                    .selected_text(&app.config.station)
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for n in &names {
                            ui.selectable_value(&mut app.config.station, n.clone(), n);
                        }
                    });
                changed |= app.config.station != station_before;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("启用 {} 项", app.config.active_items().len()))
                            .small()
                            .color(theme::MUTED),
                    );
                });
            });

            ui.add_space(10.0);
            ui.separator();

            // —— 测试项 ——
            theme::section(ui, "测试项");
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("勾选写入当前工位")
                    .small()
                    .color(theme::MUTED),
            );
            ui.add_space(4.0);
            if let Some(profile) = app.config.profiles.get_mut(&app.config.station) {
                egui::Grid::new("items_grid")
                    .num_columns(3)
                    .spacing([20.0, 8.0])
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

            ui.add_space(10.0);
            ui.separator();

            // —— WiFi 测试深度 ——
            theme::section(ui, "WiFi 测试深度");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let depth_before = app.config.wifi.depth;
                ui.selectable_value(
                    &mut app.config.wifi.depth,
                    WifiDepth::Driver,
                    "驱动（camera-factory 5）",
                );
                ui.selectable_value(
                    &mut app.config.wifi.depth,
                    WifiDepth::Throughput,
                    "驱动 + 速率（ping 网关）",
                );
                changed |= app.config.wifi.depth != depth_before;
            });
            if app.config.wifi.depth == WifiDepth::Throughput {
                ui.add_space(4.0);
                let t = &mut app.config.wifi.throughput;
                ui.horizontal(|ui| {
                    ui.label("ping 包数");
                    changed |= ui
                        .add(egui::DragValue::new(&mut t.ping_count).range(1..=100))
                        .changed();
                    ui.separator();
                    ui.label("丢包 ≤");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut t.loss_max_pct)
                                .suffix("%")
                                .range(0.0..=100.0),
                        )
                        .changed();
                    ui.separator();
                    ui.label("平均延迟 ≤");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut t.rtt_max_ms)
                                .suffix("ms")
                                .range(1.0..=2000.0),
                        )
                        .changed();
                });
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("速率 = 驱动通过后 ping 网关，统计丢包率与平均延迟")
                        .small()
                        .color(theme::MUTED),
                );
            }

            ui.add_space(10.0);
            ui.separator();

            // —— 按键测试 ——
            theme::section(ui, "按键测试");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("必测键:");
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
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("固件回读格式 key 4/5/6，收齐必测键自动通过")
                    .small()
                    .color(theme::MUTED),
            );

            ui.add_space(10.0);
            ui.separator();

            // —— 超时 ——
            theme::section(ui, "超时");
            ui.add_space(6.0);
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
            ui.label(
                egui::RichText::new("0 = 不限制；命令超时判 FAIL 并继续，人工超时自动判不合格")
                    .small()
                    .color(theme::MUTED),
            );

            ui.add_space(10.0);
            ui.separator();

            // —— 连接 ——
            theme::section(ui, "连接");
            ui.add_space(6.0);
            changed |= ui
                .checkbox(&mut app.config.conn.auto_start, "插入设备后自动开始全测")
                .changed();
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("检测到 USB 设备上线即开始；测试中拔出立即中止")
                    .small()
                    .color(theme::MUTED),
            );
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
