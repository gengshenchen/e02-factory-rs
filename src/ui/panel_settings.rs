//! 设置窗口（遵循 design-md/claude 规范）：
//! - 产品型号（E02W WiFi 版 / E02E eSIM 版）；
//! - 工位选择（模组工位 / 整机工位 / 出厂写号），即时写入 `config.toml`；
//! - 下方配置卡按工位显示：测试工位显示测试项与 WiFi/按键/连接参数，出厂写号显示工单版本、
//!   写入校验规则与 E02E 4G 参数模板编辑。

use crate::app::E02App;
use crate::config::{self, Config, FourGCfg, GateMode, IperfDir, ModelKind, WifiDepth};
use crate::engine::TestItem;
use crate::protocol::key_name;
use crate::provision;
use crate::ui::theme;

fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme::MUTED));
}

fn card_head(ui: &mut egui::Ui, title: &str, badge: Option<String>) {
    ui.horizontal(|ui| {
        theme::card_title(ui, title);
        if let Some(t) = badge {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                theme::badge(ui, &t, theme::INK, theme::SELECTED);
            });
        }
    });
}

fn card_begin(ui: &mut egui::Ui) {
    theme::card_inputs(ui);
    ui.set_min_width(ui.available_width());
}

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

fn save_ledger(app: &mut E02App, ok_msg: String) {
    let path = crate::ledger::path_of(&app.config);
    match app.ledger_view.save(&path) {
        Ok(()) => app.push_log(format!("{ok_msg}（{}）", path.display())),
        Err(e) => app.push_log(format!("绑定记录保存失败: {e}")),
    }
}

fn export_csv(app: &mut E02App) {
    let path = crate::ledger::path_of(&app.config)
        .parent()
        .map(|d| {
            d.join(format!(
                "绑定记录-{}.csv",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ))
        })
        .unwrap_or_else(|| "绑定记录.csv".into());
    match std::fs::write(&path, app.ledger_view.to_csv()) {
        Ok(()) => app.push_log(format!(
            "绑定记录已导出 {} 台: {}",
            app.ledger_view.len(),
            path.display()
        )),
        Err(e) => app.push_log(format!("绑定记录导出失败: {e}")),
    }
}

pub fn show(ctx: &egui::Context, app: &mut E02App) {
    if !app.settings_open {
        return;
    }
    let mut open = true;
    let mut changed = false;
    let locked = app.busy || app.ship.busy;
    let center = ctx.screen_rect().center();

    egui::Window::new("设置")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .vscroll(false)
        .fixed_size([640.0, 700.0])
        .default_pos(center - egui::vec2(320.0, 350.0))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.add_space(4.0);

                    // —— 1. 产品型号 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        card_head(ui, "产品型号", None);
                        ui.add_space(4.0);
                        ui.add_enabled_ui(!locked, |ui| {
                            ui.horizontal(|ui| {
                                for (kind, name) in [
                                    (ModelKind::E02w, "E02W（WiFi 版）"),
                                    (ModelKind::E02e, "E02E（eSIM 版）"),
                                ] {
                                    if tab(ui, app.model == kind, name) && app.model != kind {
                                        app.config.model = Some(kind);
                                        app.model = kind;
                                        app.ship.sn_preview.clear();
                                        app.ship.cred_preview.clear();
                                        app.sync_ship_steps_for_model();
                                        app.reload_fourg_template_buf();
                                        changed = true;
                                    }
                                }
                            });
                        });
                        ui.add_space(2.0);
                        caption(
                            ui,
                            if app.model.has_wifi() {
                                "WiFi 版：含无线驱动与速率测试；绑定蓝牙 MAC 与 WiFi MAC；SN 示例 6472W1000016"
                            } else {
                                "eSIM 版：无 WiFi 模组，测陀螺仪与 IMEI/CCID，支持 4G 参数写入；SN 示例 6472E1000012"
                            },
                        );
                    });

                    ui.add_space(12.0);

                    // —— 2. 工位选择 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        let badge = if app.config.is_ship_station() {
                            "写号与出厂检验".to_string()
                        } else {
                            format!("启用 {} 项", app.batch_items().len())
                        };
                        card_head(ui, "工位选择", Some(badge));
                        ui.add_space(4.0);
                        let mut station_opts: Vec<String> = Vec::new();
                        for builtin in ["module", "complete"] {
                            if app.config.profiles.contains_key(builtin) {
                                station_opts.push(builtin.to_string());
                            }
                        }
                        for k in app.config.profiles.keys() {
                            if k != "module" && k != "complete" {
                                station_opts.push(k.clone());
                            }
                        }
                        station_opts.push(Config::STATION_SHIP.to_string());

                        let station_before = app.config.station.clone();
                        ui.add_enabled_ui(!locked, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                for s in &station_opts {
                                    let label = Config::station_label(s);
                                    if tab(ui, *s == station_before, label) && s != &station_before
                                    {
                                        app.config.station = s.clone();
                                        app.refresh_stats();
                                        app.refresh_ship_stats();
                                        if app.config.is_ship_station() {
                                            app.ship.stock = crate::stock::ProvisionStock::load(
                                                &crate::stock::stock_path(),
                                            );
                                            app.sync_ship_steps_for_model();
                                            app.reload_fourg_template_buf();
                                        }
                                    }
                                }
                            });
                        });
                        changed |= app.config.station != station_before;
                        ui.add_space(2.0);
                        caption(
                            ui,
                            match app.config.station.as_str() {
                                "module" => "模组工位：半成品板级测试，报告与整机工位独立保存",
                                "complete" => "整机工位：成品全项功能测试，全项合格自动记入记录供写号工位核验",
                                Config::STATION_SHIP => "出厂写号：扫码写入 SN/凭据、核对工单版本与电量、切换出厂模式并导出出货表",
                                _ => "自定义测试工位",
                            },
                        );
                    });

                    ui.add_space(12.0);

                    if app.config.is_ship_station() {
                        show_ship_settings_cards(ui, app, &mut changed);
                    } else {
                        show_test_settings_cards(ui, app, &mut changed);
                    }

                    ui.add_space(12.0);
                    show_ledger_card(ui, app);
                    ui.add_space(4.0);
                });
        });

    app.settings_open = open;
    if changed {
        let saved = app.config.to_toml().and_then(|t| {
            std::fs::write(config::default_path(), t).map_err(|e| anyhow::anyhow!("写入失败: {e}"))
        });
        if let Err(e) = saved {
            app.push_log(format!("配置保存失败: {e:#}"));
        }
    }
}

/// 测试工位配置卡组。
fn show_test_settings_cards(ui: &mut egui::Ui, app: &mut E02App, changed: &mut bool) {
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(ui, "测试项", None);
        ui.add_space(2.0);
        caption(ui, "勾选写入当前工位；设备信息与蓝牙 MAC 为身份来源，必测");
        ui.add_space(4.0);
        if let Some(profile) = app.config.profiles.get_mut(&app.config.station) {
            egui::Grid::new("items_grid")
                .num_columns(3)
                .spacing([24.0, 8.0])
                .show(ui, |ui| {
                    let avail: Vec<&TestItem> = TestItem::ALL
                        .iter()
                        .filter(|i| i.available_for(app.model))
                        .collect();
                    for (idx, item) in avail.iter().enumerate() {
                        let fixed = matches!(**item, TestItem::DeviceInfo | TestItem::BtMac);
                        let slug = item.slug().to_string();
                        let mut on = profile.items.contains(&slug);
                        if fixed {
                            on = true;
                            ui.add_enabled(false, egui::Checkbox::new(&mut on, item.label()));
                        } else if ui.checkbox(&mut on, item.label()).changed() {
                            if on {
                                profile.items.push(slug);
                            } else {
                                profile.items.retain(|s| s != &slug);
                            }
                            *changed = true;
                        }
                        if idx % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
        }
    });

    if app.model.has_wifi() {
        ui.add_space(12.0);
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
            *changed |= app.config.wifi.depth != depth_before;
            ui.add_space(2.0);
            caption(
                ui,
                if app.config.wifi.depth == WifiDepth::Throughput {
                    "连上 AP 后用 iperf3 实测吞吐并按阈值判定"
                } else {
                    "只验证无线驱动初始化，不测速率"
                },
            );
        });

        if app.config.wifi.depth == WifiDepth::Throughput {
            ui.add_space(12.0);
            theme::card().show(ui, |ui| {
                card_begin(ui);
                card_head(ui, "吞吐测试参数", None);
                ui.add_space(4.0);
                caption(
                    ui,
                    "前提：耳机与这台电脑连接同一个 AP（同一网段），否则测不通或速率偏低",
                );
                ui.add_space(6.0);
                let t = &mut app.config.wifi.throughput;
                egui::Grid::new("wifi_thr")
                    .num_columns(3)
                    .spacing([16.0, 8.0])
                    .min_col_width(58.0)
                    .show(ui, |ui| {
                        ui.label("测试方向");
                        ui.allocate_ui([56.0, 30.0].into(), |ui| {
                            if tab(ui, t.direction == IperfDir::Up, "只测上行") {
                                t.direction = IperfDir::Up;
                                *changed = true;
                            }
                            if tab(ui, t.direction == IperfDir::Down, "只测下行") {
                                t.direction = IperfDir::Down;
                                *changed = true;
                            }
                            if tab(ui, t.direction == IperfDir::Both, "双向") {
                                t.direction = IperfDir::Both;
                                *changed = true;
                            }
                        });
                        caption(ui, "双向=上、下行各测一次");
                        ui.end_row();

                        ui.label("每次时长");
                        *changed |= ui
                            .add(
                                egui::DragValue::new(&mut t.duration_secs)
                                    .suffix("s")
                                    .range(2..=30),
                            )
                            .changed();
                        caption(ui, "每个方向单次测试时长");
                        ui.end_row();

                        ui.label("上行阈值");
                        *changed |= ui
                            .add(
                                egui::DragValue::new(&mut t.min_mbps_up)
                                    .suffix("Mbps")
                                    .range(0.0..=300.0),
                            )
                            .changed();
                        caption(ui, "耳机发往电脑，低于此值判不合格；0 不判");
                        ui.end_row();

                        ui.label("下行阈值");
                        *changed |= ui
                            .add(
                                egui::DragValue::new(&mut t.min_mbps_down)
                                    .suffix("Mbps")
                                    .range(0.0..=300.0),
                            )
                            .changed();
                        caption(ui, "电脑发往耳机，低于此值判不合格；0 不判");
                        ui.end_row();

                        ui.label("重传上限");
                        *changed |= ui
                            .add(egui::DragValue::new(&mut t.max_retr).range(0..=10000))
                            .changed();
                        caption(ui, "单方向 TCP 重传总数超过判不合格");
                        ui.end_row();

                        ui.label("复测");
                        *changed |= ui.checkbox(&mut t.retest, "自动复测一次").changed();
                        caption(ui, "边缘不达标或偶发异常时自动重测一次");
                        ui.end_row();

                        ui.label("端口");
                        *changed |= ui
                            .add(egui::DragValue::new(&mut t.server_port).range(0..=65535))
                            .changed();
                        caption(ui, "电脑侧 iperf3 端口；0=自动");
                        ui.end_row();
                    });
                ui.add_space(2.0);
                caption(
                    ui,
                    "2.4G 单天线理论 72Mbps；阈值建议用金机在产线 AP 上实测校准",
                );
            });

            ui.add_space(12.0);
            theme::card().show(ui, |ui| {
                card_begin(ui);
                card_head(ui, "产线配网", None);
                ui.add_space(4.0);
                egui::Grid::new("wifi_join")
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("产线 SSID");
                        *changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut app.config.wifi.ssid)
                                    .hint_text("留空 = 用耳机当前网络")
                                    .desired_width(190.0),
                            )
                            .changed();
                        ui.end_row();
                        ui.label("密码");
                        *changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut app.config.wifi.psk)
                                    .hint_text("开放网络留空")
                                    .desired_width(190.0),
                            )
                            .changed();
                        ui.end_row();
                    });
                ui.add_space(2.0);
                caption(
                    ui,
                    "填了即每次吞吐前推送重连，保证测的是指定 AP；留空则用耳机当前网络",
                );
            });
        }
    }

    ui.add_space(12.0);

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
                    *changed = true;
                }
            }
        });
        ui.add_space(4.0);
        caption(ui, "固件回读格式 key 4/5/6，收齐必测键自动通过");
    });

    ui.add_space(12.0);

    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(ui, "超时", None);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("单项命令");
            *changed |= ui
                .add(
                    egui::DragValue::new(&mut app.config.timeouts.item_secs)
                        .suffix("s")
                        .range(0..=600),
                )
                .changed();
            ui.separator();
            ui.label("人工应答");
            *changed |= ui
                .add(
                    egui::DragValue::new(&mut app.config.timeouts.manual_secs)
                        .suffix("s")
                        .range(0..=600),
                )
                .changed();
        });
        ui.add_space(2.0);
        caption(ui, "0 = 不限制；超时判不合格并继续下一项");
    });

    ui.add_space(12.0);

    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(ui, "连接", None);
        ui.add_space(4.0);
        *changed |= ui
            .checkbox(&mut app.config.conn.auto_start, "插入设备后自动开始全测")
            .changed();
        ui.add_space(2.0);
        caption(ui, "检测到 USB 设备上线即开始；测试中拔出立即中止");
    });
}

/// 出厂写号工位配置卡组。
fn show_ship_settings_cards(ui: &mut egui::Ui, app: &mut E02App, changed: &mut bool) {
    let model = app.model;

    // —— 1. 工单与检测要求 ——
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(
            ui,
            "工单与检测要求",
            Some(format!("{} 参数", model.label())),
        );
        ui.add_space(4.0);
        caption(
            ui,
            "写入前须核对 T32 与蓝牙固件版本完全一致，未填写工单版本时禁止写入。",
        );
        ui.add_space(6.0);

        let mut reboot_secs = app.config.provision.reboot_verify_secs;
        let mut cfg_path = app.config.provision.config_path.clone();
        let p = app.config.provision.for_model_mut(model);

        egui::Grid::new("ship_order_grid")
            .num_columns(3)
            .spacing([14.0, 8.0])
            .min_col_width(88.0)
            .show(ui, |ui| {
                ui.label("T32 软件版本");
                *changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut p.t32_ver)
                            .hint_text("如 V2.2.9（必填）")
                            .desired_width(185.0)
                            .font(egui::TextStyle::Monospace),
                    )
                    .changed();
                caption(ui, "核对 cf3 soft-ware");
                ui.end_row();

                ui.label("蓝牙固件版本");
                *changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut p.bt_ver)
                            .hint_text("如 V1_7_4（必填）")
                            .desired_width(185.0)
                            .font(egui::TextStyle::Monospace),
                    )
                    .changed();
                caption(ui, "核对 cf7 蓝牙版本");
                ui.end_row();

                ui.label("最低电池电压");
                *changed |= ui
                    .add(
                        egui::DragValue::new(&mut p.min_batt_volts)
                            .suffix("V")
                            .range(3.0..=4.5)
                            .speed(0.05),
                    )
                    .changed();
                caption(ui, "低于此值禁止写入，防止写到一半掉电");
                ui.end_row();

                ui.label("SD 卡检测");
                *changed |= ui
                    .checkbox(&mut p.check_sdcard, "写入前检查 SD 卡在位")
                    .changed();
                caption(ui, "cf4 切换出厂模式需清理 U 盘");
                ui.end_row();

                ui.label("重启回读等待");
                if ui
                    .add(
                        egui::DragValue::new(&mut reboot_secs)
                            .suffix("s")
                            .range(0..=600),
                    )
                    .changed()
                {
                    *changed = true;
                }
                caption(ui, "0 = 跳过（正式版无 adb 时设为 0）");
                ui.end_row();

                ui.label("设备配置文件");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut cfg_path)
                            .desired_width(185.0)
                            .font(egui::TextStyle::Monospace),
                    )
                    .changed()
                {
                    *changed = true;
                }
                caption(ui, "设备端 ini 绝对路径");
                ui.end_row();
            });

        app.config.provision.reboot_verify_secs = reboot_secs;
        app.config.provision.config_path = cfg_path;
    });

    ui.add_space(12.0);

    // —— 2. 写入与校验规则 ——
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(ui, "写入与校验规则", None);
        ui.add_space(4.0);

        let mut gate_mode = app.config.provision.gate_mode;
        let mut auto_commit = app.config.provision.auto_commit;

        ui.horizontal(|ui| {
            ui.label("整机测试校验");
            for (m, label) in [
                (GateMode::Warn, "提示确认"),
                (GateMode::Strict, "强制拦截"),
                (GateMode::Off, "不校验"),
            ] {
                if tab(ui, gate_mode == m, label) && gate_mode != m {
                    gate_mode = m;
                    *changed = true;
                }
            }
        });
        caption(
            ui,
            match gate_mode {
                GateMode::Strict => "强制拦截：记录中无本机「整机工位全项合格」一律禁止写入",
                GateMode::Warn => {
                    "提示确认（默认）：未查到整机合格记录时暂停提示，人工核实后可继续"
                }
                GateMode::Off => "不校验：跳过整机测试记录检查",
            },
        );
        ui.add_space(4.0);
        if ui
            .checkbox(&mut auto_commit, "校验无异常时自动写入")
            .changed()
        {
            *changed = true;
        }
        caption(
            ui,
            "若触发同 SN 重写、覆盖已有凭据或未过整机校验，仍会暂停等待人工确认",
        );
        app.config.provision.gate_mode = gate_mode;
        app.config.provision.auto_commit = auto_commit;

        ui.add_space(8.0);
        let p = app.config.provision.for_model_mut(model);
        let mut sn_pat_changed = false;
        egui::Grid::new("ship_rule_grid")
            .num_columns(3)
            .spacing([14.0, 8.0])
            .min_col_width(88.0)
            .show(ui, |ui| {
                ui.label("SN 格式正则");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut p.sn_pattern)
                            .desired_width(185.0)
                            .font(egui::TextStyle::Monospace),
                    )
                    .changed()
                {
                    sn_pat_changed = true;
                    *changed = true;
                }
                caption(ui, &format!("示例 {}", model.sn_example()));
                ui.end_row();
            });
        if let Err(e) = regex_lite::Regex::new(&p.sn_pattern) {
            ui.label(
                egui::RichText::new(format!("SN 正则语法错误: {e}"))
                    .small()
                    .color(theme::BAD),
            );
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("写入项");
            let mut sn_on = true;
            ui.add_enabled(false, egui::Checkbox::new(&mut sn_on, "sn（必选）"));
            let mut has_uuid = p.keys.iter().any(|k| k == "uuid");
            let mut has_key = p.keys.iter().any(|k| k == "auth_key");
            let mut keys_changed = false;
            keys_changed |= ui.checkbox(&mut has_uuid, "uuid").changed();
            keys_changed |= ui.checkbox(&mut has_key, "auth_key").changed();
            if keys_changed {
                let mut next = vec!["sn".to_string()];
                if has_uuid {
                    next.push("uuid".to_string());
                }
                if has_key {
                    next.push("auth_key".to_string());
                }
                p.keys = next;
                *changed = true;
            }
        });

        let mut cred_pat_changed = false;
        if p.needs_credentials() {
            ui.add_space(4.0);
            caption(
                ui,
                "已启用凭据写入：写入前从凭据池自动分配，无可用凭据时禁止写入",
            );
            egui::Grid::new("ship_cred_pat_grid")
                .num_columns(3)
                .spacing([14.0, 8.0])
                .min_col_width(88.0)
                .show(ui, |ui| {
                    ui.label("uuid 正则");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut p.uuid_pattern)
                                .desired_width(185.0)
                                .font(egui::TextStyle::Monospace),
                        )
                        .changed()
                    {
                        cred_pat_changed = true;
                        *changed = true;
                    }
                    caption(ui, "导入凭据文件时校验");
                    ui.end_row();

                    ui.label("auth_key 正则");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut p.key_pattern)
                                .desired_width(185.0)
                                .font(egui::TextStyle::Monospace),
                        )
                        .changed()
                    {
                        cred_pat_changed = true;
                        *changed = true;
                    }
                    caption(ui, "导入凭据文件时校验");
                    ui.end_row();
                });
            if let Err(e) = regex_lite::Regex::new(&p.uuid_pattern) {
                ui.label(
                    egui::RichText::new(format!("uuid 正则错误: {e}"))
                        .small()
                        .color(theme::BAD),
                );
            }
            if let Err(e) = regex_lite::Regex::new(&p.key_pattern) {
                ui.label(
                    egui::RichText::new(format!("auth_key 正则错误: {e}"))
                        .small()
                        .color(theme::BAD),
                );
            }
        }
        if sn_pat_changed {
            app.ship.sn_preview.clear();
        }
        if cred_pat_changed {
            app.ship.cred_preview.clear();
        }
    });

    // —— 3. 4G 参数写入（仅 E02E 显示）——
    if !model.has_wifi() {
        ui.add_space(12.0);
        let mut reload_clicked = false;
        let mut save_tpl_path: Option<String> = None;

        theme::card().show(ui, |ui| {
            card_begin(ui);
            let fg_on = app
                .config
                .provision
                .e02e
                .fourg
                .as_ref()
                .is_some_and(|f| f.enable);
            card_head(
                ui,
                "4G 参数写入（E02E）",
                Some(if fg_on { "已启用" } else { "已关闭" }.into()),
            );
            ui.add_space(4.0);
            let f = app
                .config
                .provision
                .e02e
                .fourg
                .get_or_insert_with(FourGCfg::default);
            if ui
                .checkbox(&mut f.enable, "启用 4G 模组参数写入（经设备端 4g-test）")
                .changed()
            {
                *changed = true;
            }
            caption(
                ui,
                if f.enable {
                    "开启后若模板缺失或回读不一致，将判定不合格并停止切换出厂模式"
                } else {
                    "关闭时跳过 4G 参数步骤（4g-test 固件未就绪前保持关闭）"
                },
            );

            if f.enable {
                ui.add_space(6.0);
                egui::Grid::new("ship_4g_grid")
                    .num_columns(3)
                    .spacing([14.0, 8.0])
                    .min_col_width(88.0)
                    .show(ui, |ui| {
                        ui.label("设备串口");
                        *changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut f.port)
                                    .hint_text("/dev/ttyS2")
                                    .desired_width(185.0)
                                    .font(egui::TextStyle::Monospace),
                            )
                            .changed();
                        caption(ui, "留空用 4g-test 默认");
                        ui.end_row();

                        ui.label("波特率");
                        *changed |= ui
                            .add(
                                egui::DragValue::new(&mut f.baud)
                                    .speed(100)
                                    .range(0..=921600),
                            )
                            .changed();
                        caption(ui, "0 = 默认 115200");
                        ui.end_row();

                        ui.label("模板路径");
                        *changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut f.template)
                                    .hint_text("templates/e02e_4g.ini")
                                    .desired_width(185.0)
                                    .font(egui::TextStyle::Monospace),
                            )
                            .changed();
                        caption(ui, "相对于数据目录");
                        ui.end_row();
                    });

                let tpl_rel = f.template.trim().to_string();
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("模板内容（每行 key=value，支持 # 注释与 {{sn}} 占位）")
                        .small()
                        .color(theme::BODY),
                );
                if ui
                    .add(
                        egui::TextEdit::multiline(&mut app.ship.fourg_tpl_buf)
                            .desired_rows(5)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    )
                    .changed()
                {
                    app.ship.fourg_tpl_dirty = true;
                }

                match provision::parse_fourg_template(&app.ship.fourg_tpl_buf, "6472E1000012") {
                    Ok(lines) => {
                        ui.label(
                            egui::RichText::new(format!(
                                "语法正常，共 {} 项配置{}",
                                lines.len(),
                                if app.ship.fourg_tpl_dirty {
                                    "（未保存）"
                                } else {
                                    ""
                                }
                            ))
                            .small()
                            .color(theme::OK_STRONG),
                        );
                    }
                    Err(e) => {
                        ui.label(
                            egui::RichText::new(format!("模板语法错误：{e}"))
                                .small()
                                .color(theme::BAD),
                        );
                    }
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let can_save = !tpl_rel.is_empty()
                        && provision::parse_fourg_template(&app.ship.fourg_tpl_buf, "6472E1000012")
                            .is_ok();
                    if ui
                        .add_enabled(can_save, egui::Button::new("保存模板"))
                        .clicked()
                    {
                        save_tpl_path = Some(tpl_rel.clone());
                    }
                    if ui.button("重新读取").clicked() {
                        reload_clicked = true;
                    }
                });
            }
        });

        if reload_clicked {
            app.reload_fourg_template_buf();
        }
        if let Some(rel) = save_tpl_path {
            let full = crate::paths::data_dir().join(&rel);
            let res = full
                .parent()
                .map(std::fs::create_dir_all)
                .transpose()
                .and_then(|_| std::fs::write(&full, &app.ship.fourg_tpl_buf));
            match res {
                Ok(()) => {
                    app.ship.fourg_tpl_dirty = false;
                    app.push_log(format!("4G 模板已保存: {}", full.display()));
                }
                Err(e) => app.push_log(format!("4G 模板保存失败（{}）: {e}", full.display())),
            }
        }
    }

    ui.add_space(12.0);

    // —— 4. 命令超时 ——
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(ui, "命令超时", None);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("单条命令上限");
            *changed |= ui
                .add(
                    egui::DragValue::new(&mut app.config.timeouts.item_secs)
                        .suffix("s")
                        .range(0..=600),
                )
                .changed();
            caption(ui, "0 = 不限制；防止设备无响应导致卡死");
        });
    });
}

fn show_ledger_card(ui: &mut egui::Ui, app: &mut E02App) {
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(
            ui,
            "设备绑定记录",
            Some(format!("{} 台", app.ledger_view.len())),
        );
        ui.add_space(4.0);
        caption(
            ui,
            if app.model.has_wifi() {
                "记录蓝牙 MAC 与 WiFi MAC 对应关系、整机合格状态与出厂 SN"
            } else {
                "记录蓝牙 MAC 与 IMEI 对应关系、整机合格状态与出厂 SN"
            },
        );
        if !app.config.binding.enable {
            caption(ui, "绑定校验已关闭（config: binding.enable = false）");
        }
        ui.add_space(6.0);
        if app.ledger_view.is_empty() {
            caption(ui, "尚无记录；测试或写号完成后自动保存");
        } else {
            egui::Grid::new("ledger_grid")
                .num_columns(5)
                .spacing([14.0, 6.0])
                .show(ui, |ui| {
                    for rec in app.ledger_view.recent(5) {
                        ui.label(egui::RichText::new(&rec.bt_mac).monospace());
                        ui.label(egui::RichText::new(&rec.partner).monospace());
                        let tag = if !rec.shipped_sn.is_empty() {
                            format!("已写 {}", rec.shipped_sn)
                        } else if rec.complete_verdict == "PASS" {
                            "整机合格".to_string()
                        } else {
                            format!("{} 次", rec.tests)
                        };
                        ui.label(tag);
                        let verdict = if !rec.ship_verdict.is_empty() {
                            rec.ship_verdict.as_str()
                        } else if !rec.last_verdict.is_empty() {
                            rec.last_verdict.as_str()
                        } else {
                            "—"
                        };
                        ui.label(verdict);
                        if ui.button("删").clicked() {
                            app.ledger_view.remove(&rec.bt_mac);
                            save_ledger(app, format!("已删绑定记录 {}", rec.bt_mac));
                        }
                        ui.end_row();
                    }
                });
            ui.add_space(2.0);
            caption(ui, "最近 5 台；完整记录请导出 CSV 查看");
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("导出绑定记录 CSV").clicked() && !app.ledger_view.is_empty() {
                export_csv(app);
            }
            if app.ledger_clear_armed {
                if ui.add(theme::danger("确认清空？")).clicked() {
                    app.ledger_view.clear();
                    app.ledger_clear_armed = false;
                    save_ledger(app, "绑定记录已清空".into());
                }
                if ui.button("取消").clicked() {
                    app.ledger_clear_armed = false;
                }
            } else if ui.button("清空记录").clicked() {
                app.ledger_clear_armed = true;
            }
        });
    });
}
