//! 设置窗口（遵循 design-md/claude 规范 + 分级权限管理）：
//! - 普通模式（无需密码）：仅开放「产品型号」「工位选择」「工单版本（出厂写号）」及「查看/导出设备绑定记录」；
//! - 管理员模式（输入账号密码解锁）：开放测试项、WiFi/按键/超时/连接、写号检测与流程规则、4G 模板编辑、
//!   删除/清空绑定记录以及修改管理员密码；关闭设置窗口或点击「锁定配置」自动重新锁定。

use crate::app::E02App;
use crate::auth::{self, AuthRole};
use crate::config::{self, Config, FourGCfg, GateMode, IperfDir, ModelKind, WifiDepth};
use crate::engine::TestItem;
use crate::protocol::key_name;
use crate::provision;
use crate::ui::{panel_ship, theme};
use egui::{Color32, Rounding};

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
        (Color32::TRANSPARENT, theme::MUTED)
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
    let dir = crate::paths::bindings_export_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        let msg = format!("创建绑定记录导出目录失败: {e}");
        app.binding_export_note = msg.clone();
        app.push_log(msg);
        return;
    }
    let path = dir.join(format!(
        "绑定记录-{}.csv",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    match std::fs::write(&path, app.ledger_view.to_csv()) {
        Ok(()) => {
            app.last_binding_export_path = Some(path.clone());
            let msg = format!(
                "绑定记录已导出 {} 台：{}",
                app.ledger_view.len(),
                path.display()
            );
            app.binding_export_note = msg.clone();
            app.push_log(msg);
        }
        Err(e) => {
            let msg = format!("绑定记录导出失败: {e}");
            app.binding_export_note = msg.clone();
            app.push_log(msg);
        }
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

                    // —— 权限状态条与管理员登录/改密 ——
                    show_auth_bar(ui, app, &mut changed);
                    ui.add_space(12.0);

                    // —— 基础设置（普通模式开放）——
                    // 1. 产品型号
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
                                "WiFi 版：含无线驱动与速率测试（无陀螺仪与 eSIM）；绑定蓝牙 MAC 与 WiFi MAC；SN 示例 6472W1000016"
                            } else {
                                "eSIM 版：无 WiFi 模组，测陀螺仪与 IMEI/CCID，支持 4G 参数写入；SN 示例 6472E1000012"
                            },
                        );
                    });

                    ui.add_space(12.0);

                    // 2. 工位选择
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
                                "complete" => {
                                    "整机工位：成品全项功能测试，全项合格自动记入记录供写号工位核验"
                                }
                                Config::STATION_SHIP => {
                                    "出厂写号：扫码写入 SN/凭据、核对工单版本与电量、切换出厂模式并导出出货表"
                                }
                                _ => "自定义测试工位",
                            },
                        );
                    });

                    // 3. 出厂写号工位：工单版本号（基础开放，换批次常需修改）
                    if app.config.is_ship_station() {
                        ui.add_space(12.0);
                        show_ship_version_card(ui, app, &mut changed);
                    }

                    ui.add_space(12.0);

                    // —— 高级配置（需管理员账号密码解锁）——
                    if app.is_admin() {
                        if app.config.is_ship_station() {
                            show_ship_admin_cards(ui, app, &mut changed);
                        } else {
                            show_test_admin_cards(ui, app, &mut changed);
                        }
                    } else {
                        show_locked_summary_card(ui, app);
                    }

                    ui.add_space(12.0);
                    show_ledger_card(ui, app);
                    ui.add_space(4.0);
                });
        });

    app.settings_open = open;
    if !open {
        app.lock_admin();
    }
    if changed {
        let saved = app.config.to_toml().and_then(|t| {
            std::fs::write(config::default_path(), t).map_err(|e| anyhow::anyhow!("写入失败: {e}"))
        });
        if let Err(e) = saved {
            app.push_log(format!("配置保存失败: {e:#}"));
        }
    }
}

/// 顶部权限条 + 内联管理员登录卡 + 修改密码卡。
fn show_auth_bar(ui: &mut egui::Ui, app: &mut E02App, changed: &mut bool) {
    let role = app.auth_ui.role;
    egui::Frame::default()
        .fill(if role.is_some() {
            theme::OK_BG
        } else {
            theme::SOFT
        })
        .stroke(theme::hairline(theme::HAIRLINE))
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                if let Some(r) = role {
                    theme::dot(ui, theme::OK_STRONG);
                    let title = match r {
                        AuthRole::Admin => "管理员模式（已解锁全部配置）",
                        AuthRole::Master => "管理员模式（高级恢复权限，可重置 admin 密码）",
                    };
                    ui.label(
                        egui::RichText::new(title)
                            .small()
                            .strong()
                            .color(theme::OK_STRONG),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if ui.small_button("锁定配置").clicked() {
                            app.lock_admin();
                        }
                        if ui.small_button("修改密码").clicked() {
                            app.auth_ui.change_pwd_open = !app.auth_ui.change_pwd_open;
                            app.auth_ui.new_pwd.clear();
                            app.auth_ui.confirm_pwd.clear();
                            app.auth_ui.pwd_msg.clear();
                            app.auth_ui.pwd_is_err = false;
                        }
                    });
                } else {
                    theme::dot(ui, theme::MUTED);
                    ui.label(
                        egui::RichText::new(
                            "普通模式：仅开放型号、工位、工单版本与导出记录；高级配置已锁定",
                        )
                        .small()
                        .color(theme::INK),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        let btn_text = if app.auth_ui.login_open {
                            "收起"
                        } else {
                            "管理员解锁"
                        };
                        if ui.small_button(btn_text).clicked() {
                            app.auth_ui.login_open = !app.auth_ui.login_open;
                            app.auth_ui.login_err.clear();
                            if !app.auth_ui.login_open {
                                app.auth_ui.pass_input.clear();
                            }
                        }
                    });
                }
            });
        });

    // 内联管理员登录卡片
    if role.is_none() && app.auth_ui.login_open {
        ui.add_space(8.0);
        let mut submit = false;
        theme::card().show(ui, |ui| {
            card_begin(ui);
            card_head(ui, "管理员登录", None);
            ui.add_space(4.0);
            caption(
                ui,
                "输入管理员账号与密码以修改高级配置（关闭设置窗口后自动重新锁定）",
            );
            ui.add_space(6.0);
            egui::Grid::new("admin_login_grid")
                .num_columns(2)
                .spacing([14.0, 8.0])
                .min_col_width(64.0)
                .show(ui, |ui| {
                    ui.label("账号");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.auth_ui.user_input)
                            .hint_text("输入管理员账号")
                            .desired_width(210.0),
                    );
                    ui.end_row();

                    ui.label("密码");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut app.auth_ui.pass_input)
                            .password(true)
                            .hint_text("输入密码后按回车")
                            .desired_width(210.0),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    ui.end_row();
                });
            if !app.auth_ui.login_err.is_empty() {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(&app.auth_ui.login_err)
                        .small()
                        .color(theme::BAD),
                );
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(theme::primary("登录解锁")).clicked() {
                    submit = true;
                }
                if ui.button("取消").clicked() {
                    app.auth_ui.login_open = false;
                    app.auth_ui.pass_input.clear();
                    app.auth_ui.login_err.clear();
                }
            });
        });

        if submit {
            match auth::verify(
                &app.auth_ui.user_input,
                &app.auth_ui.pass_input,
                &app.config.auth.admin_hash,
            ) {
                Some(r) => {
                    app.auth_ui.role = Some(r);
                    app.auth_ui.login_open = false;
                    app.auth_ui.pass_input.clear();
                    app.auth_ui.login_err.clear();
                    app.push_log("设置页已解锁管理员权限");
                }
                None => {
                    app.auth_ui.login_err = "账号或密码错误，请重新输入".into();
                    app.auth_ui.pass_input.clear();
                }
            }
        }
    }

    // 内联修改 admin 密码卡片
    if role.is_some() && app.auth_ui.change_pwd_open {
        ui.add_space(8.0);
        let mut save_pwd = false;
        theme::card().show(ui, |ui| {
            card_begin(ui);
            card_head(ui, "修改管理员密码（admin）", None);
            ui.add_space(4.0);
            caption(ui, "修改后立即生效并以哈希形式保存到 config.toml，不存明文");
            ui.add_space(6.0);
            egui::Grid::new("admin_chpwd_grid")
                .num_columns(2)
                .spacing([14.0, 8.0])
                .min_col_width(72.0)
                .show(ui, |ui| {
                    ui.label("新密码");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.auth_ui.new_pwd)
                            .password(true)
                            .hint_text("至少 4 位字符")
                            .desired_width(210.0),
                    );
                    ui.end_row();

                    ui.label("确认新密码");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut app.auth_ui.confirm_pwd)
                            .password(true)
                            .hint_text("再次输入新密码")
                            .desired_width(210.0),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        save_pwd = true;
                    }
                    ui.end_row();
                });
            if !app.auth_ui.pwd_msg.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(&app.auth_ui.pwd_msg).small().color(
                    if app.auth_ui.pwd_is_err {
                        theme::BAD
                    } else {
                        theme::OK_STRONG
                    },
                ));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(theme::primary("保存新密码")).clicked() {
                    save_pwd = true;
                }
                if ui.button("取消").clicked() {
                    app.auth_ui.change_pwd_open = false;
                    app.auth_ui.new_pwd.clear();
                    app.auth_ui.confirm_pwd.clear();
                    app.auth_ui.pwd_msg.clear();
                }
            });
        });

        if save_pwd {
            match auth::make_admin_hash(&app.auth_ui.new_pwd, &app.auth_ui.confirm_pwd) {
                Ok(hash) => {
                    app.config.auth.admin_hash = hash;
                    *changed = true;
                    app.auth_ui.new_pwd.clear();
                    app.auth_ui.confirm_pwd.clear();
                    app.auth_ui.pwd_msg = "admin 密码已更新并保存".into();
                    app.auth_ui.pwd_is_err = false;
                    app.push_log("管理员（admin）密码已更新");
                }
                Err(e) => {
                    app.auth_ui.pwd_msg = e.into();
                    app.auth_ui.pwd_is_err = true;
                }
            }
        }
    }
}

/// 出厂写号基础卡片：工单版本号（普通模式开放修改）。
fn show_ship_version_card(ui: &mut egui::Ui, app: &mut E02App, changed: &mut bool) {
    let model = app.model;
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(ui, "工单版本", Some(format!("{} 参数", model.label())));
        ui.add_space(4.0);
        caption(
            ui,
            "写入前须核对 T32 与蓝牙固件版本一致（不区分大小写，V/v 均可），未填写工单版本时禁止写入。",
        );
        ui.add_space(6.0);

        let p = app.config.provision.for_model_mut(model);
        egui::Grid::new("ship_ver_grid")
            .num_columns(3)
            .spacing([14.0, 8.0])
            .min_col_width(88.0)
            .show(ui, |ui| {
                ui.label("T32 软件版本");
                *changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut p.t32_ver)
                            .hint_text("如 V2.4.3 / v2.4.3（必填）")
                            .desired_width(185.0)
                            .font(egui::TextStyle::Monospace),
                    )
                    .changed();
                caption(ui, "核对 cf3 soft-ware（不区分大小写）");
                ui.end_row();

                ui.label("蓝牙固件版本");
                *changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut p.bt_ver)
                            .hint_text("如 V3_9_2（必填）")
                            .desired_width(185.0)
                            .font(egui::TextStyle::Monospace),
                    )
                    .changed();
                caption(ui, "核对 cf7 蓝牙版本（不区分大小写）");
                ui.end_row();
            });
    });
}

/// 普通模式下的高级配置只读摘要卡（可一眼核对当前生效参数，点击按钮进入解锁）。
fn show_locked_summary_card(ui: &mut egui::Ui, app: &mut E02App) {
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(ui, "高级配置（管理员）", Some("已锁定".into()));
        ui.add_space(4.0);

        if app.config.is_ship_station() {
            let p = app.config.provision.for_model(app.model);
            let gate_txt = match app.config.provision.gate_mode {
                GateMode::Warn => "未过整机测试时弹窗提示",
                GateMode::Strict => "未过整机测试禁止写号",
                GateMode::Off => "不检查整机测试记录",
            };
            let commit_txt = if app.config.provision.auto_commit {
                "扫码正常自动写入"
            } else {
                "逐台人工点确认"
            };
            let keys_txt = if p.needs_credentials() {
                "SN + uuid + auth_key"
            } else {
                "仅 SN"
            };
            let sd_txt = if p.check_sdcard { "开启" } else { "关闭" };
            let mut summary = format!(
                "当前参数：最低电压 {:.2}V · SD 卡检测 {} · {} · {} · 写入内容 {} · 超时 {}s",
                p.min_batt_volts,
                sd_txt,
                gate_txt,
                commit_txt,
                keys_txt,
                app.config.timeouts.item_secs
            );
            if !app.model.has_wifi() {
                let fg_txt = if p.fourg.as_ref().is_some_and(|f| f.enable) {
                    "已启用"
                } else {
                    "已关闭"
                };
                summary.push_str(&format!(" · 4G 参数 {fg_txt}"));
            }
            caption(ui, &summary);
        } else {
            let wifi_txt = if app.model.has_wifi() {
                match app.config.active_wifi_depth() {
                    WifiDepth::Driver => " · WiFi 仅测驱动",
                    WifiDepth::Throughput => " · WiFi 驱动 + 速率",
                }
            } else {
                ""
            };
            let auto_txt = if app.config.conn.auto_start {
                "插机自动开测"
            } else {
                "手动点击开测"
            };
            let summary = format!(
                "当前参数：启用 {} 项测试{} · 必测按键 {} 个 · 命令超时 {}s · {}",
                app.batch_items().len(),
                wifi_txt,
                app.config.keys.required.len(),
                app.config.timeouts.item_secs,
                auto_txt
            );
            caption(ui, &summary);
        }

        ui.add_space(8.0);
        if ui.add(theme::secondary("输入管理员账号密码修改")).clicked() {
            app.auth_ui.login_open = true;
            app.auth_ui.login_err.clear();
        }
    });
}

/// 测试工位高级配置卡组（管理员解锁后显示）。
fn show_test_admin_cards(ui: &mut egui::Ui, app: &mut E02App, changed: &mut bool) {
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
        let cur_depth = app.config.active_wifi_depth();
        ui.add_space(12.0);
        theme::card().show(ui, |ui| {
            card_begin(ui);
            card_head(
                ui,
                "WiFi 测试深度",
                Some(Config::station_label(&app.config.station).to_string()),
            );
            ui.add_space(4.0);
            let mut next_depth = cur_depth;
            ui.horizontal(|ui| {
                if tab(ui, cur_depth == WifiDepth::Driver, "驱动") {
                    next_depth = WifiDepth::Driver;
                }
                if tab(ui, cur_depth == WifiDepth::Throughput, "驱动 + 速率") {
                    next_depth = WifiDepth::Throughput;
                }
            });
            if next_depth != cur_depth {
                app.config.set_active_wifi_depth(next_depth);
                *changed = true;
            }
            ui.add_space(2.0);
            caption(
                ui,
                if app.config.active_wifi_depth() == WifiDepth::Throughput {
                    "连上 AP 后用 iperf3 实测吞吐并按阈值判定（整机工位默认）"
                } else {
                    "只验证无线驱动初始化，不测速率（模组工位默认）"
                },
            );
        });

        if app.config.active_wifi_depth() == WifiDepth::Throughput {
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

/// 出厂写号工位高级配置卡组（管理员解锁后显示）。
fn show_ship_admin_cards(ui: &mut egui::Ui, app: &mut E02App, changed: &mut bool) {
    let model = app.model;

    // —— 1. 硬件检测与配置文件 ——
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(
            ui,
            "硬件检测与设备文件",
            Some(format!("{} 参数", model.label())),
        );
        ui.add_space(6.0);

        let mut cfg_path = app.config.provision.config_path.clone();
        let p = app.config.provision.for_model_mut(model);

        egui::Grid::new("ship_hw_grid")
            .num_columns(3)
            .spacing([14.0, 8.0])
            .min_col_width(88.0)
            .show(ui, |ui| {
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
                caption(ui, "默认不勾选；按需开启写号前检测");
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

        app.config.provision.config_path = cfg_path;
    });

    ui.add_space(12.0);

    // —— 2. 写号前检查与写入内容（用通俗产线语言说明每项作用）——
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(ui, "写号前检查与写入内容", None);
        ui.add_space(6.0);

        let mut gate_mode = app.config.provision.gate_mode;
        let mut auto_commit = app.config.provision.auto_commit;

        ui.label(
            egui::RichText::new("1. 写号前是否要求先通过「整机工位」测试")
                .strong()
                .color(theme::INK),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (m, label) in [
                (GateMode::Off, "不检查"),
                (GateMode::Warn, "未过时弹窗提示"),
                (GateMode::Strict, "未过则禁止写号"),
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
                GateMode::Off => {
                    "默认：不检查整机测试记录，直接进入写号流程（适合整机测试与出厂写号不在同一台电脑的产线）"
                }
                GateMode::Warn => {
                    "写号前查本机整机测试记录；查不到合格记录时弹窗提醒，人工核实后可继续"
                }
                GateMode::Strict => {
                    "严格管控：本机必须先在本软件「整机工位」全项测合格，否则直接禁止写号"
                }
            },
        );

        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("2. 扫码后是否自动写入")
                .strong()
                .color(theme::INK),
        );
        ui.add_space(2.0);
        if ui
            .checkbox(&mut auto_commit, "扫码后自动开始写入（免逐台点确认）")
            .changed()
        {
            *changed = true;
        }
        caption(
            ui,
            "勾选后：版本、电量、SN 等检查都正常时，扫完条码直接开始写入；如遇同号重写、未过整机测试或覆盖已有凭据，仍会暂停弹窗确认。取消勾选则每台都需要人工点确认。",
        );
        app.config.provision.gate_mode = gate_mode;
        app.config.provision.auto_commit = auto_commit;

        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("3. 写入内容")
                .strong()
                .color(theme::INK),
        );
        ui.add_space(2.0);
        let p = app.config.provision.for_model_mut(model);
        let mut write_cred = p.needs_credentials();
        if ui
            .checkbox(&mut write_cred, "除 SN 外，同时写入 uuid 和 auth_key")
            .changed()
        {
            p.keys = if write_cred {
                vec!["sn".to_string(), "uuid".to_string(), "auth_key".to_string()]
            } else {
                vec!["sn".to_string()]
            };
            *changed = true;
        }
        caption(
            ui,
            if write_cred {
                "已开启：写号时自动从「批次与库存」导入的凭据池中取出 uuid 与 auth_key 一并写入，无可用凭据时禁止写入"
            } else {
                "默认仅写入 SN；勾选后同步写入 uuid 和 auth_key"
            },
        );
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
    let is_admin = app.is_admin();
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
                "记录蓝牙 MAC 与 WiFi MAC 对应关系、整机合格状态与出厂 SN（导出至 exports/bindings/）"
            } else {
                "记录蓝牙 MAC 与 IMEI 对应关系、整机合格状态与出厂 SN（导出至 exports/bindings/）"
            },
        );
        if !app.config.binding.enable {
            caption(ui, "绑定校验已关闭（config: binding.enable = false）");
        }
        ui.add_space(6.0);
        if app.ledger_view.is_empty() {
            caption(ui, "尚无记录；测试或写号完成后自动保存");
        } else {
            let cols = if is_admin { 5 } else { 4 };
            egui::Grid::new("ledger_grid")
                .num_columns(cols)
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
                        if is_admin && ui.button("删").clicked() {
                            app.ledger_view.remove(&rec.bt_mac);
                            save_ledger(app, format!("已删绑定记录 {}", rec.bt_mac));
                        }
                        ui.end_row();
                    }
                });
            ui.add_space(2.0);
            caption(ui, "最近 5 台；完整记录请导出 CSV 查看");
        }
        if !app.binding_export_note.is_empty() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(&app.binding_export_note)
                    .small()
                    .color(theme::OK_STRONG),
            );
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !app.ledger_view.is_empty(),
                    egui::Button::new("导出绑定记录 CSV"),
                )
                .clicked()
            {
                export_csv(app);
            }
            if ui.button("打开文件位置").clicked() {
                if let Some(p) = &app.last_binding_export_path {
                    let _ = panel_ship::reveal_file(p);
                } else {
                    let _ = panel_ship::open_dir(&crate::paths::bindings_export_dir());
                }
            }
            if is_admin {
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
            }
        });
        if !is_admin {
            ui.add_space(2.0);
            caption(ui, "删除或清空绑定记录需先解锁管理员权限");
        }
    });
}
