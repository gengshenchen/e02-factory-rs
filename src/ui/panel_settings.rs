//! 设置窗口（V2.2）：工位档案切换、测试项勾选、WiFi 深度、按键参数、超时。
//! 改动即时生效（每次测试线程取当前配置快照）；"保存"写回 exe 同目录 config.toml。

use crate::app::E02App;
use crate::config;
use crate::config::WifiDepth;
use crate::engine::TestItem;
use crate::protocol::key_name;

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    if !app.settings_open {
        return;
    }
    let mut open = true;
    let mut save_msg: Option<String> = None;
    // 居中初始位（V3.6）：锚定窗不可拖动
    let center = ctx.screen_rect().center();
    egui::Window::new("设置")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(center - egui::vec2(230.0, 300.0))
        .show(ctx, |ui| {
            ui.set_min_width(420.0);

            // —— 工位档案 ——
            ui.strong("工位档案");
            let names: Vec<String> = app.config.profiles.keys().cloned().collect();
            egui::ComboBox::from_id_salt("station")
                .selected_text(&app.config.station)
                .show_ui(ui, |ui| {
                    for n in &names {
                        let selected = *n == app.config.station;
                        if ui.selectable_label(selected, n).clicked() {
                            app.config.station = n.clone();
                        }
                    }
                });
            ui.label(format!(
                "当前工位启用 {} 项",
                app.config.active_items().len()
            ));

            ui.separator();
            ui.strong("测试项（勾选写入当前工位）");
            if let Some(profile) = app.config.profiles.get_mut(&app.config.station) {
                egui::Grid::new("items_grid").num_columns(3).show(ui, |ui| {
                    for (idx, item) in TestItem::ALL.iter().enumerate() {
                        let slug = item.slug().to_string();
                        let mut on = profile.items.contains(&slug);
                        if ui.checkbox(&mut on, item.label()).changed() {
                            if on {
                                profile.items.push(slug);
                            } else {
                                profile.items.retain(|s| *s != slug);
                            }
                        }
                        if idx % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
            }

            ui.separator();
            ui.strong("WiFi 测试深度");
            ui.horizontal(|ui| {
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
            });
            if app.config.wifi.depth == WifiDepth::Throughput {
                let t = &mut app.config.wifi.throughput;
                ui.horizontal(|ui| {
                    ui.label("ping 包数");
                    ui.add(egui::DragValue::new(&mut t.ping_count).range(1..=100));
                    ui.label("丢包≤%");
                    ui.add(egui::DragValue::new(&mut t.loss_max_pct).range(0.0..=100.0));
                    ui.label("avg≤ms");
                    ui.add(egui::DragValue::new(&mut t.rtt_max_ms).range(1.0..=2000.0));
                });
            }

            ui.separator();
            ui.strong("按键测试");
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
                    }
                }
            });
            ui.small("固件回读格式 key 4/5/6（多功能键/开机键/音量+）");

            ui.separator();
            ui.strong("超时");
            ui.horizontal(|ui| {
                ui.label("单项命令(s)");
                ui.add(egui::DragValue::new(&mut app.config.timeouts.item_secs).range(0..=600));
                ui.label("人工应答(s)");
                ui.add(egui::DragValue::new(&mut app.config.timeouts.manual_secs).range(0..=600));
            });
            ui.small("0 = 不限制；命令超时判 FAIL 并继续，人工超时自动判不合格");

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("保存到 config.toml").clicked() {
                    save_msg = Some(
                        match app.config.to_toml().and_then(|t| {
                            std::fs::write(config::default_path(), t)
                                .map_err(|e| anyhow::anyhow!("写入失败: {e}"))
                        }) {
                            Ok(()) => format!("已保存 {}", config::default_path().display()),
                            Err(e) => format!("保存失败: {e:#}"),
                        },
                    );
                }
                ui.label("改动即时生效，保存后重启仍保留");
            });
        });
    app.settings_open = open;
    if let Some(msg) = save_msg {
        app.push_log(msg);
    }
}
