//! 出厂写号页（FQC §6，V3.16）：主列 = 档案库存条 + 扫码框 + 确认卡 +
//! 步骤行 + 大号判定；右列 = 今日写号统计 + 库存数据管理（SN/凭据导入、
//! 作废、清未用、出货明细 CSV）。
//!
//! 交互要点：扫码框回车即开写（焦点常驻——扫码枪是本页主要输入，其他
//! 控件拿到焦点时不抢）；写号 busy 期间输入锁定；确认卡是写前最后一道
//! 人工闸门（worker 阻塞等应答）。导入/作废走库存文件，引擎侧每次领用
//! 都从磁盘重读，两页不会写脏。

use crate::app::E02App;
use crate::provision::StepVerdict;
use crate::stock;
use crate::ui::theme;
use crate::report;

/// 小号说明文字（暗底版本）。
fn caption_dark(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme::ON_DARK_SOFT));
}

/// 小号说明文字（亮卡版本，同 panel_settings）。
fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme::MUTED));
}

/// 卡标题 + 右侧徽章（同 panel_settings）。
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

/// 卡内起手：输入件回退画布底 + 抬到卡全宽。
fn card_begin(ui: &mut egui::Ui) {
    theme::card_inputs(ui);
    ui.set_min_width(ui.available_width());
}

pub fn show(ui: &mut egui::Ui, app: &mut E02App) {
    // 右列：库存数据管理（固定宽，主列为扫码/流程留最大空间）
    egui::SidePanel::right("ship_manage")
        .resizable(false)
        .exact_width(330.0)
        .frame(
            egui::Frame::default()
                .fill(theme::CANVAS)
                .inner_margin(egui::Margin::symmetric(14.0, 12.0)),
        )
        .show_inside(ui, |ui| manage_column(ui, app));
    // 主列：深底"写号工位屏"（与测试页媒体井同一视觉语言）
    egui::CentralPanel::default()
        .frame(
            egui::Frame::default()
                .fill(theme::DARK)
                .inner_margin(egui::Margin::symmetric(16.0, 12.0)),
        )
        .show_inside(ui, |ui| main_column(ui, app));
}

// ————————————————————————————— 主列 —————————————————————————————

fn main_column(ui: &mut egui::Ui, app: &mut E02App) {
    profile_bar(ui, app);
    ui.add_space(8.0);
    scan_box(ui, app);
    ui.add_space(8.0);
    confirm_card(ui, app);
    ui.add_space(8.0);
    steps_list(ui, app);
    verdict_banner(ui, app);
}

/// 档案/库存条：当前出厂档案、闸门档、双池余量（开写前操作员一眼核对）。
fn profile_bar(ui: &mut egui::Ui, app: &mut E02App) {
    ui.horizontal_wrapped(|ui| {
        let prov = &app.config.provision;
        match prov.active() {
            Some(p) => {
                ui.label(
                    egui::RichText::new(format!("档案 {}", prov.active_profile))
                        .strong()
                        .color(theme::ON_DARK),
                );
                theme::badge(ui, p.model.label(), theme::ON_DARK, theme::DARK_ELEVATED);
                let gate = match prov.gate_mode {
                    crate::config::GateMode::Strict => "闸门 硬拦",
                    crate::config::GateMode::Warn => "闸门 告警可继续",
                    crate::config::GateMode::Off => "闸门 跳过",
                };
                theme::badge(ui, gate, theme::ON_DARK, theme::DARK_ELEVATED);
            }
            None => {
                ui.label(
                    egui::RichText::new("档案表为空（config.toml [provision]）")
                        .strong()
                        .color(theme::BAD),
                );
            }
        }
        let st = &app.ship.stock;
        let sn_text = if st.has_sn_pool() {
            format!("SN 池 未用 {}", st.available_sns())
        } else {
            "SN 池 未导入".to_string()
        };
        theme::badge(ui, &sn_text, theme::ON_DARK, theme::DARK_ELEVATED);
        let needs_cred = prov.active().is_some_and(|p| p.needs_credentials());
        if needs_cred {
            let n = st.available_credentials();
            let (fg, text) = if n == 0 {
                (theme::BAD, "凭据 缺池".to_string())
            } else {
                (theme::ON_DARK, format!("凭据 未用 {n}"))
            };
            theme::badge(ui, &text, fg, theme::DARK_ELEVATED);
        }
        if app.config.provision.auto_commit {
            theme::badge(ui, "扫码即写", theme::ON_DARK, theme::DARK_ELEVATED);
        }
    });
}

/// 扫码框：回车/按钮开写；busy 锁定；空闲且无其他焦点时抢回焦点。
fn scan_box(ui: &mut egui::Ui, app: &mut E02App) {
    let mut go = false;
    ui.horizontal(|ui| {
        let edit = egui::TextEdit::singleline(&mut app.ship.scan_input)
            .hint_text(if app.ship.busy {
                "写号进行中……"
            } else {
                "扫码 SN 后回车开始写号"
            })
            .font(egui::TextStyle::Monospace)
            .desired_width(ui.available_width().max(160.0) - 120.0)
            .min_size([0.0, 32.0].into());
        let resp = ui.add_enabled(!app.ship.busy, edit);
        if !app.ship.busy {
            // 焦点常驻（无其他输入件持有键盘焦点时）：扫码枪即扫即写。
            // egui 0.29 memory.focus 是 pub(crate)，用全局"是否有人要键盘"
            // 判断——导入框正在打字时为 true，不抢
            if !ui.ctx().wants_keyboard_input() || resp.has_focus() {
                resp.request_focus();
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
        }
        let can_start =
            !app.ship.busy && !app.ship.scan_input.trim().is_empty() && app.device_online();
        if ui
            .add_enabled(can_start, theme::primary("开始写号"))
            .clicked()
        {
            go = true;
        }
    });
    if go {
        let sn = app.ship.scan_input.clone();
        app.ship.scan_input.clear();
        app.start_ship(&sn);
    }
}

/// 确认卡（⑥）：暗屏上的亮卡，SN/凭据全值/设备现值/告警全部过目后放行。
fn confirm_card(ui: &mut egui::Ui, app: &mut E02App) {
    let Some(confirm) = app.ship.confirm.as_ref() else {
        return;
    };
    let card = confirm.card.clone();
    let mut answer: Option<bool> = None;
    egui::Frame::default()
        .fill(theme::CANVAS)
        .rounding(egui::Rounding::same(10.0))
        .inner_margin(egui::Margin::symmetric(14.0, 12.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("写号确认（放行后开始 cf30 写入）")
                        .strong()
                        .color(theme::WARN),
                );
                if card.re_run {
                    theme::badge(ui, "同号重写", theme::INK, theme::SELECTED);
                }
            });
            ui.add_space(6.0);
            egui::Grid::new("ship_confirm")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.label("SN");
                    ui.label(egui::RichText::new(&card.sn).monospace().strong());
                    ui.end_row();
                    ui.label("蓝牙 MAC");
                    ui.label(egui::RichText::new(&card.bt_mac).monospace());
                    ui.end_row();
                    ui.label("型号");
                    ui.label(&card.model);
                    ui.end_row();
                    if let Some((uuid, key)) = &card.credential {
                        ui.label("uuid");
                        ui.label(egui::RichText::new(uuid).monospace());
                        ui.end_row();
                        ui.label("auth_key");
                        ui.label(egui::RichText::new(key).monospace());
                        ui.end_row();
                    }
                    for (k, v) in &card.current {
                        ui.label(format!("现值 {k}"));
                        ui.label(if v.is_empty() {
                            egui::RichText::new("（未写）").color(theme::MUTED)
                        } else {
                            egui::RichText::new(v).monospace()
                        });
                        ui.end_row();
                    }
                });
            ui.add_space(4.0);
            if card.warnings.is_empty() {
                caption(ui, "无告警");
            } else {
                for w in &card.warnings {
                    ui.label(egui::RichText::new(format!("⚠ {w}")).color(theme::BAD));
                }
                caption(ui, "有告警请人工核实后再放行");
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add(theme::primary("确认写入")).clicked() {
                    answer = Some(true);
                }
                if ui.add(theme::danger("取消")).clicked() {
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

/// 步骤行（⓪–⑫）：判定着色，详情行内小字。
fn steps_list(ui: &mut egui::Ui, app: &mut E02App) {
    egui::Frame::default()
        .fill(theme::DARK_ELEVATED)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            for r in &app.ship.steps {
                ui.horizontal(|ui| {
                    let (label, fg) = match r.verdict {
                        StepVerdict::Pending => ("未测", theme::MUTED),
                        StepVerdict::Pass => ("PASS", theme::OK),
                        StepVerdict::Warn => ("告警", theme::WARN),
                        StepVerdict::Fail => ("FAIL", theme::BAD),
                        StepVerdict::Skip => ("跳过", theme::MUTED),
                    };
                    // 固定宽判定列 + 步骤名对齐（monospace 保证等宽）
                    ui.label(
                        egui::RichText::new(format!("{label:<4}"))
                            .monospace()
                            .strong()
                            .color(fg),
                    );
                    ui.label(
                        egui::RichText::new(r.step.label())
                            .strong()
                            .color(theme::ON_DARK),
                    );
                    if !r.detail.is_empty() {
                        ui.label(
                            egui::RichText::new(if r.secs > 0 {
                                format!("{}（{}s）", r.detail, r.secs)
                            } else {
                                r.detail.clone()
                            })
                            .small()
                            .color(theme::ON_DARK_SOFT),
                        );
                    }
                });
            }
        });
}

/// 大号判定（空闲时展示最近一轮结果）。
fn verdict_banner(ui: &mut egui::Ui, app: &mut E02App) {
    if app.ship.busy {
        return;
    }
    let Some(pass) = app.ship.last_pass else {
        return;
    };
    ui.add_space(10.0);
    if pass {
        ui.label(
            egui::RichText::new("✓ 写号完成")
                .size(30.0)
                .strong()
                .color(theme::OK),
        );
        caption_dark(ui, "已切正式版并重启；下一台扫码继续");
    } else {
        ui.label(
            egui::RichText::new("✗ 写号失败")
                .size(30.0)
                .strong()
                .color(theme::BAD),
        );
        caption_dark(ui, "失败原因见步骤行与底部日志；未切正式版，返修后同 SN 重跑自动复原凭据");
    }
}

// ————————————————————————————— 右列 —————————————————————————————

fn manage_column(ui: &mut egui::Ui, app: &mut E02App) {
    // —— 今日写号统计（ship- 报告扫描）——
    theme::card().show(ui, |ui| {
        card_begin(ui);
        card_head(ui, "今日写号", None);
        ui.add_space(4.0);
        if let Some(t) = &app.ship.today {
            theme::today_badges(ui, t);
        }
        ui.add_space(2.0);
        caption(ui, "按出厂报告统计；失败台也留报告可查");
    });

    ui.add_space(12.0);

    // —— 库存数据管理（FQC §4）——
    theme::card().show(ui, |ui| {
        card_begin(ui);
        let st = &app.ship.stock;
        card_head(
            ui,
            "库存与数据",
            Some(format!(
                "SN {}/{} 凭据 {}/{}",
                st.available_sns(),
                st.sns.len(),
                st.available_credentials(),
                st.credentials.len()
            )),
        );
        ui.add_space(4.0);
        caption(ui, "右列计数：未用/总数（已分配、已写入、已作废保留）");
        ui.add_space(6.0);

        let busy = app.ship.busy; // 写号中不并改库存（引擎每次领用重读磁盘）
        ui.add_enabled_ui(!busy, |ui| {
            caption(ui, "SN 池导入（发号系统 txt：一行一个，# 注释）");
            let resp = ui.add(
                egui::TextEdit::multiline(&mut app.ship.sn_text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
            if resp.changed() {
                app.ship.sn_preview = sn_preview_text(app);
            }
            if !app.ship.sn_preview.is_empty() {
                caption(ui, &app.ship.sn_preview);
            }
            if ui.button("导入 SN 池").clicked() {
                import_sns(app);
            }
            ui.add_space(6.0);
            caption(ui, "凭据导入（客户预分配 txt：uuid,auth_key 一行一对）");
            let resp = ui.add(
                egui::TextEdit::multiline(&mut app.ship.cred_text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
            if resp.changed() {
                app.ship.cred_preview = cred_preview_text(app);
            }
            if !app.ship.cred_preview.is_empty() {
                caption(ui, &app.ship.cred_preview);
            }
            if ui.button("导入凭据").clicked() {
                import_creds(app);
            }
        });
        if busy {
            caption(ui, "写号进行中，导入/作废暂停");
        }

        // 操作回执（错误列行号）
        if !app.ship.note.is_empty() {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(&app.ship.note).small().color(theme::BODY));
        }

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);

        // 作废（报废机/错发凭据）：SN 或 uuid 均可
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut app.ship.void_input)
                        .hint_text("SN 或 uuid")
                        .desired_width(150.0)
                        .font(egui::TextStyle::Monospace),
                );
                if app.ship.void_armed {
                    if ui.add(theme::danger("确认作废？")).clicked() {
                        app.ship.void_armed = false;
                        do_void(app);
                    }
                } else if ui.button("作废").clicked() {
                    app.ship.void_armed = true;
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // 清未用（换批）：两步确认
                if app.ship.clear_sn_armed {
                    if ui.add(theme::danger("确认清空？")).clicked() {
                        app.ship.clear_sn_armed = false;
                        clear_available(app, true);
                    }
                } else if ui.button("清空未用 SN").clicked() {
                    app.ship.clear_sn_armed = true;
                }
                if app.ship.clear_cred_armed {
                    if ui.add(theme::danger("确认清空？")).clicked() {
                        app.ship.clear_cred_armed = false;
                        clear_available(app, false);
                    }
                } else if ui.button("清空未用凭据").clicked() {
                    app.ship.clear_cred_armed = true;
                }
            });
        });
        ui.add_space(2.0);
        caption(ui, "作废/清空只动未用与指定条目，已写入记录永不重置");

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);

        // 出货明细 CSV（台账 × 库存关联）
        if ui
            .checkbox(&mut app.ship.include_auth_key, "CSV 附带 auth_key")
            .changed()
        {
            app.ship.void_armed = false;
        }
        if ui.button("导出出货明细 CSV").clicked() {
            app.export_ship_csv();
        }
        ui.add_space(2.0);
        caption(ui, "落数据目录（Excel 直开）；默认只导 uuid，勾选才带 auth_key");
    });
    ui.add_space(4.0);
}

// ————————————————————————————— 库存操作 —————————————————————————————

/// 库存落盘 + 回执；成功才替换 UI 快照。
fn commit_stock(app: &mut E02App, st: stock::ProvisionStock, ok_msg: String) {
    let path = stock::stock_path();
    match st.save(&path) {
        Ok(()) => {
            app.push_log(format!("{ok_msg}（{}）", path.display()));
            app.ship.stock = st;
            app.ship.note = ok_msg;
        }
        Err(e) => app.ship.note = format!("库存保存失败: {e}"),
    }
}

fn now_stamp() -> String {
    report::time_str(std::time::SystemTime::now())
}

/// 导入预检回显（FQC §4：整批校验过才落库——预检不入库，错误带行号）。
fn outcome_text(out: &stock::ImportOutcome) -> Option<String> {
    if !out.ok() {
        Some(format!("预检未过（不会入库）：{}", out.errors.join("；")))
    } else if out.added > 0 || out.skipped_dup > 0 {
        Some(format!("预检：将新增 {}，跳过重复 {}", out.added, out.skipped_dup))
    } else {
        None
    }
}

fn sn_preview_text(app: &E02App) -> String {
    if app.ship.sn_text.trim().is_empty() {
        return String::new();
    }
    match app.config.provision.active() {
        Some(p) => {
            outcome_text(&stock::preview_sn_import(&app.ship.stock, &app.ship.sn_text, &p.sn_pattern))
                .unwrap_or_default()
        }
        None => "档案表为空，无法预检".into(),
    }
}

fn cred_preview_text(app: &E02App) -> String {
    if app.ship.cred_text.trim().is_empty() {
        return String::new();
    }
    match app.config.provision.active() {
        Some(p) => outcome_text(&stock::preview_cred_import(
            &app.ship.stock,
            &app.ship.cred_text,
            &p.uuid_pattern,
            &p.key_pattern,
        ))
        .unwrap_or_default(),
        None => "档案表为空，无法预检".into(),
    }
}

/// SN 池导入：整批校验（格式按当前档案正则），失败库存不动。
fn import_sns(app: &mut E02App) {
    let Some(p) = app.config.provision.active().cloned() else {
        app.ship.note = "出厂档案表为空，无法导入".into();
        return;
    };
    // 克隆隔离：失败分支不回写（UI 快照不动）
    let mut st = app.ship.stock.clone();
    let out = st.import_sns(&app.ship.sn_text, &p.sn_pattern, &now_stamp());
    if !out.ok() {
        app.ship.note = format!("SN 导入被拒（库存未变）：{}", out.errors.join("；"));
        return;
    }
    let text = std::mem::take(&mut app.ship.sn_text);
    app.ship.sn_preview.clear();
    commit_stock(
        app,
        st,
        format!(
            "SN 导入成功：新增 {} 台，跳过重复 {} 台（{} 行）",
            out.added,
            out.skipped_dup,
            text.lines().count()
        ),
    );
}

/// 凭据导入：两列解析 + 格式/冲突校验，失败库存不动。
fn import_creds(app: &mut E02App) {
    let Some(p) = app.config.provision.active().cloned() else {
        app.ship.note = "出厂档案表为空，无法导入".into();
        return;
    };
    let mut st = app.ship.stock.clone();
    let out = st.import_credentials(
        &app.ship.cred_text,
        &p.uuid_pattern,
        &p.key_pattern,
        &now_stamp(),
    );
    if !out.ok() {
        app.ship.note = format!(
            "凭据导入被拒（库存未变）：{}",
            out.errors.join("；")
        );
        return;
    }
    let text = std::mem::take(&mut app.ship.cred_text);
    app.ship.cred_preview.clear();
    commit_stock(
        app,
        st,
        format!(
            "凭据导入成功：新增 {} 对，跳过重复 {} 对（{} 行）",
            out.added,
            out.skipped_dup,
            text.lines().count()
        ),
    );
}

/// 作废：输入按 SN（大写归一）或 uuid（保留原大小写）匹配。
fn do_void(app: &mut E02App) {
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
    // uuid 大小写不敏感匹配，落库用条目原值
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
    app.ship.note = format!("库存中未找到 {raw}（SN 与 uuid 均无匹配）");
}

/// 清空未用（换批）：只删 available，绝不碰已分配/已写入/已作废。
fn clear_available(app: &mut E02App, sns: bool) {
    let mut st = app.ship.stock.clone();
    let (n, what) = if sns {
        (st.clear_available_sns(), "未用 SN")
    } else {
        (st.clear_available_credentials(), "未用凭据")
    };
    commit_stock(app, st, format!("已清空 {what} {n} 条"));
}
