//! 应用状态与窗口装配：worker 事件消费、媒体显示、人工判定应答、三工位统一布局。

use crate::auth::{self, AuthRole};
use crate::config::{self, Config};
use crate::devwatch::{self, DevState};
use crate::engine::events::{UiEvent, UiResponse};
use crate::engine::worker;
use crate::engine::{TestItem, TestResult};
use crate::exec::StopReason;
use crate::protocol::DeviceInfo;
use crate::provision;
use crate::report;
use crate::stock;
use crate::ui;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// 内嵌中文字体：Droid Sans Fallback（Apache-2.0，完整 CJK 覆盖，3.9MB）。
const CJK_FONT: &[u8] = include_bytes!("../assets/fonts/DroidSansFallbackFull.ttf");

/// 视频回放帧率（worker 抽帧同为 2fps）。
const PLAYBACK_INTERVAL: Duration = Duration::from_millis(200);

/// 4G 模板首启占位注释（文件尚不存在时填充到设置页编辑器）。
const DEFAULT_FOURG_TPL_HINT: &str = "\
# E02E 4G 模组配置模板（每行 key=value，禁含引号，支持 {{sn}} 变量）
# 示例：
# apn=cmnet
# server=ota.example.com
";

/// 人工判定待应答请求（UI 弹窗收集应答后经 reply 传回 worker）。
pub struct ManualAsk {
    pub title: String,
    pub question: String,
    /// 判定对象在媒体井（录像/拍照）：井底判定条；否则居中弹窗
    pub in_media: bool,
    /// 应答截止时刻（None = 不限时）；到点 worker 自动判失败
    pub deadline: Option<Instant>,
    reply: Sender<UiResponse>,
}

impl ManualAsk {
    pub fn answer(&self, yes: bool) {
        let _ = self.reply.send(UiResponse::Manual(yes));
    }
}

/// 按键引导弹窗状态（worker 经 [`UiEvent::KeysUpdate`] 驱动）。
pub struct KeysPrompt {
    /// 已检测到的键码（4=多功能 5=开机 6=音量）
    pub detected: Vec<u8>,
    /// true：未收齐，等待操作员 重测/改判
    pub awaiting: bool,
    /// 应答截止时刻（None = 不限时）；到点 worker 自动判失败
    pub deadline: Option<Instant>,
    reply: Sender<UiResponse>,
}

impl KeysPrompt {
    pub fn retry(&self) {
        let _ = self.reply.send(UiResponse::KeysRetry);
    }

    pub fn force(&self, yes: bool) {
        let _ = self.reply.send(UiResponse::KeysForce(yes));
    }
}

/// 序列进度（顶栏进度条数据）。
pub struct Progress {
    pub total: usize,
    pub done: usize,
    pub started: Instant,
}

/// 媒体区状态：占位说明 / 拍照图片 / 录像抽帧回放。
pub enum MediaState {
    Note(String),
    Photo {
        texture: egui::TextureHandle,
        size: [f32; 2],
    },
    Video {
        frames: Vec<egui::TextureHandle>,
        sizes: Vec<[f32; 2]>,
        idx: usize,
        last: Instant,
        has_audio: bool,
    },
}

/// 写号确认卡（⑥：worker 阻塞等应答；UI 按钮/停止键经 reply 放行或取消）。
pub struct ShipConfirm {
    pub card: provision::ConfirmCard,
    reply: Sender<provision::ShipAnswer>,
}

impl ShipConfirm {
    pub fn answer(&self, yes: bool) {
        let _ = self.reply.send(provision::ShipAnswer::Confirm(yes));
    }
}

type FilePickMsg = (bool, Result<Option<PathBuf>, String>);

/// 出厂写号工位界面状态（FQC §6）。
pub struct ShipUi {
    /// 写号流程进行中（扫码输入锁定）
    pub busy: bool,
    /// 当前正在执行的步骤（左栏高亮与顶栏/底栏状态展示）
    pub current: Option<provision::ShipStep>,
    /// 写号进度（顶栏进度条与右栏写号结论卡共用）
    pub(crate) progress: Option<Progress>,
    /// 右侧被测设备实时字段快照
    pub device: provision::ShipDeviceView,
    /// 扫码输入框（回车即开写；trim + 大写后传引擎）
    pub scan_input: String,
    /// 步骤行（按当前型号有效步骤排好，事件按 step 原位更新）
    pub steps: Vec<provision::ShipStepResult>,
    /// 待应答确认卡
    pub confirm: Option<ShipConfirm>,
    /// ⑪ cf4 后 adb reboot 已发：屏蔽 devwatch 拔出中止直到 Finished
    pub rebooting: bool,
    /// 最近一轮判定（None = 未跑过）
    pub last_pass: Option<bool>,
    /// 今日写号统计（ship- 报告扫描）
    pub today: Option<report::DayStats>,
    /// 批次与库存管理弹窗开关
    pub stock_open: bool,
    /// 原生文件选择窗后台等待中
    pub file_picking: bool,
    pub(crate) file_pick_rx: Option<Receiver<FilePickMsg>>,
    /// 库存快照（SN/凭据双池；导入/作废/写号结束后刷新）
    pub stock: stock::ProvisionStock,
    /// SN txt 手动粘贴框
    pub sn_text: String,
    /// 凭据 txt 手动粘贴框
    pub cred_text: String,
    /// 导入预检回显
    pub sn_preview: String,
    pub cred_preview: String,
    /// 操作回执提示
    pub note: String,
    /// 最近一次导出的出货明细 CSV 路径（用于「打开文件位置」）
    pub last_export_path: Option<PathBuf>,
    /// 出货 CSV 勾选：附带 auth_key（默认只导 uuid）
    pub include_auth_key: bool,
    /// 作废输入（SN 或 uuid）
    pub void_input: String,
    /// 作废两步确认
    pub void_armed: bool,
    /// 清空未用 SN 两步确认
    pub clear_sn_armed: bool,
    /// 清空未用凭据两步确认
    pub clear_cred_armed: bool,
    /// 设置页 4G 模板内联编辑缓冲
    pub fourg_tpl_buf: String,
    pub fourg_tpl_dirty: bool,
}

impl Default for ShipUi {
    fn default() -> Self {
        Self {
            busy: false,
            current: None,
            progress: None,
            device: provision::ShipDeviceView::default(),
            scan_input: String::new(),
            steps: provision::ShipStep::ALL
                .map(|step| provision::ShipStepResult {
                    step,
                    verdict: provision::StepVerdict::Pending,
                    detail: String::new(),
                    secs: 0,
                })
                .to_vec(),
            confirm: None,
            rebooting: false,
            last_pass: None,
            today: None,
            stock_open: false,
            file_picking: false,
            file_pick_rx: None,
            stock: stock::ProvisionStock::default(),
            sn_text: String::new(),
            cred_text: String::new(),
            sn_preview: String::new(),
            cred_preview: String::new(),
            note: String::new(),
            last_export_path: None,
            include_auth_key: false,
            void_input: String::new(),
            void_armed: false,
            clear_sn_armed: false,
            clear_cred_armed: false,
            fourg_tpl_buf: String::new(),
            fourg_tpl_dirty: false,
        }
    }
}

/// 设置页管理员登录与改密 UI 状态（关闭设置窗自动清空并锁定）。
pub struct AdminAuthUi {
    /// 已解锁角色（None = 普通模式，锁定高级设置）
    pub role: Option<AuthRole>,
    /// 展开登录输入框
    pub login_open: bool,
    pub user_input: String,
    pub pass_input: String,
    pub login_err: String,
    /// 展开修改 admin 密码表单
    pub change_pwd_open: bool,
    pub new_pwd: String,
    pub confirm_pwd: String,
    pub pwd_msg: String,
    pub pwd_is_err: bool,
}

impl Default for AdminAuthUi {
    fn default() -> Self {
        Self {
            role: None,
            login_open: false,
            user_input: auth::ADMIN_USER.to_string(),
            pass_input: String::new(),
            login_err: String::new(),
            change_pwd_open: false,
            new_pwd: String::new(),
            confirm_pwd: String::new(),
            pwd_msg: String::new(),
            pwd_is_err: false,
        }
    }
}

/// 应用全局状态。
pub struct E02App {
    /// 被测设备信息（测试工位）
    pub device: DeviceInfo,
    /// 设备在位状态（V3.8 USB 插拔监测：开测门槛与拔出中止依据）
    pub dev: DevState,
    /// 当前连接的这台设备是否已完成过时间同步（同一次接入只校时一次）
    time_synced: bool,
    /// 各测试项结果，顺序与左侧面板一致
    pub results: Vec<TestResult>,
    /// 测试序列进行中
    pub busy: bool,
    /// 当前正在执行的测试项
    pub current: Option<TestItem>,
    /// 待应答的人工判定弹窗
    pub manual: Option<ManualAsk>,
    /// 媒体区
    pub media: MediaState,
    /// 过程日志（尾部若干条）
    pub log: VecDeque<String>,
    /// 序列进度；空闲时 None
    pub(crate) progress: Option<Progress>,
    /// 按键引导弹窗；无按键测试进行时 None
    pub keys_prompt: Option<KeysPrompt>,
    /// 工位配置（V2.2）
    pub config: Config,
    /// 设置窗口开关
    pub settings_open: bool,
    /// 设置页管理员权限状态
    pub auth_ui: AdminAuthUi,
    /// 最近一次导出的绑定记录 CSV 路径
    pub last_binding_export_path: Option<PathBuf>,
    /// 最近一次导出绑定记录的回执提示
    pub binding_export_note: String,
    /// 最近一次报告路径（Finished 时更新，报告中心默认选中用）
    pub last_report: Option<PathBuf>,
    /// 报告中心窗口开关
    pub report_open: bool,
    /// 报告中心筛选维度
    pub report_filter: ui::dialog_report::ReportFilter,
    /// 报告目录（数据目录下 reports/，内部按 YYYY-MM-DD 日期分目录）
    pub reports_dir: PathBuf,
    /// 今日统计（测试工位）
    pub today: Option<report::DayStats>,
    /// 报告中心：历史列表（新→旧）
    pub report_list: Vec<PathBuf>,
    /// 报告中心：当前选中
    pub report_sel: Option<PathBuf>,
    /// 报告中心：选中文件内容缓存（路径, 文本）
    report_text: Option<(PathBuf, String)>,
    /// 台账卡快照（设置窗打开时从磁盘加载）
    pub ledger_view: crate::ledger::Ledger,
    /// 台账"清空"两步确认状态
    pub ledger_clear_armed: bool,
    /// 当前产品型号（全局唯一真源）
    pub model: config::ModelKind,
    /// 出厂写号工位界面状态
    pub ship: ShipUi,
    event_rx: Option<Receiver<UiEvent>>,
    dev_rx: Receiver<DevState>,
    manual_tx: Option<Sender<UiResponse>>,
    ship_rx: Option<Receiver<provision::ShipEvent>>,
    ship_answer_tx: Option<Sender<provision::ShipAnswer>>,
    audio_out: Option<(rodio::OutputStream, rodio::OutputStreamHandle)>,
    audio_sink: Option<rodio::Sink>,
}

impl E02App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        ui::theme::apply(&cc.egui_ctx);
        let dev_rx = devwatch::spawn(&cc.egui_ctx);
        let (cfg, cfg_err) = match Config::load(&config::default_path()) {
            Ok(c) => (c, None),
            Err(e) => (
                Config::builtin(),
                Some(format!("配置加载失败，已用内置默认: {e:#}")),
            ),
        };
        let model = cfg.model.unwrap_or_default();
        let mut app = Self {
            device: DeviceInfo::default(),
            dev: DevState::Disconnected,
            time_synced: false,
            results: TestItem::ALL.map(TestResult::new).to_vec(),
            busy: false,
            current: None,
            manual: None,
            media: MediaState::Note("媒体区：拍照显示图片，录像解码后回放".into()),
            log: VecDeque::new(),
            progress: None,
            keys_prompt: None,
            config: cfg,
            settings_open: false,
            auth_ui: AdminAuthUi::default(),
            last_binding_export_path: None,
            binding_export_note: String::new(),
            last_report: None,
            report_open: false,
            report_filter: ui::dialog_report::ReportFilter::All,
            reports_dir: crate::paths::reports_dir(),
            today: None,
            report_list: Vec::new(),
            report_sel: None,
            report_text: None,
            ledger_view: crate::ledger::Ledger::default(),
            ledger_clear_armed: false,
            model,
            ship: ShipUi::default(),
            event_rx: None,
            dev_rx,
            manual_tx: None,
            ship_rx: None,
            ship_answer_tx: None,
            audio_out: None,
            audio_sink: None,
        };
        if let Some(msg) = cfg_err {
            app.push_log(msg);
        }
        app.sync_ship_steps_for_model();
        app.reload_fourg_template_buf();
        app.ship.stock = stock::ProvisionStock::load(&stock::stock_path());
        app.refresh_stats();
        app.refresh_ship_stats();

        if app.config.is_ship_station() {
            app.push_log(format!(
                "工位: 出厂写号（型号 {}），数据目录 {}",
                app.model.label(),
                crate::paths::data_dir().display()
            ));
        } else {
            app.push_log(format!(
                "工位: {}（型号 {}，启用 {} 项），数据目录 {}",
                Config::station_label(&app.config.station),
                app.model.label(),
                app.batch_items().len(),
                crate::paths::data_dir().display()
            ));
        }
        app.push_log("等待设备插入（USB 连接后自动检测）");
        app
    }

    /// 按当前型号同步写号步骤表（空闲时调用）。
    pub fn sync_ship_steps_for_model(&mut self) {
        if self.ship.busy {
            return;
        }
        self.ship.steps = provision::ShipStep::steps_for(self.model)
            .into_iter()
            .map(|step| provision::ShipStepResult {
                step,
                verdict: provision::StepVerdict::Pending,
                detail: String::new(),
                secs: 0,
            })
            .collect();
    }

    /// 从磁盘加载 E02E 的 4G 模板文件到设置页编辑缓冲（文件不存在时填充注释示例）。
    pub fn reload_fourg_template_buf(&mut self) {
        let rel = self
            .config
            .provision
            .e02e
            .fourg
            .as_ref()
            .map(|f| f.template.trim())
            .unwrap_or("templates/e02e_4g.ini");
        if rel.is_empty() {
            self.ship.fourg_tpl_buf = DEFAULT_FOURG_TPL_HINT.to_string();
            self.ship.fourg_tpl_dirty = false;
            return;
        }
        let full = crate::paths::data_dir().join(rel);
        self.ship.fourg_tpl_buf =
            std::fs::read_to_string(&full).unwrap_or_else(|_| DEFAULT_FOURG_TPL_HINT.to_string());
        self.ship.fourg_tpl_dirty = false;
    }

    /// 开始测试项集：当前工位启用项，按原程序批次顺序执行并按型号过滤。
    pub fn batch_items(&self) -> Vec<TestItem> {
        let active = self.config.active_items();
        worker::BATCH_ORDER
            .into_iter()
            .filter(|i| active.contains(i) && i.available_for(self.model))
            .collect()
    }

    /// 设备是否在线且唯一。
    pub fn device_online(&self) -> bool {
        matches!(self.dev, DevState::Online(_))
    }

    /// 启动测试序列（单项或全测）；busy 期间忽略重复触发。
    pub fn start_run(&mut self, items: Vec<TestItem>) {
        if self.busy || self.ship.busy || items.is_empty() {
            return;
        }
        if !self.device_online() {
            self.push_log(match &self.dev {
                DevState::Disconnected => "设备未连接：请插入 USB 线后再开始".to_string(),
                DevState::Multiple => "检测到多台设备：请只连接一台被测耳机".to_string(),
                DevState::Online(_) => unreachable!("device_online 已排除"),
            });
            return;
        }
        self.busy = true;
        self.current = items.first().copied();
        self.progress = Some(Progress {
            total: items.len(),
            done: 0,
            started: Instant::now(),
        });
        self.keys_prompt = None;
        let (tx, rx) = mpsc::channel();
        let (mtx, mrx) = mpsc::channel();
        self.event_rx = Some(rx);
        self.manual_tx = Some(mtx);
        let media = crate::paths::data_dir().join("media");
        let reports = self.reports_dir.clone();
        let n = items.len();
        let cfg = self.config.clone();
        let serial = match &self.dev {
            DevState::Online(sn) => Some(sn.clone()),
            _ => None,
        };
        let base = if items.len() == 1 {
            let active = self.config.active_items();
            worker::RunBase {
                device: self.device.clone(),
                results: self
                    .results
                    .iter()
                    .filter(|r| active.contains(&r.item) && r.item.available_for(self.model))
                    .cloned()
                    .collect(),
                skip_time_sync: self.time_synced,
            }
        } else {
            worker::RunBase {
                skip_time_sync: self.time_synced,
                ..worker::RunBase::default()
            }
        };
        thread::spawn(move || {
            let report =
                worker::run_sequence(&items, &tx, &mrx, &media, &reports, &cfg, base, serial);
            let _ = tx.send(UiEvent::Finished { report });
        });
        self.push_log(format!("开始测试，共 {n} 项（自动项先行，人工项弹窗判定）"));
    }

    /// 操作员请求停止测试。
    pub fn request_stop(&mut self, reason: StopReason) {
        crate::exec::request_abort(reason);
        self.keys_prompt = None;
        self.manual = None;
        self.stop_audio();
        let msg = match reason {
            StopReason::User => "已请求停止，等待当前命令退出...",
            StopReason::DeviceGone => "设备已拔出，测试中止，等待当前命令退出...",
        };
        self.push_log(msg);
    }

    /// 开始一轮出厂写号（扫码回车触发；busy 期间忽略）。
    pub fn start_ship(&mut self, sn: &str) {
        let sn = sn.trim().to_uppercase();
        if self.ship.busy || self.busy || sn.is_empty() {
            return;
        }
        if !self.device_online() {
            self.push_log(match &self.dev {
                DevState::Disconnected => "设备未连接：请插入 USB 线后再扫码写号".to_string(),
                DevState::Multiple => "检测到多台设备：请只连接一台被测耳机".to_string(),
                DevState::Online(_) => unreachable!("device_online 已排除"),
            });
            return;
        }
        let step_list = provision::ShipStep::steps_for(self.model);
        self.ship.busy = true;
        self.ship.current = step_list.first().copied();
        self.ship.progress = Some(Progress {
            total: step_list.len(),
            done: 0,
            started: Instant::now(),
        });
        self.ship.steps = step_list
            .into_iter()
            .map(|step| provision::ShipStepResult {
                step,
                verdict: provision::StepVerdict::Pending,
                detail: String::new(),
                secs: 0,
            })
            .collect();
        self.ship.device = provision::ShipDeviceView {
            target_sn: sn.clone(),
            ..Default::default()
        };
        self.ship.last_pass = None;
        self.ship.rebooting = false;
        self.ship.confirm = None;
        self.ship.note.clear();
        let (tx, rx) = mpsc::channel();
        let (atx, arx) = mpsc::channel();
        self.ship_rx = Some(rx);
        self.ship_answer_tx = Some(atx);
        let serial = match &self.dev {
            DevState::Online(s) => Some(s.clone()),
            _ => None,
        };
        let mut cfg = self.config.clone();
        cfg.model = Some(self.model);
        let ctx = provision::ShipCtx::new(&cfg, &self.reports_dir);
        let scanned = sn.clone();
        thread::spawn(move || {
            provision::run_ship(&scanned, serial, &tx, &arx, &ctx);
        });
        self.push_log(format!("出厂写号：扫码 SN {sn}"));
    }

    /// 停止写号：确认卡先答"否"，再请求中止当前命令。
    pub fn request_ship_stop(&mut self, reason: StopReason) {
        if !self.ship.busy {
            return;
        }
        if let Some(c) = self.ship.confirm.take() {
            c.answer(false);
        }
        crate::exec::request_abort(reason);
        let msg = match reason {
            StopReason::User => "已请求停止出厂写号，等待当前命令退出...",
            StopReason::DeviceGone => "设备已拔出，写号中止，等待当前命令退出...",
        };
        self.push_log(msg);
    }

    /// 重扫今日写号统计（ship- 报告）。
    pub fn refresh_ship_stats(&mut self) {
        self.ship.today = Some(report::scan_ship_stats(
            &self.reports_dir,
            &report::today_str(),
        ));
    }

    fn drain_ship_events(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        if let Some(rx) = &self.ship_rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
        }
        for ev in events {
            self.handle_ship_event(ev, ctx);
        }
    }

    fn handle_ship_event(&mut self, ev: provision::ShipEvent, ctx: &egui::Context) {
        use provision::ShipEvent;
        match ev {
            ShipEvent::Started => {}
            ShipEvent::Current(step) => {
                self.ship.current = Some(step);
                if let Some(slot) = self.ship.steps.iter_mut().find(|s| s.step == step) {
                    slot.verdict = provision::StepVerdict::Pending;
                    slot.detail = String::new();
                    slot.secs = 0;
                }
            }
            ShipEvent::StepResult(r) => {
                if let Some(p) = &mut self.ship.progress {
                    p.done = (p.done + 1).min(p.total);
                }
                if let Some(slot) = self.ship.steps.iter_mut().find(|s| s.step == r.step) {
                    *slot = r;
                } else {
                    self.ship.steps.push(r);
                }
            }
            ShipEvent::Device(d) => {
                self.ship.device = d;
            }
            ShipEvent::Confirm(card) => {
                if let Some(reply) = self.ship_answer_tx.clone() {
                    attention(ctx);
                    self.ship.confirm = Some(ShipConfirm { card, reply });
                }
            }
            ShipEvent::Rebooting => {
                self.ship.rebooting = true;
            }
            ShipEvent::Log(s) => self.push_log(s),
            ShipEvent::Finished { report, pass } => {
                self.ship.busy = false;
                self.ship.current = None;
                self.ship.progress = None;
                self.ship.rebooting = false;
                self.ship.confirm = None;
                self.ship.last_pass = Some(pass);
                self.last_report = report.clone();
                self.ship_rx = None;
                self.ship_answer_tx = None;
                match &report {
                    Some(p) => self.push_log(format!("出厂报告: {}", p.display())),
                    None => self.push_log("出厂报告写入失败"),
                }
                self.ship.stock = stock::ProvisionStock::load(&stock::stock_path());
                self.refresh_ship_stats();
                self.refresh_reports();
            }
        }
    }

    /// 是否处于管理员已解锁状态。
    pub fn is_admin(&self) -> bool {
        self.auth_ui.role.is_some()
    }

    /// 退出管理员模式并清空全部密码输入缓冲。
    pub fn lock_admin(&mut self) {
        self.auth_ui.role = None;
        self.auth_ui.login_open = false;
        self.auth_ui.pass_input.clear();
        self.auth_ui.login_err.clear();
        self.auth_ui.change_pwd_open = false;
        self.auth_ui.new_pwd.clear();
        self.auth_ui.confirm_pwd.clear();
        self.auth_ui.pwd_msg.clear();
        self.auth_ui.pwd_is_err = false;
        self.ledger_clear_armed = false;
    }

    /// 导出出货明细 CSV（台账 shipped 记录 × 库存凭据关联，存放于 `exports/shipments/`）。
    pub fn export_ship_csv(&mut self) {
        let ledger = crate::ledger::Ledger::load(&crate::ledger::path_of(&self.config));
        let shipped = ledger.shipped();
        if shipped.is_empty() {
            let msg = "暂无已出厂记录，无需导出".to_string();
            self.ship.note = msg.clone();
            self.push_log(msg);
            return;
        }
        let mut rows = Vec::new();
        for rec in shipped {
            let cred = self.ship.stock.credential_by_sn(&rec.shipped_sn);
            let (wifi_mac, imei) = if rec.model == "E02E" {
                (String::new(), rec.partner.clone())
            } else {
                (rec.partner.clone(), String::new())
            };
            rows.push(stock::ShipCsvRow {
                sn: rec.shipped_sn.clone(),
                bt_mac: rec.bt_mac.clone(),
                model: if rec.model.is_empty() || rec.model == "E02" {
                    "E02W".into()
                } else {
                    rec.model.clone()
                },
                shipped_at: rec.shipped_at.clone(),
                ship_verdict: rec.ship_verdict.clone(),
                wifi_mac,
                imei,
                ccid: rec.ccid.clone(),
                uuid: cred.map(|c| c.uuid.clone()).unwrap_or_default(),
                auth_key: cred.map(|c| c.auth_key.clone()).unwrap_or_default(),
            });
        }
        let dir = crate::paths::shipments_export_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            let msg = format!("创建出货明细导出目录失败: {e}");
            self.ship.note = msg.clone();
            self.push_log(msg);
            return;
        }
        let path = dir.join(format!(
            "出货明细-{}.csv",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        match std::fs::write(&path, stock::ship_csv(&rows, self.ship.include_auth_key)) {
            Ok(()) => {
                self.ship.last_export_path = Some(path.clone());
                let msg = format!("出货明细已导出 {} 台（{}）", rows.len(), path.display());
                self.ship.note = format!("出货明细已导出 {} 台：{}", rows.len(), path.display());
                self.push_log(msg);
            }
            Err(e) => {
                let msg = format!("出货明细导出失败: {e}");
                self.ship.note = msg.clone();
                self.push_log(msg);
            }
        }
    }

    /// 清空测试工位上一台界面显示（在下一台插入沿调用）。
    fn clear_unit_state(&mut self) {
        self.results = TestItem::ALL.map(TestResult::new).to_vec();
        self.device = DeviceInfo::default();
        self.time_synced = false;
        self.stop_audio();
        self.media = MediaState::Note("媒体区：拍照显示图片，录像解码后回放".into());
    }

    /// 清空写号工位上一台界面显示（在下一台插入沿调用，拔线后仍保留上台结论供查看）。
    fn clear_ship_unit_state(&mut self) {
        self.sync_ship_steps_for_model();
        self.ship.device = provision::ShipDeviceView::default();
        self.ship.last_pass = None;
        self.ship.current = None;
        self.ship.progress = None;
    }

    fn play_audio(&mut self, wav: &Path) {
        let handle = match &self.audio_out {
            Some((_, h)) => h.clone(),
            None => match rodio::OutputStream::try_default() {
                Ok((stream, handle)) => {
                    self.audio_out = Some((stream, handle.clone()));
                    handle
                }
                Err(e) => {
                    self.push_log(format!("音频设备不可用（静音回放）: {e}"));
                    return;
                }
            },
        };
        match rodio::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(wav).expect("音轨文件刚抽出来"),
        )) {
            Ok(source) => match rodio::Sink::try_new(&handle) {
                Ok(sink) => {
                    sink.append(source);
                    self.audio_sink = Some(sink);
                }
                Err(e) => self.push_log(format!("音频播放失败（静音回放）: {e}")),
            },
            Err(e) => self.push_log(format!("音轨解码失败（静音回放）: {e}")),
        }
    }

    fn stop_audio(&mut self) {
        if let Some(sink) = self.audio_sink.take() {
            sink.stop();
        }
    }

    /// 重扫当前测试工位今日统计（当工位为 ship 时兜底扫 complete）。
    pub fn refresh_stats(&mut self) {
        let station = if self.config.is_ship_station() {
            "complete"
        } else {
            self.config.station.as_str()
        };
        self.today = Some(report::scan_stats(
            &self.reports_dir,
            &report::today_str(),
            station,
        ));
    }

    /// 重扫报告历史列表。
    pub fn refresh_reports(&mut self) {
        self.report_list = report::list_reports(&self.reports_dir, 300);
        self.report_sel = if self
            .report_sel
            .as_ref()
            .is_some_and(|p| self.report_list.contains(p))
        {
            self.report_sel.clone()
        } else {
            self.last_report
                .clone()
                .filter(|p| self.report_list.contains(p))
                .or_else(|| self.report_list.first().cloned())
        };
        self.report_text = None;
    }

    pub fn report_content(&mut self) -> Option<&str> {
        let want = self.report_sel.clone()?;
        if self.report_text.as_ref().is_some_and(|(p, _)| *p == want) {
            return self.report_text.as_ref().map(|(_, t)| t.as_str());
        }
        let text = std::fs::read_to_string(&want).unwrap_or_else(|e| format!("报告读取失败: {e}"));
        let text = report::sanitize(&text);
        self.report_text = Some((want, text));
        self.report_text.as_ref().map(|(_, t)| t.as_str())
    }

    pub fn push_log(&mut self, line: impl Into<String>) {
        let clean: String = line.into().chars().filter(|c| !c.is_control()).collect();
        self.log
            .push_back(format!("{} {}", report::now_hms(), clean));
        while self.log.len() > 200 {
            self.log.pop_front();
        }
    }

    fn manual_deadline(&self) -> Option<Instant> {
        let secs = self.config.timeouts.manual_secs;
        (secs > 0).then(|| Instant::now() + Duration::from_secs(secs))
    }

    fn drain_file_pick(&mut self) {
        let picked = self
            .ship
            .file_pick_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some((is_cred, res)) = picked {
            self.ship.file_picking = false;
            self.ship.file_pick_rx = None;
            match res {
                Ok(Some(path)) => ui::panel_ship::import_picked_file(self, is_cred, &path),
                Ok(None) => {}
                Err(e) => {
                    self.ship.note = e.clone();
                    self.push_log(e);
                }
            }
        }
    }

    fn drain_dev_events(&mut self) {
        while let Ok(next) = self.dev_rx.try_recv() {
            if next == self.dev {
                continue;
            }
            let was_online = matches!(self.dev, DevState::Online(_));
            self.dev = next.clone();
            match next {
                DevState::Multiple => {
                    self.push_log("检测到多台设备，请只连接一台被测耳机");
                }
                DevState::Online(sn) => {
                    self.time_synced = false;
                    self.push_log(format!("设备已连接: {sn}"));
                    if self.config.is_ship_station() {
                        // 出厂写号工位：不自动跑测试；若空闲则复位上一台的写号结论，待扫新码
                        if !self.ship.busy && self.ship.last_pass.is_some() {
                            self.clear_ship_unit_state();
                            self.push_log("新机接入，已清空上一台写号结果，请扫码");
                        }
                    } else if !self.busy {
                        self.clear_unit_state();
                        self.push_log("已清空上一台显示");
                        if self.config.conn.auto_start {
                            let items = self.batch_items();
                            if !items.is_empty() {
                                self.push_log("自动开始全测");
                                self.start_run(items);
                            }
                        }
                    }
                }
                DevState::Disconnected => {
                    self.time_synced = false;
                    if was_online {
                        self.push_log("设备已拔出");
                        let shipping = self.ship.busy;
                        let shielded =
                            shipping && (self.ship.rebooting || provision::is_rebooting());
                        if shipping && !shielded {
                            self.request_ship_stop(StopReason::DeviceGone);
                        } else if self.busy {
                            self.request_stop(StopReason::DeviceGone);
                        }
                    }
                }
            }
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        if let Some(rx) = &self.event_rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
        }
        for ev in events {
            self.handle_event(ev, ctx);
        }
    }

    fn handle_event(&mut self, ev: UiEvent, ctx: &egui::Context) {
        match ev {
            UiEvent::Started => {}
            UiEvent::Current(item) => self.current = Some(item),
            UiEvent::Result(r, dev) => {
                if let Some(d) = dev {
                    self.device.merge(d);
                }
                if let Some(p) = &mut self.progress {
                    p.done += 1;
                }
                if r.item == TestItem::Keys {
                    self.keys_prompt = None;
                }
                if matches!(
                    r.item,
                    TestItem::Led | TestItem::MicSpeaker | TestItem::Video | TestItem::Photo
                ) {
                    self.manual = None;
                }
                if let Some(slot) = self.results.iter_mut().find(|s| s.item == r.item) {
                    *slot = r;
                }
            }
            UiEvent::Manual {
                title,
                question,
                in_media,
            } => {
                if let Some(reply) = self.manual_tx.clone() {
                    attention(ctx);
                    self.manual = Some(ManualAsk {
                        title,
                        question,
                        in_media,
                        deadline: self.manual_deadline(),
                        reply,
                    });
                }
            }
            UiEvent::KeysUpdate { detected, awaiting } => {
                if let Some(reply) = self.manual_tx.clone() {
                    if awaiting {
                        attention(ctx);
                    }
                    self.keys_prompt = Some(KeysPrompt {
                        detected,
                        awaiting,
                        deadline: self.manual_deadline(),
                        reply,
                    });
                }
            }
            UiEvent::PhotoReady(path) => match load_texture(ctx, &path) {
                Ok((texture, size)) => self.media = MediaState::Photo { texture, size },
                Err(e) => self.push_log(format!("图片解码失败: {e:#}")),
            },
            UiEvent::VideoReady {
                frames_dir,
                count,
                has_audio,
                audio,
            } => {
                let mut names: Vec<PathBuf> = match std::fs::read_dir(&frames_dir) {
                    Ok(rd) => rd
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
                        .collect(),
                    Err(_) => Vec::new(),
                };
                names.sort();
                let mut frames = Vec::new();
                let mut sizes = Vec::new();
                for path in names {
                    match load_texture(ctx, &path) {
                        Ok((t, s)) => {
                            frames.push(t);
                            sizes.push(s);
                        }
                        Err(_) => continue,
                    }
                }
                if frames.is_empty() {
                    self.push_log(format!("视频帧加载失败（期望 {count} 帧）"));
                } else {
                    self.stop_audio();
                    if let Some(wav) = audio {
                        self.play_audio(&wav);
                    }
                    self.media = MediaState::Video {
                        frames,
                        sizes,
                        idx: 0,
                        last: Instant::now(),
                        has_audio,
                    };
                }
            }
            UiEvent::Log(s) => self.push_log(s),
            UiEvent::TimeSynced => self.time_synced = true,
            UiEvent::Finished { report } => {
                self.busy = false;
                self.current = None;
                self.event_rx = None;
                self.manual_tx = None;
                self.progress = None;
                self.keys_prompt = None;
                self.manual = None;
                self.push_log("序列结束");
                self.last_report = report.clone();
                match report {
                    Some(p) => self.push_log(format!("报告: {}", p.display())),
                    None => self.push_log("报告写入失败"),
                }
                self.refresh_stats();
                self.refresh_reports();
            }
        }
    }

    fn tick_video(&mut self, ctx: &egui::Context) {
        if let MediaState::Video {
            frames, idx, last, ..
        } = &mut self.media
        {
            if last.elapsed() >= PLAYBACK_INTERVAL {
                *idx = (*idx + 1) % frames.len().max(1);
                *last = Instant::now();
            }
            ctx.request_repaint();
        }
    }
}

fn install_cjk_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("droid-cjk".into(), egui::FontData::from_static(CJK_FONT));
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "droid-cjk".into());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("droid-cjk".into());
    ctx.set_fonts(fonts);
}

fn load_texture(
    ctx: &egui::Context,
    path: &Path,
) -> anyhow::Result<(egui::TextureHandle, [f32; 2])> {
    let img = image::open(path)?;
    let size = [img.width() as f32, img.height() as f32];
    let rgba = img.to_rgba8();
    let pixels = egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        &rgba,
    );
    let texture = ctx.load_texture(
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "media".into()),
        pixels,
        egui::TextureOptions::default(),
    );
    Ok((texture, size))
}

impl eframe::App for E02App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.style().visuals.window_fill != ui::theme::CANVAS {
            ui::theme::apply(ctx);
        }
        if self.busy || self.ship.busy {
            ctx.request_repaint();
        }
        self.drain_file_pick();
        self.drain_ship_events(ctx);
        self.drain_events(ctx);
        self.drain_dev_events();
        self.tick_video(ctx);

        // 出厂写号工位下支持拖拽 .txt/.csv 文件进窗口直接校验导入
        if self.config.is_ship_station() && !self.ship.busy {
            let dropped: Vec<PathBuf> = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect()
            });
            for path in dropped {
                ui::panel_ship::import_dropped_file(self, &path);
            }
        }

        let is_ship = self.config.is_ship_station();

        egui::TopBottomPanel::top("top_bar")
            .frame(
                egui::Frame::default()
                    .fill(ui::theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        egui::RichText::new("E02 工厂测试")
                            .strong()
                            .size(18.0)
                            .color(ui::theme::INK),
                    );
                    ui::theme::badge(
                        ui,
                        &format!(
                            "{} · {}",
                            Config::station_label(&self.config.station),
                            self.model.label()
                        ),
                        ui::theme::INK,
                        ui::theme::SELECTED,
                    );

                    // 顶栏进度条：测试工位与出厂写号工位共用同一组件
                    let active_prog = if self.busy {
                        self.progress.as_ref().map(|p| {
                            (
                                p,
                                self.current
                                    .map(|i| i.label().to_string())
                                    .unwrap_or_default(),
                            )
                        })
                    } else if self.ship.busy {
                        self.ship.progress.as_ref().map(|p| {
                            (
                                p,
                                self.ship
                                    .current
                                    .map(|s| s.label().to_string())
                                    .unwrap_or_default(),
                            )
                        })
                    } else {
                        None
                    };
                    if let Some((p, cur_label)) = active_prog {
                        let frac = p.done as f32 / p.total.max(1) as f32;
                        let elapsed = p.started.elapsed();
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .desired_width(170.0)
                                    .desired_height(10.0)
                                    .fill(ui::theme::PRIMARY),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}/{} {} {:02}:{:02}",
                                    p.done,
                                    p.total,
                                    cur_label,
                                    elapsed.as_secs() / 60,
                                    elapsed.as_secs() % 60
                                ))
                                .small()
                                .color(ui::theme::BODY),
                            );
                        });
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.ship.busy {
                            if ui.add(ui::theme::danger("停止写号")).clicked() {
                                self.request_ship_stop(StopReason::User);
                            }
                        } else if self.busy && ui.add(ui::theme::danger("停止测试")).clicked() {
                            self.request_stop(StopReason::User);
                        }
                        if ui.button("报告中心").clicked() {
                            self.refresh_stats();
                            self.refresh_ship_stats();
                            self.refresh_reports();
                            self.report_open = true;
                        }
                        if ui
                            .add_enabled(!self.busy && !self.ship.busy, egui::Button::new("设置"))
                            .on_disabled_hover_text("测试或写号进行中，请先等待结束或停止")
                            .clicked()
                        {
                            self.lock_admin();
                            self.binding_export_note.clear();
                            self.ledger_view =
                                crate::ledger::Ledger::load(&crate::ledger::path_of(&self.config));
                            self.reload_fourg_template_buf();
                            self.settings_open = true;
                        }

                        ui.add_space(10.0);
                        let (dot_color, text) = match &self.dev {
                            DevState::Disconnected => (ui::theme::MUTED, "设备未连接".to_string()),
                            DevState::Online(sn) => (ui::theme::OK, format!("设备 {sn}")),
                            DevState::Multiple => (ui::theme::WARN, "多台设备".to_string()),
                        };
                        ui.label(egui::RichText::new(text).small().color(ui::theme::BODY));
                        ui::theme::dot(ui, dot_color);
                    });
                });
            });

        egui::TopBottomPanel::bottom("log_panel")
            .default_height(140.0)
            .resizable(true)
            .frame(
                egui::Frame::default()
                    .fill(ui::theme::DARK)
                    .inner_margin(egui::Margin::symmetric(16.0, 8.0)),
            )
            .show(ctx, |ui| ui::log_panel::show(ui, self));

        if is_ship {
            egui::SidePanel::left("ship_left_panel")
                .resizable(false)
                .exact_width(240.0)
                .frame(
                    egui::Frame::default()
                        .fill(ui::theme::CANVAS)
                        .inner_margin(egui::Margin::symmetric(14.0, 12.0)),
                )
                .show(ctx, |ui| ui::panel_ship::show_left(ui, self));
            egui::SidePanel::right("ship_right_panel")
                .resizable(false)
                .exact_width(300.0)
                .frame(
                    egui::Frame::default()
                        .fill(ui::theme::CANVAS)
                        .inner_margin(egui::Margin::symmetric(14.0, 12.0)),
                )
                .show(ctx, |ui| ui::panel_ship::show_right(ui, self));
            egui::CentralPanel::default()
                .frame(egui::Frame::default().fill(ui::theme::CANVAS))
                .show(ctx, |ui| ui::panel_ship::show_center(ui, self));
            ui::panel_ship::show_stock_dialog(ctx, self);
            ui::panel_settings::show(ctx, self);
            ui::dialog_report::show(ctx, self);
            return;
        }

        egui::SidePanel::left("test_panel")
            .resizable(false)
            .exact_width(240.0)
            .frame(
                egui::Frame::default()
                    .fill(ui::theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(14.0, 12.0)),
            )
            .show(ctx, |ui| ui::panel_tests::show(ui, self));
        egui::SidePanel::right("device_panel")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                egui::Frame::default()
                    .fill(ui::theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(14.0, 12.0)),
            )
            .show(ctx, |ui| ui::panel_device::show(ui, self));
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(ui::theme::DARK))
            .show(ctx, |ui| ui::panel_media::show(ui, self));

        ui::dialog_keys::show(ctx, self);
        ui::dialog_manual::show(ctx, self);
        ui::panel_settings::show(ctx, self);
        ui::dialog_report::show(ctx, self);
    }
}

fn attention(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
        egui::UserAttentionType::Critical,
    ));
}
