//! 出厂写号工位三栏界面与批次库存窗口（遵循 design-md/claude 规范）：
//! - 左栏（240px `CANVAS`）：写号流程清单 + 次级按钮「批次与库存」；
//! - 中栏（`CANVAS` 暖米底）：徽章摘要条 → 奶油内容卡（扫码输入 + 唯一珊瑚主按钮）→
//!   写入确认卡（待确认时）/ 结果色块（完成后）/ 深色执行明细卡（`DARK` + `DARK_SOFT`），
//!   与底部深色日志区自然形成「暖米 → 奶油卡 → 暖黑明细 → 暖米边距 → 暖黑日志」节奏交替；
//! - 右栏（300px `CANVAS`）：被测设备字段（点击复制）+ 写号结论卡 + 当日统计 2×2 瓦片。
//!
//! 字体与布局规范：仅用圆点与色块表达状态（无特殊符号字形）；浅色面文字显式指定 `INK`；
//! 侧栏与滚动区水平行统一 `Align::Min`；圆角严格 8px 控件 / 12px 卡片窗口 / 999px 胶囊。

use crate::app::E02App;
use crate::config::GateMode;
use crate::devwatch::DevState;
use crate::provision::{ShipStep, StepVerdict};
use crate::report;
use crate::stock::{self, EntryStatus};
use crate::ui::theme;
use egui::{Color32, Rounding, Sense, Stroke};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;

// ————————————————————————————— 左栏：流程清单（240px CANVAS） —————————————————————————————

pub fn show_left(ui: &mut egui::Ui, app: &mut E02App) {
    ui.set_min_width(ui.available_width());
    ui.label(
        egui::RichText::new("写号流程")
            .strong()
            .size(16.0)
            .color(theme::INK),
    );
    ui.add_space(4.0);
    let check_txt = match app.config.provision.gate_mode {
        GateMode::Strict => "整机校验：强制拦截",
        GateMode::Warn => "整机校验：提示确认",
        GateMode::Off => "整机校验：不校验",
    };
    let commit_txt = if app.config.provision.auto_commit {
        "自动写入"
    } else {
        "逐台确认"
    };
    ui.label(
        egui::RichText::new(format!("{check_txt} · {commit_txt}"))
            .small()
            .color(theme::MUTED),
    );
    ui.add_space(8.0);

    // 规范 button-secondary：画布底 + 发丝描边 + 40px 高度，全屏唯一批次库存入口
    let stock_btn = theme::secondary("批次与库存").min_size(egui::vec2(ui.available_width(), 40.0));
    if ui
        .add_enabled(!app.ship.busy, stock_btn)
        .on_hover_text("导入 SN 批次或凭据文件、处理未完成占用条目、导出出货明细")
        .clicked()
    {
        app.ship.stock = stock::ProvisionStock::load(&stock::stock_path());
        app.ship.stock_open = true;
    }

    let profile = app.config.provision.for_model(app.model);
    let needs_cred = profile.needs_credentials();
    let fourg_on = profile.fourg.as_ref().is_some_and(|f| f.enable);
    let probe_on = app.config.provision.reboot_verify_secs > 0;
    let has_sn_pool = app.ship.stock.has_sn_pool();

    let visible_steps = ShipStep::steps_for(app.model);
    let rows: Vec<(ShipStep, StepVerdict, String, u64, bool, bool)> = visible_steps
        .iter()
        .map(|&step| {
            let found = app.ship.steps.iter().find(|r| r.step == step);
            let verdict = found.map(|r| r.verdict).unwrap_or(StepVerdict::Pending);
            let detail = found.map(|r| r.detail.clone()).unwrap_or_default();
            let secs = found.map(|r| r.secs).unwrap_or(0);
            let cfg_enabled = match step {
                ShipStep::Claim => needs_cred || has_sn_pool,
                ShipStep::FourG => fourg_on,
                ShipStep::Probe => probe_on,
                _ => true,
            };
            let running = app.ship.busy && app.ship.current == Some(step);
            (step, verdict, detail, secs, cfg_enabled, running)
        })
        .collect();

    ui.add_space(8.0);
    let max_h = (ui.clip_rect().bottom() - ui.cursor().min.y - 30.0).max(100.0);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(max_h)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            for (step, verdict, detail, secs, cfg_enabled, running) in &rows {
                step_row(ui, *step, *verdict, detail, *secs, *cfg_enabled, *running);
            }
        });

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(format!("共 {} 步 · {}", rows.len(), app.model.label()))
            .small()
            .color(theme::MUTED),
    );
}

fn step_verdict_color(v: StepVerdict) -> Color32 {
    match v {
        StepVerdict::Pending | StepVerdict::Skip => theme::MUTED,
        StepVerdict::Pass => theme::OK_STRONG,
        StepVerdict::Warn => theme::WARN,
        StepVerdict::Fail => theme::BAD,
    }
}

fn step_row(
    ui: &mut egui::Ui,
    step: ShipStep,
    verdict: StepVerdict,
    detail: &str,
    secs: u64,
    cfg_enabled: bool,
    running: bool,
) {
    let muted_idle = !cfg_enabled && verdict == StepVerdict::Pending && !running;
    let (dot_color, label_color, strong) = if muted_idle {
        (theme::HAIRLINE, theme::MUTED, false)
    } else if running {
        (theme::PRIMARY, theme::INK, true)
    } else {
        (step_verdict_color(verdict), theme::INK, false)
    };
    let (status_text, status_color) = if running {
        ("进行中", theme::PRIMARY_ACTIVE)
    } else if muted_idle {
        ("未启用", theme::MUTED)
    } else {
        (verdict.ui_label(), step_verdict_color(verdict))
    };

    let resp = egui::Frame::default()
        .fill(if running {
            theme::SELECTED
        } else {
            Color32::TRANSPARENT
        })
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(8.0, 5.0))
        .stroke(Stroke::NONE)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::dot(ui, dot_color);
                ui.add_space(7.0);
                let name = egui::RichText::new(step.label());
                let name = if strong { name.strong() } else { name };
                ui.label(name.color(label_color));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.label(
                        egui::RichText::new(status_text)
                            .small()
                            .strong()
                            .color(status_color),
                    );
                });
            });
        })
        .response;

    if !detail.is_empty() {
        let tip = if secs > 0 {
            format!("{detail}（{secs}s）")
        } else {
            detail.to_string()
        };
        resp.on_hover_text(tip);
    }
}

// ————————————————————————————— 中栏：暖米工作区（CANVAS → CARD → DARK） —————————————————————————————

pub fn show_center(ui: &mut egui::Ui, app: &mut E02App) {
    egui::Frame::default()
        .inner_margin(egui::Margin::symmetric(20.0, 14.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            top_status_strip(ui, app);
            ui.add_space(12.0);
            if !app.ship.note.is_empty() {
                note_bar(ui, app);
                ui.add_space(10.0);
            }
            scan_card(ui, app);
            ui.add_space(12.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if app.ship.confirm.is_some() {
                        confirm_card(ui, app);
                        ui.add_space(12.0);
                    } else if !app.ship.busy && app.ship.last_pass.is_some() {
                        verdict_banner(ui, app);
                        ui.add_space(12.0);
                    }
                    execution_card(ui, app);
                });
        });
}

/// 顶部状态胶囊条（规范 badge-pill：CARD 底 INK 字；异常用语义浅底深字）。
fn top_status_strip(ui: &mut egui::Ui, app: &mut E02App) {
    let profile = app.config.provision.for_model(app.model).clone();
    let needs_cred = profile.needs_credentials();
    let has_sn_pool = app.ship.stock.has_sn_pool();
    let avail_sns = app.ship.stock.available_sns();
    let avail_creds = app.ship.stock.available_credentials();
    let has_mismatch = app.ship.stock.mismatch_warning(true).is_some();

    ui.horizontal_wrapped(|ui| {
        if profile.t32_ver.trim().is_empty() || profile.bt_ver.trim().is_empty() {
            theme::badge(ui, "未配置工单版本", theme::BAD, theme::BAD_BG);
        } else {
            theme::badge(
                ui,
                &format!(
                    "工单版本 {} / {}",
                    profile.t32_ver.trim(),
                    profile.bt_ver.trim()
                ),
                theme::INK,
                theme::CARD,
            );
        }

        let sn_text = if has_sn_pool {
            format!("SN 余量 {avail_sns}")
        } else {
            "未导入 SN 批次".to_string()
        };
        theme::badge(
            ui,
            &sn_text,
            if has_sn_pool {
                theme::INK
            } else {
                theme::MUTED
            },
            theme::CARD,
        );

        if needs_cred {
            if avail_creds == 0 {
                theme::badge(ui, "凭据余量 0", theme::BAD, theme::BAD_BG);
            } else {
                theme::badge(
                    ui,
                    &format!("凭据余量 {avail_creds}"),
                    theme::INK,
                    theme::CARD,
                );
            }
            if has_mismatch {
                theme::badge(ui, "凭据少于 SN", theme::WARN, theme::SOFT);
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if ui.add(theme::secondary("导出出货明细")).clicked() {
                app.export_ship_csv();
            }
        });
    });
}

fn note_bar(ui: &mut egui::Ui, app: &mut E02App) {
    let mut dismiss = false;
    egui::Frame::default()
        .fill(theme::SOFT)
        .stroke(theme::hairline(theme::HAIRLINE))
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::dot(ui, theme::PRIMARY);
                ui.label(
                    egui::RichText::new(&app.ship.note)
                        .small()
                        .color(theme::INK),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.small_button("关闭").clicked() {
                        dismiss = true;
                    }
                });
            });
        });
    if dismiss {
        app.ship.note.clear();
    }
}

/// 扫码内容卡（规范 feature-card：CARD 色块、12px 圆角、无描边无阴影；
/// 内嵌 canvas-on-card text-input 40px + 唯一珊瑚 primary 按钮 40px）。
fn scan_card(ui: &mut egui::Ui, app: &mut E02App) {
    let mut trigger = false;
    let mut open_settings = false;
    let mut open_stock = false;
    let profile = app.config.provision.for_model(app.model).clone();
    let missing_ver = profile.t32_ver.trim().is_empty() || profile.bt_ver.trim().is_empty();
    let missing_cred = profile.needs_credentials() && app.ship.stock.available_credentials() == 0;

    theme::card().show(ui, |ui| {
        card_begin(ui);
        ui.horizontal(|ui| {
            theme::card_title(ui, "扫码写号");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                ui.label(
                    egui::RichText::new(format!("示例 {}", app.model.sn_example()))
                        .small()
                        .monospace()
                        .color(theme::MUTED),
                );
            });
        });
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            let input_w = (ui.available_width() - 120.0).max(160.0);
            let edit = egui::TextEdit::singleline(&mut app.ship.scan_input)
                .hint_text(if app.ship.busy {
                    "正在写入，请稍候..."
                } else {
                    "扫描机身或包装 SN 条码，回车开始写入"
                })
                .font(egui::FontId::monospace(18.0))
                .desired_width(input_w)
                .min_size(egui::vec2(0.0, 40.0));
            let resp = ui.add_enabled(!app.ship.busy, edit);

            if !app.ship.busy {
                let has_newline =
                    app.ship.scan_input.contains('\n') || app.ship.scan_input.contains('\r');
                if has_newline {
                    app.ship.scan_input.retain(|c| c != '\n' && c != '\r');
                    trigger = true;
                } else if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    trigger = true;
                }
                let modal_open = app.settings_open || app.report_open || app.ship.stock_open;
                if !modal_open && (!ui.ctx().wants_keyboard_input() || resp.has_focus()) {
                    resp.request_focus();
                }
            }

            let can_start =
                !app.ship.busy && !app.ship.scan_input.trim().is_empty() && app.device_online();
            let btn = theme::primary("开始写号").min_size(egui::vec2(108.0, 40.0));
            if ui.add_enabled(can_start, btn).clicked() {
                trigger = true;
            }
        });

        // 就绪提示行：保持克制，用行内珊瑚文字链接（text-link）跳转设置/库存
        if !app.ship.busy {
            if !app.device_online() {
                ui.add_space(6.0);
                let hint = match &app.dev {
                    DevState::Disconnected => "未检测到设备，请插入单台耳机 USB 线",
                    DevState::Multiple => "检测到多台设备，请只保留一台被测耳机",
                    DevState::Online(_) => "",
                };
                ui.label(egui::RichText::new(hint).small().color(theme::MUTED));
            }
            if missing_ver {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "当前型号 {} 尚未填写工单软件/蓝牙版本",
                            app.model.label()
                        ))
                        .small()
                        .color(theme::BAD),
                    );
                    if ui.add(theme::text_link("前往设置")).clicked() {
                        open_settings = true;
                    }
                });
            }
            if missing_cred {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("已启用凭据写入，但当前无可用凭据")
                            .small()
                            .color(theme::BAD),
                    );
                    if ui.add(theme::text_link("导入凭据")).clicked() {
                        open_stock = true;
                    }
                });
            }
        }
    });

    if open_settings {
        app.ledger_view = crate::ledger::Ledger::load(&crate::ledger::path_of(&app.config));
        app.ledger_clear_armed = false;
        app.reload_fourg_template_buf();
        app.settings_open = true;
    }
    if open_stock {
        app.ship.stock = stock::ProvisionStock::load(&stock::stock_path());
        app.ship.stock_open = true;
    }

    if trigger {
        let trimmed = app.ship.scan_input.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        if !app.device_online() {
            app.ship.note = match &app.dev {
                DevState::Disconnected => {
                    "设备未连接：已保留刚扫入的 SN，插好 USB 后按回车即可写入".to_string()
                }
                DevState::Multiple => {
                    "检测到多台设备：请拔除多余设备，仅留一台后按回车写入".to_string()
                }
                DevState::Online(_) => unreachable!(),
            };
            app.push_log(app.ship.note.clone());
            return;
        }
        app.ship.scan_input.clear();
        app.start_ship(&trimmed);
    }
}

/// 写入确认卡（规范 model-comparison-card：CANVAS 底 + 1px HAIRLINE + 12px 圆角 + 20px 内边距；
/// 仅接受鼠标左键点击确认，防止扫码枪多余回车误触发）。
fn confirm_card(ui: &mut egui::Ui, app: &mut E02App) {
    let Some(confirm) = app.ship.confirm.as_ref() else {
        return;
    };
    let card = confirm.card.clone();
    let mut answer: Option<bool> = None;

    egui::Frame::default()
        .fill(theme::CANVAS)
        .stroke(theme::hairline(theme::HAIRLINE))
        .rounding(Rounding::same(12.0))
        .inner_margin(egui::Margin::same(20.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::dot(ui, theme::WARN);
                theme::card_title(ui, "写入确认");
                if card.re_run {
                    theme::badge(ui, "同 SN 重写", theme::INK, theme::SELECTED);
                }
            });
            ui.add_space(4.0);
            caption(ui, "请核对以下写入信息，确认无误后点击「确认写入」");
            ui.add_space(10.0);

            egui::Frame::default()
                .fill(theme::SOFT)
                .rounding(Rounding::same(8.0))
                .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    egui::Grid::new("ship_confirm_grid")
                        .num_columns(2)
                        .spacing([18.0, 6.0])
                        .show(ui, |ui| {
                            kv_row(ui, "目标 SN", &card.sn, true);
                            kv_row(ui, "蓝牙 MAC", &card.bt_mac, false);
                            kv_row(ui, "产品型号", &card.model, false);
                            if let Some((uuid, key)) = &card.credential {
                                kv_row(ui, "分配 uuid", uuid, false);
                                kv_row(ui, "分配 auth_key", key, false);
                            }
                            for (k, v) in &card.current {
                                let label = format!("设备当前 {k}");
                                if v.is_empty() {
                                    ui.label(
                                        egui::RichText::new(label).small().color(theme::MUTED),
                                    );
                                    ui.label(
                                        egui::RichText::new("（空）").small().color(theme::MUTED),
                                    );
                                    ui.end_row();
                                } else {
                                    kv_row(ui, &label, v, false);
                                }
                            }
                        });
                });

            if !card.warnings.is_empty() {
                ui.add_space(10.0);
                for w in &card.warnings {
                    egui::Frame::default()
                        .fill(theme::BAD_BG)
                        .rounding(Rounding::same(8.0))
                        .inner_margin(egui::Margin::symmetric(12.0, 7.0))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                theme::dot(ui, theme::BAD);
                                ui.label(egui::RichText::new(w).color(theme::BAD).strong());
                            });
                        });
                    ui.add_space(4.0);
                }
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let ok_btn = egui::Button::new(
                    egui::RichText::new("确认写入")
                        .color(Color32::WHITE)
                        .strong()
                        .size(15.0),
                )
                .fill(theme::OK_STRONG)
                .stroke(Stroke::NONE)
                .min_size(egui::vec2(124.0, 40.0));
                let ok_resp = ui.add(ok_btn);
                ok_resp.surrender_focus();
                if ok_resp.clicked_by(egui::PointerButton::Primary) {
                    answer = Some(true);
                }

                let cancel_btn = theme::danger("取消").min_size(egui::vec2(92.0, 40.0));
                let cancel_resp = ui.add(cancel_btn);
                cancel_resp.surrender_focus();
                if cancel_resp.clicked_by(egui::PointerButton::Primary) {
                    answer = Some(false);
                }
            });
        });

    if let Some(yes) = answer {
        if let Some(c) = app.ship.confirm.take() {
            c.answer(yes);
        }
    }
}

fn kv_row(ui: &mut egui::Ui, label: &str, value: &str, emphasize: bool) {
    ui.label(egui::RichText::new(label).small().color(theme::MUTED));
    let mut rt = egui::RichText::new(value).monospace().color(theme::INK);
    if emphasize {
        rt = rt.strong().size(15.5);
    }
    ui.label(rt);
    ui.end_row();
}

/// 写号结论横幅（规范色块分层：OK_BG / BAD_BG、12px 圆角、克制标题字号）。
fn verdict_banner(ui: &mut egui::Ui, app: &E02App) {
    let Some(pass) = app.ship.last_pass else {
        return;
    };
    let (bg, fg, title, sub) = if pass {
        (
            theme::OK_BG,
            theme::OK_STRONG,
            format!("写号合格  {}", app.ship.device.target_sn),
            "回读一致并已切换出厂模式，可拔下设备继续下一台".to_string(),
        )
    } else {
        let reason = app
            .ship
            .steps
            .iter()
            .find(|r| r.verdict == StepVerdict::Fail)
            .map(|r| format!("{}：{}", r.step.label(), r.detail))
            .unwrap_or_else(|| "已中止或取消，详情见下方执行明细".to_string());
        (
            theme::BAD_BG,
            theme::BAD,
            format!("写号不合格  {}", app.ship.device.target_sn),
            format!("{reason}（未切换出厂模式，排查后可重扫同一 SN）"),
        )
    };

    egui::Frame::default()
        .fill(bg)
        .rounding(Rounding::same(12.0))
        .inner_margin(egui::Margin::symmetric(20.0, 14.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new(title).strong().size(20.0).color(fg));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(sub).small().color(theme::INK));
        });
}

/// 深色执行明细卡（规范 code-window-card：`DARK` 外框 + `DARK_SOFT` 内区 + 12px/8px 圆角，
/// 在暖米画布与奶油扫码卡下方形成 Claude 标志性的明暗节奏交替）。
fn execution_card(ui: &mut egui::Ui, app: &E02App) {
    let has_progress = app.ship.busy
        || app.ship.last_pass.is_some()
        || app
            .ship
            .steps
            .iter()
            .any(|s| s.verdict != StepVerdict::Pending);

    egui::Frame::default()
        .fill(theme::DARK)
        .rounding(Rounding::same(12.0))
        .inner_margin(egui::Margin::symmetric(20.0, 16.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("执行明细")
                        .strong()
                        .size(15.0)
                        .color(theme::ON_DARK),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if app.ship.busy {
                        let step_name = app.ship.current.map(|s| s.label()).unwrap_or("准备中");
                        let text = if app.ship.rebooting {
                            format!("正在执行：{step_name}（设备重启中，请勿拔线）")
                        } else if app.ship.confirm.is_some() {
                            "等待人工确认".to_string()
                        } else {
                            format!("正在执行：{step_name}（请勿拔线）")
                        };
                        ui.label(egui::RichText::new(text).small().color(theme::TEAL));
                        ui.spinner();
                    } else if let Some(pass) = app.ship.last_pass {
                        let (t, c) = if pass {
                            ("已完成", theme::OK)
                        } else {
                            ("已停止", theme::BAD)
                        };
                        ui.label(egui::RichText::new(t).small().color(c));
                    } else {
                        ui.label(
                            egui::RichText::new("等待扫码")
                                .small()
                                .color(theme::ON_DARK_SOFT),
                        );
                    }
                });
            });
            ui.add_space(10.0);

            egui::Frame::default()
                .fill(theme::DARK_SOFT)
                .rounding(Rounding::same(8.0))
                .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if !has_progress {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        for line in [
                            "1. 接入单台耳机，确认右上角显示设备在线。",
                            "2. 使用扫码枪扫描机身或包装 SN 条码（或手输后按回车）。",
                            "3. 自动核对设备信息、固件版本与电量，写入后回读校验并切换出厂模式。",
                            "4. 可直接将 SN 批次文件或凭据文件（.txt / .csv）拖入本窗口导入。",
                        ] {
                            ui.label(egui::RichText::new(line).small().color(theme::ON_DARK_SOFT));
                        }
                        return;
                    }

                    let visible = ShipStep::steps_for(app.model);
                    egui::Grid::new("ship_steps_detail_grid")
                        .num_columns(3)
                        .spacing([14.0, 6.0])
                        .min_col_width(52.0)
                        .show(ui, |ui| {
                            for step in visible {
                                let running = app.ship.busy && app.ship.current == Some(step);
                                let found = app.ship.steps.iter().find(|r| r.step == step);
                                let verdict =
                                    found.map(|r| r.verdict).unwrap_or(StepVerdict::Pending);
                                let detail = found.map(|r| r.detail.as_str()).unwrap_or("");
                                let secs = found.map(|r| r.secs).unwrap_or(0);

                                let (tag, color) = if running {
                                    ("进行中", theme::PRIMARY)
                                } else {
                                    let c = match verdict {
                                        StepVerdict::Pending | StepVerdict::Skip => {
                                            theme::ON_DARK_SOFT
                                        }
                                        StepVerdict::Pass => theme::OK,
                                        StepVerdict::Warn => theme::WARN,
                                        StepVerdict::Fail => theme::BAD,
                                    };
                                    (verdict.ui_label(), c)
                                };

                                ui.label(egui::RichText::new(tag).small().strong().color(color));
                                ui.label(
                                    egui::RichText::new(step.label())
                                        .small()
                                        .strong()
                                        .color(theme::ON_DARK),
                                );
                                let msg = if running {
                                    "执行中...".to_string()
                                } else if detail.is_empty() {
                                    "—".to_string()
                                } else if secs > 0 {
                                    format!("{detail}（{secs}s）")
                                } else {
                                    detail.to_string()
                                };
                                ui.label(
                                    egui::RichText::new(msg).small().color(theme::ON_DARK_SOFT),
                                );
                                ui.end_row();
                            }
                        });
                });
        });
}

// ————————————————————————————— 右栏：设备信息 + 结论 + 统计（300px CANVAS） —————————————————————————————

pub fn show_right(ui: &mut egui::Ui, app: &E02App) {
    ui.set_min_width(ui.available_width());
    let d = &app.ship.device;

    right_heading(ui, "被测设备");
    egui::Grid::new("ship_device_info")
        .num_columns(2)
        .spacing([10.0, 8.0])
        .show(ui, |ui| {
            copy_field(ui, "目标 SN", &d.target_sn, true);
            copy_field(ui, "设备原 SN", &d.orig_sn, false);
            copy_field(ui, "蓝牙 MAC", &d.bt_mac, false);
            if app.model.has_wifi() {
                copy_field(ui, "WiFi MAC", &d.wifi_mac, false);
            } else {
                copy_field(ui, "IMEI", &d.imei, false);
                copy_field(ui, "CCID", &d.ccid, false);
            }
            copy_field(ui, "T32 版本", &d.t32_ver, false);
            copy_field(ui, "蓝牙版本", &d.bt_ver, false);
            copy_field(ui, "硬件版本", &d.hardware_ver, false);
            let batt = d
                .battery_volts
                .map(|v| format!("{v:.3} V"))
                .unwrap_or_default();
            copy_field(ui, "电池电压", &batt, false);
        });

    ui.add_space(14.0);
    right_heading(ui, "写号结论");
    ship_result_card(ui, app);

    ui.add_space(14.0);
    right_heading(ui, "当日统计");
    if let Some(t) = &app.ship.today {
        ship_stats_card(ui, t);
    }
}

fn right_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .strong()
            .size(16.0)
            .color(theme::INK),
    );
    ui.add_space(8.0);
}

fn ship_result_card(ui: &mut egui::Ui, app: &E02App) {
    let (text, fg, bg) = if app.ship.confirm.is_some() {
        ("待人工确认".to_string(), theme::INK, theme::SELECTED)
    } else if app.ship.busy {
        let p = app
            .ship
            .progress
            .as_ref()
            .map(|p| format!("写入中 {}/{}", p.done, p.total))
            .unwrap_or_else(|| "写入中".to_string());
        (p, theme::INK, theme::SELECTED)
    } else {
        match app.ship.last_pass {
            Some(true) => ("写号合格".to_string(), theme::OK_STRONG, theme::OK_BG),
            Some(false) => ("写号不合格".to_string(), theme::BAD, theme::BAD_BG),
            None => ("待扫码".to_string(), theme::MUTED, theme::SOFT),
        }
    };

    egui::Frame::default()
        .fill(bg)
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(10.0, 9.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(text).strong().size(15.0).color(fg));
            });
        });
}

fn ship_stats_card(ui: &mut egui::Ui, t: &report::DayStats) {
    let pass_color = if t.pass > 0 {
        theme::OK_STRONG
    } else {
        theme::MUTED
    };
    let fail_color = if t.fail > 0 { theme::BAD } else { theme::MUTED };
    let (rate, rate_color) = match (t.pass * 100).checked_div(t.total) {
        Some(pct) if t.fail == 0 => (format!("{pct}%"), theme::OK_STRONG),
        Some(pct) => (format!("{pct}%"), theme::WARN),
        None => ("-".to_string(), theme::MUTED),
    };
    let w = (ui.available_width() - 10.0) / 2.0;
    ui.horizontal(|ui| {
        stat_tile(ui, w, &format!("{} 台", t.total), "已写", theme::INK);
        stat_tile(ui, w, &rate, "合格率", rate_color);
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        stat_tile(ui, w, &t.pass.to_string(), "合格", pass_color);
        stat_tile(ui, w, &t.fail.to_string(), "不合格", fail_color);
    });
}

fn stat_tile(ui: &mut egui::Ui, width: f32, value: &str, label: &str, color: Color32) {
    egui::Frame::default()
        .fill(theme::CARD)
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(10.0, 9.0))
        .show(ui, |ui| {
            ui.set_width(width - 20.0);
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(value).strong().size(17.0).color(color));
                ui.add_space(2.0);
                ui.label(egui::RichText::new(label).small().color(theme::MUTED));
            });
        });
}

fn copy_field(ui: &mut egui::Ui, name: &str, value: &str, strong: bool) {
    ui.label(egui::RichText::new(name).small().color(theme::MUTED));
    let empty = value.is_empty();
    let text = if empty { "-" } else { value };
    let mut rich =
        egui::RichText::new(text)
            .monospace()
            .color(if empty { theme::MUTED } else { theme::INK });
    if strong && !empty {
        rich = rich.strong();
    }
    let resp = ui.add(egui::Label::new(rich).sense(Sense::click()));
    let resp = if empty {
        resp
    } else {
        resp.on_hover_text("点击复制")
    };
    if resp.clicked() && !empty {
        ui.output_mut(|w| w.copied_text = value.to_string());
    }
    ui.end_row();
}

// ————————————————————————————— 弹窗：批次与库存 —————————————————————————————

fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme::MUTED));
}

fn card_begin(ui: &mut egui::Ui) {
    theme::card_inputs(ui);
    ui.set_min_width(ui.available_width());
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

pub fn show_stock_dialog(ctx: &egui::Context, app: &mut E02App) {
    if !app.ship.stock_open {
        return;
    }
    let mut open = true;
    let center = ctx.screen_rect().center();
    let needs_cred = app
        .config
        .provision
        .for_model(app.model)
        .needs_credentials();
    let busy = app.ship.busy || app.ship.file_picking;

    egui::Window::new("批次与库存")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .vscroll(false)
        .fixed_size([620.0, 650.0])
        .default_pos(center - egui::vec2(310.0, 325.0))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if !app.ship.note.is_empty() {
                        egui::Frame::default()
                            .fill(theme::SOFT)
                            .stroke(theme::hairline(theme::HAIRLINE))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(
                                    egui::RichText::new(&app.ship.note)
                                        .small()
                                        .color(theme::INK),
                                );
                            });
                        ui.add_space(12.0);
                    }

                    // —— 1. SN 批次池 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        let st = &app.ship.stock;
                        let badge = format!("未用 {} / 总计 {}", st.available_sns(), st.sns.len());
                        card_head(ui, "SN 批次池（可选）", Some(badge));
                        ui.add_space(4.0);
                        caption(
                            ui,
                            &format!(
                                "未用 {} · 占用中 {} · 已写入 {} · 已作废 {}（{} 示例：{}）",
                                st.count_sn_status(EntryStatus::Available),
                                st.count_sn_status(EntryStatus::Allocated),
                                st.count_sn_status(EntryStatus::Committed),
                                st.count_sn_status(EntryStatus::Voided),
                                app.model.label(),
                                app.model.sn_example()
                            ),
                        );
                        ui.add_space(8.0);
                        ui.add_enabled_ui(!busy, |ui| {
                            ui.horizontal(|ui| {
                                if ui.button("选择 SN 文件导入...").clicked() {
                                    spawn_file_picker(ctx, app, false);
                                }
                                if app.ship.clear_sn_armed {
                                    if ui.add(theme::danger("确认清空未用 SN？")).clicked() {
                                        app.ship.clear_sn_armed = false;
                                        clear_available(app, true);
                                    }
                                    if ui.button("取消").clicked() {
                                        app.ship.clear_sn_armed = false;
                                    }
                                } else if ui.button("清空未用").clicked() {
                                    app.ship.clear_sn_armed = true;
                                }
                            });
                            ui.add_space(4.0);
                            egui::CollapsingHeader::new(
                                egui::RichText::new("手动粘贴导入")
                                    .small()
                                    .color(theme::BODY),
                            )
                            .id_salt("paste_sn")
                            .show(ui, |ui| {
                                let resp = ui.add(
                                    egui::TextEdit::multiline(&mut app.ship.sn_text)
                                        .hint_text(format!(
                                            "一行一个 SN（如 {}），支持 # 注释",
                                            app.model.sn_example()
                                        ))
                                        .desired_rows(4)
                                        .desired_width(f32::INFINITY)
                                        .font(egui::TextStyle::Monospace),
                                );
                                if resp.changed() {
                                    app.ship.sn_preview = sn_preview_text(app);
                                }
                                if !app.ship.sn_preview.is_empty() {
                                    caption(ui, &app.ship.sn_preview);
                                }
                                if ui.button("导入").clicked() {
                                    import_sns_text(app, &app.ship.sn_text.clone(), "粘贴导入");
                                    if app.ship.note.contains("导入成功") {
                                        app.ship.sn_text.clear();
                                        app.ship.sn_preview.clear();
                                    }
                                }
                            });
                        });
                    });

                    // —— 2. 凭据池（仅启用凭据写入时展示） ——
                    if needs_cred {
                        ui.add_space(12.0);
                        theme::card().show(ui, |ui| {
                            card_begin(ui);
                            let st = &app.ship.stock;
                            let badge = format!(
                                "未用 {} / 总计 {}",
                                st.available_credentials(),
                                st.credentials.len()
                            );
                            card_head(ui, "凭据池（uuid / auth_key）", Some(badge));
                            ui.add_space(4.0);
                            caption(
                                ui,
                                &format!(
                                    "未用 {} · 占用中 {} · 已写入 {} · 已作废 {}",
                                    st.count_cred_status(EntryStatus::Available),
                                    st.count_cred_status(EntryStatus::Allocated),
                                    st.count_cred_status(EntryStatus::Committed),
                                    st.count_cred_status(EntryStatus::Voided),
                                ),
                            );
                            if let Some(warn) = st.mismatch_warning(true) {
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(warn).small().color(theme::BAD));
                            }
                            ui.add_space(8.0);
                            ui.add_enabled_ui(!busy, |ui| {
                                ui.horizontal(|ui| {
                                    if ui.button("选择凭据文件导入...").clicked() {
                                        spawn_file_picker(ctx, app, true);
                                    }
                                    if app.ship.clear_cred_armed {
                                        if ui.add(theme::danger("确认清空未用凭据？")).clicked()
                                        {
                                            app.ship.clear_cred_armed = false;
                                            clear_available(app, false);
                                        }
                                        if ui.button("取消").clicked() {
                                            app.ship.clear_cred_armed = false;
                                        }
                                    } else if ui.button("清空未用").clicked() {
                                        app.ship.clear_cred_armed = true;
                                    }
                                });
                                ui.add_space(4.0);
                                egui::CollapsingHeader::new(
                                    egui::RichText::new("手动粘贴导入")
                                        .small()
                                        .color(theme::BODY),
                                )
                                .id_salt("paste_cred")
                                .show(ui, |ui| {
                                    let resp = ui.add(
                                        egui::TextEdit::multiline(&mut app.ship.cred_text)
                                            .hint_text("一行一对：uuid,auth_key（支持逗号、Tab 或空格分隔）")
                                            .desired_rows(4)
                                            .desired_width(f32::INFINITY)
                                            .font(egui::TextStyle::Monospace),
                                    );
                                    if resp.changed() {
                                        app.ship.cred_preview = cred_preview_text(app);
                                    }
                                    if !app.ship.cred_preview.is_empty() {
                                        caption(ui, &app.ship.cred_preview);
                                    }
                                    if ui.button("导入").clicked() {
                                        import_creds_text(
                                            app,
                                            &app.ship.cred_text.clone(),
                                            "粘贴导入",
                                        );
                                        if app.ship.note.contains("导入成功") {
                                            app.ship.cred_text.clear();
                                            app.ship.cred_preview.clear();
                                        }
                                    }
                                });
                            });
                        });
                    }

                    ui.add_space(12.0);

                    // —— 3. 未完成写入与作废维护 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        let alloc_sns: Vec<(String, String)> = app
                            .ship
                            .stock
                            .allocated_sns()
                            .into_iter()
                            .map(|e| (e.sn.clone(), e.bound_bt_mac.clone()))
                            .collect();
                        let alloc_creds: Vec<(String, String, String)> = if needs_cred {
                            app.ship
                                .stock
                                .allocated_credentials()
                                .into_iter()
                                .map(|c| {
                                    (c.uuid.clone(), c.bound_sn.clone(), c.bound_bt_mac.clone())
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };
                        let pending_total = alloc_sns.len() + alloc_creds.len();
                        card_head(
                            ui,
                            "未完成写入与作废",
                            (pending_total > 0).then(|| format!("占用中 {pending_total}")),
                        );
                        ui.add_space(4.0);
                        caption(
                            ui,
                            "写入中途断开的条目会停留在「占用中」；确认未写入设备可释放回未用，报废主板请标记作废。",
                        );
                        ui.add_space(8.0);
                        ui.add_enabled_ui(!busy, |ui| {
                            if pending_total == 0 {
                                caption(ui, "当前无占用中的未完成条目");
                            } else {
                                egui::Grid::new("pending_alloc_grid")
                                    .num_columns(4)
                                    .spacing([12.0, 6.0])
                                    .show(ui, |ui| {
                                        for (sn, mac) in alloc_sns {
                                            ui.label(
                                                egui::RichText::new(format!("SN {sn}"))
                                                    .monospace()
                                                    .color(theme::INK),
                                            );
                                            caption(ui, &format!("设备 {mac}"));
                                            if ui.small_button("释放").clicked() {
                                                release_pending(app, true, &sn);
                                            }
                                            if ui.small_button("作废").clicked() {
                                                void_direct(app, true, &sn);
                                            }
                                            ui.end_row();
                                        }
                                        for (uuid, bsn, mac) in alloc_creds {
                                            ui.label(
                                                egui::RichText::new(&uuid)
                                                    .monospace()
                                                    .color(theme::INK),
                                            );
                                            caption(ui, &format!("SN {bsn} · {mac}"));
                                            if ui.small_button("释放").clicked() {
                                                release_pending(app, false, &uuid);
                                            }
                                            if ui.small_button("作废").clicked() {
                                                void_direct(app, false, &uuid);
                                            }
                                            ui.end_row();
                                        }
                                    });
                            }
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.label("手动作废");
                                ui.add(
                                    egui::TextEdit::singleline(&mut app.ship.void_input)
                                        .hint_text(if needs_cred {
                                            "输入 SN 或 uuid"
                                        } else {
                                            "输入 SN"
                                        })
                                        .desired_width(210.0)
                                        .font(egui::TextStyle::Monospace),
                                );
                                if app.ship.void_armed {
                                    if ui.add(theme::danger("确认作废？")).clicked() {
                                        app.ship.void_armed = false;
                                        do_void_input(app);
                                    }
                                    if ui.button("取消").clicked() {
                                        app.ship.void_armed = false;
                                    }
                                } else if ui.button("作废").clicked() {
                                    app.ship.void_armed = true;
                                }
                            });
                        });
                    });

                    ui.add_space(12.0);

                    // —— 4. 出货明细导出 ——
                    theme::card().show(ui, |ui| {
                        card_begin(ui);
                        card_head(ui, "出货明细导出", None);
                        ui.add_space(4.0);
                        if needs_cred {
                            ui.checkbox(
                                &mut app.ship.include_auth_key,
                                "导出时包含 auth_key 列（默认仅导出 uuid）",
                            );
                            ui.add_space(4.0);
                        }
                        ui.horizontal(|ui| {
                            if ui.button("导出出货明细 CSV").clicked() {
                                app.export_ship_csv();
                            }
                            if ui.button("打开数据目录").clicked() {
                                let _ = open_dir(crate::paths::data_dir());
                            }
                        });
                        ui.add_space(4.0);
                        caption(
                            ui,
                            "导出 UTF-8 BOM CSV 至数据目录（Excel 可直接打开），列按型号自动适配。",
                        );
                    });
                    ui.add_space(4.0);
                });
        });

    app.ship.stock_open = open;
}

// ————————————————————————————— 原生文件选择器与拖拽导入 —————————————————————————————

fn spawn_file_picker(ctx: &egui::Context, app: &mut E02App, is_cred: bool) {
    if app.ship.file_picking {
        return;
    }
    app.ship.file_picking = true;
    let title = if is_cred {
        "选择凭据文件（uuid / auth_key）"
    } else {
        "选择 SN 批次文件"
    };
    let (tx, rx) = mpsc::channel();
    app.ship.file_pick_rx = Some(rx);
    let egui_ctx = ctx.clone();
    thread::spawn(move || {
        let res = pick_file_blocking(title);
        let _ = tx.send((is_cred, res));
        egui_ctx.request_repaint();
    });
}

fn pick_file_blocking(title: &str) -> Result<Option<PathBuf>, String> {
    if cfg!(windows) {
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; \
             $f = New-Object System.Windows.Forms.OpenFileDialog; \
             $f.Title = '{}'; \
             $f.Filter = '文本与表格 (*.txt;*.csv)|*.txt;*.csv|所有文件 (*.*)|*.*'; \
             if ($f.ShowDialog() -eq 'OK') {{ \
                 [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
                 Write-Output $f.FileName \
             }}",
            title.replace('\'', "''")
        );
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-Sta", "-Command", &script]);
        crate::exec::hide_window(&mut cmd);
        let out = cmd
            .output()
            .map_err(|e| format!("打开文件对话框失败: {e}"))?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() {
            Ok(None)
        } else {
            Ok(Some(PathBuf::from(path)))
        }
    } else if cfg!(target_os = "macos") {
        let script = format!(
            "POSIX path of (choose file with prompt \"{}\")",
            title.replace('"', "\\\"")
        );
        let out = Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(|e| format!("打开文件选择失败: {e}"))?;
        if !out.status.success() {
            return Ok(None);
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((!path.is_empty()).then(|| PathBuf::from(path)))
    } else {
        let z = Command::new("zenity")
            .args([
                "--file-selection",
                &format!("--title={title}"),
                "--file-filter=文本/CSV (*.txt *.csv) | *.txt *.csv",
                "--file-filter=所有文件 | *",
            ])
            .output();
        match z {
            Ok(out) => {
                if !out.status.success() {
                    return Ok(None);
                }
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                Ok((!path.is_empty()).then(|| PathBuf::from(path)))
            }
            Err(_) => {
                let kd = Command::new("kdialog")
                    .args(["--getopenfilename", ".", "*.txt *.csv"])
                    .output()
                    .map_err(|_| {
                        "未找到系统文件对话框（zenity/kdialog），可将文件直接拖入窗口或展开下方手动粘贴"
                            .to_string()
                    })?;
                if !kd.status.success() {
                    return Ok(None);
                }
                let path = String::from_utf8_lossy(&kd.stdout).trim().to_string();
                Ok((!path.is_empty()).then(|| PathBuf::from(path)))
            }
        }
    }
}

pub fn import_picked_file(app: &mut E02App, is_cred: bool, path: &Path) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            let msg = format!("读取文件失败（{}）: {e}", path.display());
            app.ship.note = msg.clone();
            app.push_log(msg);
            return;
        }
    };
    let tag = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    if is_cred {
        import_creds_text(app, &text, &tag);
    } else {
        import_sns_text(app, &text, &tag);
    }
}

pub fn import_dropped_file(app: &mut E02App, path: &Path) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            let msg = format!("读取拖入文件失败（{}）: {e}", path.display());
            app.ship.note = msg.clone();
            app.push_log(msg);
            return;
        }
    };
    let first_data = text
        .lines()
        .map(|l| l.trim_start_matches('\u{FEFF}').trim())
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("");
    let looks_two_col = first_data.contains(',') || first_data.split_whitespace().count() == 2;
    let tag = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    if looks_two_col {
        if !app
            .config
            .provision
            .for_model(app.model)
            .needs_credentials()
        {
            let msg = format!(
                "文件 {tag} 为两列凭据格式，但当前型号未勾选 uuid/auth_key 写入项（请在设置中开启）"
            );
            app.ship.note = msg.clone();
            app.push_log(msg);
            return;
        }
        import_creds_text(app, &text, &tag);
    } else {
        import_sns_text(app, &text, &tag);
    }
}

pub fn open_dir(dir: &Path) -> bool {
    let _ = std::fs::create_dir_all(dir);
    if cfg!(windows) {
        Command::new("explorer").arg(dir).spawn().is_ok()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(dir).spawn().is_ok()
    } else {
        Command::new("xdg-open").arg(dir).spawn().is_ok()
    }
}

// ————————————————————————————— 库存增删改操作 —————————————————————————————

fn commit_stock(app: &mut E02App, st: stock::ProvisionStock, ok_msg: String) {
    let path = stock::stock_path();
    match st.save(&path) {
        Ok(()) => {
            app.push_log(format!("{ok_msg}（{}）", path.display()));
            app.ship.stock = st;
            app.ship.note = ok_msg;
        }
        Err(e) => {
            let msg = format!("库存保存失败: {e}");
            app.ship.note = msg.clone();
            app.push_log(msg);
        }
    }
}

fn now_stamp() -> String {
    report::time_str(std::time::SystemTime::now())
}

fn outcome_text(out: &stock::ImportOutcome) -> String {
    if !out.ok() {
        format!("校验未通过（未入库）：{}", out.errors.join("；"))
    } else if out.added > 0 || out.skipped_dup > 0 {
        format!(
            "校验通过：将新增 {}，跳过重复 {}",
            out.added, out.skipped_dup
        )
    } else {
        String::new()
    }
}

fn sn_preview_text(app: &E02App) -> String {
    if app.ship.sn_text.trim().is_empty() {
        return String::new();
    }
    let p = app.config.provision.for_model(app.model);
    outcome_text(&stock::preview_sn_import(
        &app.ship.stock,
        &app.ship.sn_text,
        &p.sn_pattern,
    ))
}

fn cred_preview_text(app: &E02App) -> String {
    if app.ship.cred_text.trim().is_empty() {
        return String::new();
    }
    let p = app.config.provision.for_model(app.model);
    outcome_text(&stock::preview_cred_import(
        &app.ship.stock,
        &app.ship.cred_text,
        &p.uuid_pattern,
        &p.key_pattern,
    ))
}

fn import_sns_text(app: &mut E02App, text: &str, source: &str) {
    let p = app.config.provision.for_model(app.model).clone();
    let mut st = app.ship.stock.clone();
    let out = st.import_sns(text, &p.sn_pattern, &now_stamp());
    if !out.ok() {
        let msg = format!(
            "SN 导入失败（{source}，库存未变）：{}",
            out.errors.join("；")
        );
        app.ship.note = msg.clone();
        app.push_log(msg);
        return;
    }
    commit_stock(
        app,
        st,
        format!(
            "SN 导入成功（{source}）：新增 {} 台，跳过重复 {} 台",
            out.added, out.skipped_dup
        ),
    );
}

fn import_creds_text(app: &mut E02App, text: &str, source: &str) {
    let p = app.config.provision.for_model(app.model).clone();
    let mut st = app.ship.stock.clone();
    let out = st.import_credentials(text, &p.uuid_pattern, &p.key_pattern, &now_stamp());
    if !out.ok() {
        let msg = format!(
            "凭据导入失败（{source}，库存未变）：{}",
            out.errors.join("；")
        );
        app.ship.note = msg.clone();
        app.push_log(msg);
        return;
    }
    commit_stock(
        app,
        st,
        format!(
            "凭据导入成功（{source}）：新增 {} 对，跳过重复 {} 对",
            out.added, out.skipped_dup
        ),
    );
}

fn release_pending(app: &mut E02App, is_sn: bool, key: &str) {
    let mut st = app.ship.stock.clone();
    let now = now_stamp();
    let ok = if is_sn {
        st.release_allocated_sn(key, &now)
    } else {
        st.release_allocated_credential(key, &now)
    };
    if ok {
        commit_stock(app, st, format!("已将 {key} 释放回未用"));
    } else {
        app.ship.note = format!("{key} 当前不是占用状态，无法释放");
    }
}

fn void_direct(app: &mut E02App, is_sn: bool, key: &str) {
    let mut st = app.ship.stock.clone();
    let now = now_stamp();
    let ok = if is_sn {
        st.void_sn(key, &now)
    } else {
        st.void_credential(key, &now)
    };
    if ok {
        commit_stock(app, st, format!("已作废 {key}"));
    }
}

fn do_void_input(app: &mut E02App) {
    let raw = app.ship.void_input.trim().to_string();
    if raw.is_empty() {
        app.ship.note = "请输入要作废的 SN 或 uuid".into();
        return;
    }
    let now = now_stamp();
    let mut st = app.ship.stock.clone();
    let sn_up = raw.to_uppercase();
    if st.sn_entry(&sn_up).is_some() {
        st.void_sn(&sn_up, &now);
        app.ship.void_input.clear();
        commit_stock(app, st, format!("已作废 SN {sn_up}"));
        return;
    }
    if let Some(uuid) = st
        .credentials
        .iter()
        .find(|c| c.uuid.eq_ignore_ascii_case(&raw))
        .map(|c| c.uuid.clone())
    {
        if st.void_credential(&uuid, &now) {
            app.ship.void_input.clear();
            commit_stock(app, st, format!("已作废凭据 {uuid}"));
        }
        return;
    }
    app.ship.note = format!("未找到 {raw}（SN 与 uuid 均无匹配）");
}

fn clear_available(app: &mut E02App, sns: bool) {
    let mut st = app.ship.stock.clone();
    let (n, what) = if sns {
        (st.clear_available_sns(), "未用 SN")
    } else {
        (st.clear_available_credentials(), "未用凭据")
    };
    commit_stock(app, st, format!("已清空 {what} {n} 条"));
}
