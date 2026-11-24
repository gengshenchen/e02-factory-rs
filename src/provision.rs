//! 出厂写号引擎（FQC §3）：worker 线程阻塞执行 ⓪–⑫ 流程，
//! 经 [`ShipEvent`] 回传状态；确认闸门阻塞等待 [`ShipAnswer`]。
//!
//! 与测试序列（worker.rs）同构但独立通道：出厂页步骤行/确认卡/大号
//! PASS/FAIL 都由本模块的事件驱动；⑪ `adb reboot` 前发 [`ShipEvent::Rebooting`]
//! 让 UI 屏蔽 devwatch 的拔出中止（重启是预期行为不是拔线）。
//!
//! 失败隔离：⑧⑨ 任一步失败立即停、判 FAIL、**不执行 cf4**（不切正式版，
//! 留产测态待返修）；凭据保持 allocated（已预落盘绑当前 SN，不会发给
//! 下一台），重跑同 SN 自动复原。

use crate::adb;
use crate::config::{Config, FourGCfg, GateMode, ModelKind, ProvisionProfile};
use crate::exec;
use crate::ledger::{BindCheck, Ledger, ShipMeta};
use crate::protocol::{self, CameraFactory, DeviceInfo};
use crate::report;
use crate::stock::{self, clip_secret, CredEntry, EntryStatus, ProvisionStock};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime};

/// cf30 写入的 ini 节名（2026-11-23 真机实测样例 `[ProvisionInfo]`）。
const SECTION: &str = "ProvisionInfo";

/// 4G 写入事务整体上限（FQC §3.1：含整轮重试的兜底墙钟）。
const FOURG_TOTAL_BUDGET: Duration = Duration::from_secs(60);

/// ⑪/⑫ 预期重启窗口同步原子态：发 `adb reboot` 前由 worker 同步置 true，
/// `run_ship` 退出（含 panic）经 RAII Guard 自动复位——彻底消灭 UI 事件队列
/// 消费晚于 `devwatch` 断连事件的帧内竞态。
static REBOOTING: AtomicBool = AtomicBool::new(false);

pub fn is_rebooting() -> bool {
    REBOOTING.load(Ordering::Acquire)
}

struct RebootGuard;

impl Drop for RebootGuard {
    fn drop(&mut self) {
        REBOOTING.store(false, Ordering::Release);
    }
}

/// 出厂页右侧“被测设备”实时快照（随步骤推进逐步充实）。
#[derive(Debug, Clone, Default)]
pub struct ShipDeviceView {
    pub target_sn: String,
    pub orig_sn: String,
    pub bt_mac: String,
    pub wifi_mac: String,
    pub imei: String,
    pub ccid: String,
    pub t32_ver: String,
    pub bt_ver: String,
    pub hardware_ver: String,
    pub battery_volts: Option<f32>,
}

/// 出厂页 ↔ worker 事件。
#[derive(Debug)]
pub enum ShipEvent {
    Started,
    /// 步骤开始（进度指示）
    Current(ShipStep),
    /// 步骤结论（出厂页步骤行）
    StepResult(ShipStepResult),
    /// 设备实时字段更新（驱动右侧被测设备栏）
    Device(ShipDeviceView),
    /// 确认卡（worker 阻塞等待 [`ShipAnswer::Confirm`]）
    Confirm(ConfirmCard),
    /// ⑪ 即将 `adb reboot`：UI 屏蔽 devwatch 拔出中止，直到 Finished
    Rebooting,
    Log(String),
    /// 流程结束（附 ship 报告路径与最终判定）
    Finished {
        report: Option<PathBuf>,
        pass: bool,
    },
}

/// 确认卡应答（UI → worker）。
#[derive(Debug)]
pub enum ShipAnswer {
    Confirm(bool),
}

/// 流程步骤（⓪–⑫ 映射；⑥ 确认闸门是对话不是步骤行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShipStep {
    /// ⓪ 身份与现值采集（cf10/cf3/cf20[eSIM]/cat config_path）
    Identity,
    /// ① 整机闸门（gate_mode）
    Gate,
    /// ②③ SN 校验 + 台账反查 + SN 池
    SnCheck,
    /// ④ 版本核对（cf3 soft-ware / cf7 bt）
    Versions,
    /// ⑤ 硬件终检（cf12 电池 / cf8 SD）
    Hardware,
    /// ⑦ 凭据领用与预落盘
    Claim,
    /// ⑧ cf30 逐键写
    Write,
    /// ⑨ 4G 模组配置写入（eSIM）
    FourG,
    /// ⑩ 终检（cat 复核全部目标键）
    Verify,
    /// ⑪ 对时 + cf4 切正式版 + 重启
    Finalize,
    /// ⑫ 重启后探测
    Probe,
}

impl ShipStep {
    /// 出厂页步骤行标签（顺序即展示顺序）。
    pub fn label(self) -> &'static str {
        match self {
            ShipStep::Identity => "设备信息",
            ShipStep::Gate => "整机校验",
            ShipStep::SnCheck => "SN 校验",
            ShipStep::Versions => "版本核对",
            ShipStep::Hardware => "电量与存储",
            ShipStep::Claim => "分配凭据",
            ShipStep::Write => "写入配置",
            ShipStep::FourG => "4G 参数",
            ShipStep::Verify => "回读校验",
            ShipStep::Finalize => "切换出厂",
            ShipStep::Probe => "重启回读",
        }
    }

    /// 全部步骤（UI 初始化行序）。
    pub const ALL: [ShipStep; 11] = [
        ShipStep::Identity,
        ShipStep::Gate,
        ShipStep::SnCheck,
        ShipStep::Versions,
        ShipStep::Hardware,
        ShipStep::Claim,
        ShipStep::Write,
        ShipStep::FourG,
        ShipStep::Verify,
        ShipStep::Finalize,
        ShipStep::Probe,
    ];

    /// 该步骤是否属于指定产品型号（E02 WiFi 型无 4G 模组，隐藏并跳过 FourG）。
    pub fn available_for(self, model: ModelKind) -> bool {
        match self {
            ShipStep::FourG => !model.has_wifi(),
            _ => true,
        }
    }

    /// 指定型号下的有效流程步骤序列。
    pub fn steps_for(model: ModelKind) -> Vec<ShipStep> {
        Self::ALL
            .into_iter()
            .filter(|s| s.available_for(model))
            .collect()
    }
}

/// 步骤判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepVerdict {
    Pending,
    Pass,
    /// 告警但可继续（闸门 warn 等；计入确认卡）
    Warn,
    Fail,
    Skip,
}

impl StepVerdict {
    /// 报告行文字（与测试报告约定一致：PASS/FAIL/未测/告警/跳过）。
    pub fn label(self) -> &'static str {
        match self {
            StepVerdict::Pending => "未测",
            StepVerdict::Pass => "PASS",
            StepVerdict::Warn => "告警",
            StepVerdict::Fail => "FAIL",
            StepVerdict::Skip => "跳过",
        }
    }

    /// 界面展示用中文词（与 `Verdict::ui_label` 同体系，不混英文 PASS/FAIL）。
    pub fn ui_label(self) -> &'static str {
        match self {
            StepVerdict::Pending => "待测",
            StepVerdict::Pass => "合格",
            StepVerdict::Warn => "告警",
            StepVerdict::Fail => "不合格",
            StepVerdict::Skip => "跳过",
        }
    }
}

/// 单步结论（出厂页步骤行 / ship 报告行）。
#[derive(Debug, Clone)]
pub struct ShipStepResult {
    pub step: ShipStep,
    pub verdict: StepVerdict,
    pub detail: String,
    pub secs: u64,
}

/// 确认卡（⑥：SN、凭据全值、设备现值、告警清单；操作员看全再放行）。
#[derive(Debug, Clone)]
pub struct ConfirmCard {
    pub sn: String,
    pub bt_mac: String,
    pub model: String,
    /// 设备现值（写入键 → 当前值，空 = 未写）
    pub current: Vec<(String, String)>,
    /// 领取/复原的凭据全值（凭据档案才有）
    pub credential: Option<(String, String)>,
    pub warnings: Vec<String>,
    /// true = 同号重写（返修/断电续写）
    pub re_run: bool,
}

/// 出厂流程执行上下文（路径可注入——测试指到临时目录，零副作用）。
pub struct ShipCtx {
    pub cfg: Config,
    pub data_dir: PathBuf,
    pub reports_dir: PathBuf,
    pub stock_path: PathBuf,
    pub ledger_path: PathBuf,
}

impl ShipCtx {
    pub fn new(cfg: &Config, reports_dir: &Path) -> Self {
        Self {
            cfg: cfg.clone(),
            data_dir: crate::paths::data_dir().to_path_buf(),
            reports_dir: reports_dir.to_path_buf(),
            stock_path: stock::stock_path(),
            ledger_path: crate::ledger::path_of(cfg),
        }
    }
}

/// 一轮出厂写号。返回 ship 报告路径。
/// `serial` = 开始时在线的被测设备序列号（adb 定向，与测试序列同策略）。
pub fn run_ship(
    scanned_sn: &str,
    serial: Option<String>,
    tx: &Sender<ShipEvent>,
    answers: &Receiver<ShipAnswer>,
    ctx: &ShipCtx,
) -> Option<PathBuf> {
    exec::clear_abort();
    REBOOTING.store(false, Ordering::Release);
    let _reboot_guard = RebootGuard;
    let _target = adb::TargetGuard::new(serial.clone());
    let prev_timeout = exec::default_timeout();
    exec::set_default_timeout(timeout_opt(ctx.cfg.timeouts.item_secs));
    let _ = tx.send(ShipEvent::Started);
    let model = ctx.cfg.model.unwrap_or_default();
    let profile = ctx.cfg.provision.for_model(model).clone();
    let mut run = ShipRun {
        tx,
        answers,
        ctx,
        profile,
        model,
        serial,
        sn: scanned_sn.trim().to_uppercase(),
        bt_mac: String::new(),
        device: DeviceInfo::default(),
        current: BTreeMap::new(),
        warnings: Vec::new(),
        re_run: false,
        cred: None,
        results: Vec::new(),
        battery_volts: None,
        bt_ver_seen: String::new(),
        fourg_written: None,
        fourg_diff: Vec::new(),
        started_wall: SystemTime::now(),
        started: Instant::now(),
    };
    run.emit_device();
    let gate_name = match ctx.cfg.provision.gate_mode {
        GateMode::Strict => "强制拦截",
        GateMode::Warn => "提示确认",
        GateMode::Off => "不校验",
    };
    run.log(format!(
        "开始写号：SN {}，型号 {}，整机校验：{}",
        run.sn,
        run.model.label(),
        gate_name,
    ));
    let fail_detail = run.flow();
    let pass = fail_detail.is_none();
    let report = run.finish(pass, fail_detail.as_deref());
    exec::set_default_timeout(prev_timeout);
    let _ = tx.send(ShipEvent::Finished {
        report: report.clone(),
        pass,
    });
    report
}

/// 流程体：每步失败/取消返回 Some(原因)。
/// 步骤方法约定：返回 `None` = 继续；`Some(失败原因)` = 中止判 FAIL。
/// （不能用 `?` 串接——`?` 恰好会把 None 当提前返回，首个端到端测试
/// 就踩过：跑完身份采集直接"PASS"。）
fn flow_inner(run: &mut ShipRun) -> Option<String> {
    if let Some(fail) = run.identity() {
        return Some(fail);
    }
    if let Some(fail) = run.gate() {
        return Some(fail);
    }
    if let Some(fail) = run.sn_checks() {
        return Some(fail);
    }
    if let Some(fail) = run.versions() {
        return Some(fail);
    }
    if let Some(fail) = run.hardware() {
        return Some(fail);
    }
    if let Some(fail) = run.confirm_gate() {
        return Some(fail);
    }
    if let Some(fail) = run.claim() {
        return Some(fail);
    }
    if let Some(fail) = run.write_keys() {
        return Some(fail);
    }
    if !run.model.has_wifi() {
        if let Some(fail) = run.fourg() {
            return Some(fail);
        }
    }
    if let Some(fail) = run.verify() {
        return Some(fail);
    }
    if let Some(fail) = run.finalize() {
        return Some(fail);
    }
    run.probe()
}

/// 单轮状态（步骤方法共用的可变上下文）。
struct ShipRun<'a> {
    tx: &'a Sender<ShipEvent>,
    answers: &'a Receiver<ShipAnswer>,
    ctx: &'a ShipCtx,
    profile: ProvisionProfile,
    model: ModelKind,
    serial: Option<String>,
    /// 扫码 SN（清洗后）
    sn: String,
    bt_mac: String,
    device: DeviceInfo,
    /// 写入目标键的设备现值（cat/cf30 回显维护）
    current: BTreeMap<String, String>,
    warnings: Vec<String>,
    re_run: bool,
    /// 领取/复原的凭据
    cred: Option<CredEntry>,
    results: Vec<ShipStepResult>,
    battery_volts: Option<f32>,
    bt_ver_seen: String,
    /// 4G 写入行数
    fourg_written: Option<usize>,
    fourg_diff: Vec<String>,
    started_wall: SystemTime,
    started: Instant,
}

impl ShipRun<'_> {
    fn log(&self, s: impl Into<String>) {
        let _ = self.tx.send(ShipEvent::Log(s.into()));
    }

    fn emit_device(&self) {
        let orig_sn = self
            .current
            .get("sn")
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| self.device.sn.clone());
        let _ = self.tx.send(ShipEvent::Device(ShipDeviceView {
            target_sn: self.sn.clone(),
            orig_sn,
            bt_mac: self.bt_mac.clone(),
            wifi_mac: self.device.wifi_mac.clone(),
            imei: self.device.imei.clone(),
            ccid: self.device.ccid.clone(),
            t32_ver: self.device.t32_ver.clone(),
            bt_ver: self.bt_ver_seen.clone(),
            hardware_ver: self.device.hardware_ver.clone(),
            battery_volts: self.battery_volts,
        }));
    }

    /// 步骤开始 + 返回计时起点。
    fn begin(&self, step: ShipStep) -> Instant {
        let _ = self.tx.send(ShipEvent::Current(step));
        Instant::now()
    }

    /// 步骤结论（广播 + 留档进 ship 报告）。
    fn done(
        &mut self,
        step: ShipStep,
        verdict: StepVerdict,
        detail: impl Into<String>,
        t0: Instant,
    ) {
        let r = ShipStepResult {
            step,
            verdict,
            detail: detail.into(),
            secs: t0.elapsed().as_secs(),
        };
        self.log(format!(
            "【{}】{} {}",
            step.label(),
            r.verdict.label(),
            clip(&r.detail, 240)
        ));
        let _ = self.tx.send(ShipEvent::StepResult(r.clone()));
        self.results.push(r);
    }

    fn flow(&mut self) -> Option<String> {
        flow_inner(self)
    }

    // —— ⓪ 身份与现值采集 ——

    fn identity(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Identity);
        // cf10 蓝牙 MAC（出厂身份键：台账/报告/凭据绑定）
        let mac_cmd = CameraFactory::BtMac.device_cmd();
        let mac = self
            .shell(&mac_cmd)
            .and_then(|o| parse_dev(&o))
            .map_err(|e| format!("cf10 蓝牙 MAC 读取失败: {e}"));
        let d = match mac {
            Ok(d) => d,
            Err(e) => return self.fail_step(ShipStep::Identity, e, t0),
        };
        self.device.merge(d);
        self.bt_mac = self.device.bt_mac.clone();
        self.emit_device();
        if self.bt_mac.is_empty() {
            return self.fail_step(
                ShipStep::Identity,
                "cf10 未读取到蓝牙 MAC，无法写入".into(),
                t0,
            );
        }
        // cf3 设备信息（SN/WiFi MAC/软件版本）：失败或版本为空立即拦，不带残缺身份放行
        let info_cmd = CameraFactory::DeviceInfo.device_cmd();
        match self.shell(&info_cmd).and_then(|o| parse_dev(&o)) {
            Ok(d) => self.device.merge(d),
            Err(e) => {
                self.emit_device();
                return self.fail_step(
                    ShipStep::Identity,
                    format!("cf3 设备信息读取失败: {e}"),
                    t0,
                );
            }
        }
        self.emit_device();
        if self.device.t32_ver.trim().is_empty() {
            return self.fail_step(
                ShipStep::Identity,
                "cf3 未返回软件版本（soft-ware 为空）".into(),
                t0,
            );
        }
        if let Some(det) = ModelKind::detect(&self.device.t32_ver) {
            if det != self.model {
                let w = format!(
                    "型号疑似不符：固件 [{}] 前缀为 {}，当前设置型号为 {}",
                    self.device.t32_ver,
                    det.label(),
                    self.model.label()
                );
                self.log(w.clone());
                self.warnings.push(w);
            }
        }
        // eSIM 型：cf20 读 IMEI/CCID（出货明细与台账绑定对必填）
        if !self.model.has_wifi() {
            let esim_cmd = CameraFactory::CcidImei.device_cmd();
            match self.shell(&esim_cmd).and_then(|o| parse_dev(&o)) {
                Ok(d) => self.device.merge(d),
                Err(e) => self.log(format!(
                    "cf20 IMEI/CCID 读取异常（尝试从配置文件兜底）: {e}"
                )),
            }
        }
        // 读设备端配置现值（写入键的目标区）
        let text = match self.cat_config() {
            Ok(t) => t,
            Err(e) => {
                self.emit_device();
                return self.fail_step(ShipStep::Identity, e, t0);
            }
        };
        for key in self.profile.keys.clone() {
            let v = protocol::parse_cf30_response(&text, SECTION, &key)
                .echo_value
                .unwrap_or_default();
            self.current.insert(key, v);
        }
        if !self.model.has_wifi() {
            if self.device.imei.trim().is_empty() {
                if let Some(v) = protocol::parse_cf30_response(&text, SECTION, "imei").echo_value {
                    if !v.is_empty() {
                        self.device.imei = v;
                    }
                }
            }
            if self.device.ccid.trim().is_empty() {
                if let Some(v) = protocol::parse_cf30_response(&text, SECTION, "ccid").echo_value {
                    if !v.is_empty() {
                        self.device.ccid = v.to_uppercase();
                    }
                }
            }
        }
        self.emit_device();
        // 按型号校验硬件绑定值（E02W=WiFi MAC；E02E=IMEI+CCID）
        let partner = if self.model.has_wifi() {
            if self.device.wifi_mac.trim().is_empty() {
                return self.fail_step(
                    ShipStep::Identity,
                    "未读取到 WiFi MAC（E02W 必填，禁止写入）".into(),
                    t0,
                );
            }
            self.device.wifi_mac.clone()
        } else {
            if !protocol::imei_valid(&self.device.imei) {
                return self.fail_step(
                    ShipStep::Identity,
                    format!(
                        "E02E 未读到有效 IMEI（当前值 {:?}），禁止写入",
                        self.device.imei
                    ),
                    t0,
                );
            }
            if !protocol::ccid_valid(&self.device.ccid) {
                return self.fail_step(
                    ShipStep::Identity,
                    format!(
                        "E02E 未读到有效 CCID（当前值 {:?}），禁止写入",
                        self.device.ccid
                    ),
                    t0,
                );
            }
            self.device.imei.clone()
        };
        // 核对硬件地址绑定记录，防止烧重 MAC 的设备覆盖已有记录
        let ledger = Ledger::load(&self.ctx.ledger_path);
        if let Some(conflict) = ledger.check_pair(&self.bt_mac, &partner) {
            let msg = match conflict {
                BindCheck::BtConflict { other } => format!(
                    "硬件地址冲突：蓝牙 {} 已绑定 {}（首次记录 {}），当前设备为 {partner}，禁止写入",
                    self.bt_mac, other.partner, other.first_seen
                ),
                BindCheck::PartnerConflict { other } => format!(
                    "硬件地址冲突：绑定值 {partner} 已归属蓝牙 {}（首次记录 {}），禁止写入",
                    other.bt_mac, other.first_seen
                ),
                _ => unreachable!(),
            };
            return self.fail_step(ShipStep::Identity, msg, t0);
        }
        let sn_now = self.current.get("sn").cloned().unwrap_or_default();
        self.done(
            ShipStep::Identity,
            StepVerdict::Pass,
            format!(
                "蓝牙 {}，固件 [{}]，当前 SN={}",
                self.bt_mac,
                self.device.t32_ver,
                if sn_now.is_empty() {
                    "（空）".into()
                } else {
                    sn_now
                }
            ),
            t0,
        );
        None
    }

    // —— ① 整机测试前置校验 ——

    fn gate(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Gate);
        let mode = self.ctx.cfg.provision.gate_mode;
        if mode == GateMode::Off {
            self.done(ShipStep::Gate, StepVerdict::Skip, "已设置为不校验", t0);
            return None;
        }
        let ledger = Ledger::load(&self.ctx.ledger_path);
        if ledger.complete_pass(&self.bt_mac) {
            self.done(ShipStep::Gate, StepVerdict::Pass, "整机工位测试已合格", t0);
            return None;
        }
        let msg = "未查到本机整机工位全项合格记录";
        match mode {
            GateMode::Strict => self.fail_step(
                ShipStep::Gate,
                format!("{msg}，禁止写入（请先完成整机工位测试）"),
                t0,
            ),
            _ => {
                self.warnings
                    .push(format!("整机校验提醒：{msg}（如已用旧工具测完可确认继续）"));
                self.done(ShipStep::Gate, StepVerdict::Warn, msg, t0);
                None
            }
        }
    }

    // —— ②③ SN 校验 + 绑定记录反查 + SN 批次池 ——

    fn sn_checks(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::SnCheck);
        let re = match regex_lite::Regex::new(&self.profile.sn_pattern) {
            Ok(re) => re,
            Err(e) => {
                return self.fail_step(
                    ShipStep::SnCheck,
                    format!("设置中的 SN 格式正则无效: {e}"),
                    t0,
                )
            }
        };
        // ②-1 格式
        if !re.is_match(&self.sn) {
            return self.fail_step(
                ShipStep::SnCheck,
                format!(
                    "SN 格式不匹配（规则 {}）: {}",
                    self.profile.sn_pattern, self.sn
                ),
                t0,
            );
        }
        // ②-2 设备已有 SN 冲突校验
        let cfg_sn = self.current.get("sn").cloned().unwrap_or_default();
        let existing = if cfg_sn.is_empty() {
            self.device.sn.clone()
        } else {
            cfg_sn
        }
        .to_uppercase();
        let placeholders = &self.ctx.cfg.provision.placeholder_sns;
        let is_placeholder = existing.is_empty()
            || placeholders
                .iter()
                .any(|p| p.eq_ignore_ascii_case(&existing));
        let is_formal = |s: &str| !s.is_empty() && re.is_match(s);
        if !is_placeholder && is_formal(&existing) {
            if existing != self.sn {
                return self.fail_step(
                    ShipStep::SnCheck,
                    format!(
                        "设备已有其他正式 SN {existing}，与目标 {} 冲突，禁止写入",
                        self.sn
                    ),
                    t0,
                );
            }
            self.re_run = true;
            self.warnings.push(format!(
                "同 SN 重写：设备当前已写入 {existing}（返修或重跑场景）"
            ));
        }
        // ②-3 绑定记录反查
        let ledger = Ledger::load(&self.ctx.ledger_path);
        if let Some(rec) = ledger.find_by_shipped_sn(&self.sn) {
            if !rec.bt_mac.eq_ignore_ascii_case(&self.bt_mac) {
                return self.fail_step(
                    ShipStep::SnCheck,
                    format!(
                        "SN {} 已绑定其他设备 {}（写入时间 {}），不可重复使用",
                        self.sn, rec.bt_mac, rec.shipped_at
                    ),
                    t0,
                );
            }
            if !self.re_run {
                self.re_run = true;
                self.warnings.push(format!(
                    "同 SN 重写：本机此前已写入 SN {}（{}）",
                    self.sn, rec.shipped_at
                ));
            } else {
                self.log(format!(
                    "记录核对：SN {} 曾于 {} 写入本机",
                    self.sn, rec.shipped_at
                ));
            }
        }
        // ③ SN 批次池（已导入才启用）
        let stock = ProvisionStock::load(&self.ctx.stock_path);
        let note = if stock.has_sn_pool() {
            match stock.sn_entry(&self.sn) {
                None => {
                    return self.fail_step(
                        ShipStep::SnCheck,
                        format!("SN {} 不在已导入批次中，请核对批次文件", self.sn),
                        t0,
                    )
                }
                Some(e) if e.status == EntryStatus::Voided => {
                    return self.fail_step(
                        ShipStep::SnCheck,
                        format!("SN {} 已标记作废", self.sn),
                        t0,
                    )
                }
                Some(e)
                    if !e.bound_bt_mac.is_empty()
                        && !e.bound_bt_mac.eq_ignore_ascii_case(&self.bt_mac) =>
                {
                    return self.fail_step(
                        ShipStep::SnCheck,
                        format!("SN {} 已被设备 {} 占用", self.sn, e.bound_bt_mac),
                        t0,
                    )
                }
                Some(e) => format!("（批次状态：{}）", e.status.label()),
            }
        } else {
            "（未导入 SN 批次，仅校验格式与记录）".to_string()
        };
        let v = if self.re_run {
            StepVerdict::Warn
        } else {
            StepVerdict::Pass
        };
        self.done(ShipStep::SnCheck, v, format!("校验通过{note}"), t0);
        None
    }

    // —— ④ 版本核对 ——

    fn versions(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Versions);
        if self.profile.t32_ver.trim().is_empty() {
            return self.fail_step(
                ShipStep::Versions,
                "请先在设置中填写工单 T32 软件版本".into(),
                t0,
            );
        }
        if self.device.t32_ver.trim() != self.profile.t32_ver.trim() {
            return self.fail_step(
                ShipStep::Versions,
                format!(
                    "T32 软件版本不符：设备 [{}] ≠ 工单 [{}]",
                    self.device.t32_ver, self.profile.t32_ver
                ),
                t0,
            );
        }
        if self.profile.bt_ver.trim().is_empty() {
            return self.fail_step(
                ShipStep::Versions,
                "请先在设置中填写工单蓝牙固件版本".into(),
                t0,
            );
        }
        let bt_cmd = CameraFactory::BtVersion.device_cmd();
        let bt = match self.shell(&bt_cmd) {
            Ok(o) => match protocol::parse_bt_version(&o) {
                Some(v) => v,
                None => {
                    return self.fail_step(
                        ShipStep::Versions,
                        format!("cf7 未解析到蓝牙版本: {}", clip(&o, 80)),
                        t0,
                    )
                }
            },
            Err(e) => return self.fail_step(ShipStep::Versions, format!("cf7 执行失败: {e}"), t0),
        };
        self.bt_ver_seen = bt.clone();
        self.emit_device();
        if bt.trim() != self.profile.bt_ver.trim() {
            return self.fail_step(
                ShipStep::Versions,
                format!(
                    "蓝牙固件版本不符：设备 [{}] ≠ 工单 [{}]",
                    bt, self.profile.bt_ver
                ),
                t0,
            );
        }
        self.done(
            ShipStep::Versions,
            StepVerdict::Pass,
            format!("T32 {}，蓝牙 {}", self.profile.t32_ver, bt),
            t0,
        );
        None
    }

    // —— ⑤ 电量与存储检测 ——

    fn hardware(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Hardware);
        let batt_cmd = CameraFactory::Battery.device_cmd();
        let out = match self.shell(&batt_cmd) {
            Ok(o) => o,
            Err(e) => return self.fail_step(ShipStep::Hardware, format!("cf12 执行失败: {e}"), t0),
        };
        let volts = match protocol::parse_battery_volts(&out) {
            Some(v) => v,
            None => {
                return self.fail_step(
                    ShipStep::Hardware,
                    format!("cf12 未解析到电池电压: {}", clip(&out, 80)),
                    t0,
                )
            }
        };
        self.battery_volts = Some(volts);
        self.emit_device();
        if volts < self.profile.min_batt_volts {
            return self.fail_step(
                ShipStep::Hardware,
                format!(
                    "电池电压过低：{volts:.3}V < 要求 {}V，请充电后重试",
                    self.profile.min_batt_volts
                ),
                t0,
            );
        }
        if self.profile.check_sdcard {
            let sd_cmd = CameraFactory::SdCard.device_cmd();
            let out = match self.shell(&sd_cmd) {
                Ok(o) => o,
                Err(e) => {
                    return self.fail_step(ShipStep::Hardware, format!("cf8 执行失败: {e}"), t0)
                }
            };
            if !protocol::sdcard_ok(&out) {
                return self.fail_step(ShipStep::Hardware, "未检测到 SD 卡".into(), t0);
            }
            self.done(
                ShipStep::Hardware,
                StepVerdict::Pass,
                format!(
                    "电压 {volts:.3}V（要求 ≥ {}V），SD 卡正常",
                    self.profile.min_batt_volts
                ),
                t0,
            );
        } else {
            self.done(
                ShipStep::Hardware,
                StepVerdict::Pass,
                format!(
                    "电压 {volts:.3}V（要求 ≥ {}V，已跳过 SD 卡检测）",
                    self.profile.min_batt_volts
                ),
                t0,
            );
        }
        None
    }

    // —— ⑥ 写入前确认 ——

    fn confirm_gate(&mut self) -> Option<String> {
        let cred = self.cred_preview();
        if self.profile.needs_credentials() {
            let Some((uuid, key)) = &cred else {
                let t0 = self.begin(ShipStep::Claim);
                return self.fail_step(
                    ShipStep::Claim,
                    "无可用凭据，请先在「批次与库存」导入凭据文件".into(),
                    t0,
                );
            };
            for (key_name, target) in [("uuid", uuid), ("auth_key", key)] {
                if let Some(cur) = self.current.get(key_name) {
                    if !cur.is_empty() && cur != target {
                        self.warnings.push(format!(
                            "设备已有不同 {key_name}（当前值 {}），确认后将覆盖",
                            clip_secret(cur)
                        ));
                    }
                }
            }
        }
        let auto = self.ctx.cfg.provision.auto_commit && self.warnings.is_empty();
        if auto {
            self.log("校验无异常，自动开始写入");
            return None;
        }
        let card = ConfirmCard {
            sn: self.sn.clone(),
            bt_mac: self.bt_mac.clone(),
            model: self.model.label().into(),
            current: self
                .profile
                .keys
                .iter()
                .map(|k| {
                    let v = self.current.get(k).cloned().unwrap_or_default();
                    (k.clone(), v)
                })
                .collect(),
            credential: cred,
            warnings: self.warnings.clone(),
            re_run: self.re_run,
        };
        let _ = self.tx.send(ShipEvent::Confirm(card));
        loop {
            match self.answers.recv_timeout(Duration::from_millis(200)) {
                Ok(ShipAnswer::Confirm(true)) => {
                    self.log("操作员已确认，开始写入");
                    return None;
                }
                Ok(ShipAnswer::Confirm(false)) => return Some("已取消写入".into()),
                Err(RecvTimeoutError::Timeout) => {
                    if exec::abort_requested() {
                        return Some(format!("手动停止：{}", exec::abort_reason().text()));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Some("应答通道已关闭（取消）".into())
                }
            }
        }
    }

    /// 确认卡凭据预览（领用前的目标值：优先复原既有分配，否则提示池顶）。
    fn cred_preview(&self) -> Option<(String, String)> {
        if !self.profile.needs_credentials() {
            return None;
        }
        let stock = ProvisionStock::load(&self.ctx.stock_path);
        if let Some(c) = stock.credential_bound_to(&self.sn) {
            return Some((c.uuid.clone(), c.auth_key.clone()));
        }
        stock
            .credentials
            .iter()
            .find(|c| c.status == EntryStatus::Available)
            .map(|c| (c.uuid.clone(), c.auth_key.clone()))
    }

    // —— ⑦ 分配凭据并锁定 ——

    fn claim(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Claim);
        let now = report::time_str(SystemTime::now());
        let mut stock = ProvisionStock::load(&self.ctx.stock_path);
        if self.profile.needs_credentials() && self.cred.is_none() {
            if let Some(c) = stock.credential_bound_to(&self.sn).cloned() {
                self.cred = Some(c);
                self.log(format!(
                    "复用已分配凭据 {}（同 SN 重写）",
                    clip_secret(&self.cred.as_ref().unwrap().uuid)
                ));
            } else {
                match stock.claim_next_credential(&self.sn, &self.bt_mac, &now) {
                    Some(c) => self.cred = Some(c),
                    None => {
                        return self.fail_step(
                            ShipStep::Claim,
                            "无可用凭据，请先在「批次与库存」导入凭据文件".into(),
                            t0,
                        )
                    }
                }
            }
        }
        stock.mark_sn_allocated(&self.sn, &self.bt_mac, &now);
        if let Err(e) = stock.save(&self.ctx.stock_path) {
            return self.fail_step(ShipStep::Claim, format!("保存库存记录失败: {e:#}"), t0);
        }
        let detail = match &self.cred {
            Some(c) => format!("已锁定凭据 {}", clip_secret(&c.uuid)),
            None if stock.has_sn_pool() => "已锁定批次 SN".into(),
            None => "无需分配凭据".into(),
        };
        self.done(ShipStep::Claim, StepVerdict::Pass, detail, t0);
        None
    }

    // —— ⑧ cf30 逐键写 ——

    fn write_keys(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Write);
        if self.profile.keys.is_empty() {
            return self.fail_step(ShipStep::Write, "档案 keys 为空（无写入目标）".into(), t0);
        }
        let file_arg = Path::new(&self.ctx.cfg.provision.config_path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.ctx.cfg.provision.config_path.clone());
        let mut written = 0usize;
        let mut skipped = 0usize;
        for key in self.profile.keys.clone() {
            let value = match self.value_for(&key) {
                Ok(v) => v,
                Err(e) => return self.fail_step(ShipStep::Write, e, t0),
            };
            let cur = self.current.get(&key).cloned().unwrap_or_default();
            if cur == value {
                skipped += 1;
                self.log(format!("cf30 {key} 已一致，跳过（幂等）"));
                continue;
            }
            let cmd = match protocol::cf30_cmd(&file_arg, SECTION, &key, &value) {
                Ok(c) => c,
                Err(e) => {
                    return self.fail_step(ShipStep::Write, format!("cf30 命令构造失败: {e:#}"), t0)
                }
            };
            // 过程日志对 auth_key 脱敏（确认卡与 ship 报告仍留全值）
            if key == "auth_key" {
                self.log(format!("执行 cf30 写 {key} = {}", clip_secret(&value)));
            } else {
                self.log(format!("执行 adb shell {cmd}"));
            }
            let out = match self.shell(&cmd) {
                Ok(o) => o,
                Err(e) => {
                    return self.fail_step(
                        ShipStep::Write,
                        format!("cf30 写 {key} 执行失败: {e}"),
                        t0,
                    )
                }
            };
            let r = protocol::parse_cf30_response(&out, SECTION, &key);
            if r.updated != Some(true) {
                return self.fail_step(
                    ShipStep::Write,
                    format!(
                        "cf30 写 {key} 设备回报失败（config-update:{}，输出 {}）",
                        match r.updated {
                            Some(false) => "false".to_string(),
                            None => "缺失（协议异常）".to_string(),
                            Some(true) => unreachable!(),
                        },
                        clip(&out, 120)
                    ),
                    t0,
                );
            }
            match r.echo_value {
                Some(v) if v == value => {
                    self.current.insert(key, v);
                    written += 1;
                }
                other => {
                    return self.fail_step(
                        ShipStep::Write,
                        format!(
                            "cf30 写 {key} 回显不符：{} ≠ {}",
                            clip_secret(other.as_deref().unwrap_or("（缺失）")),
                            clip_secret(&value)
                        ),
                        t0,
                    )
                }
            }
        }
        self.emit_device();
        self.done(
            ShipStep::Write,
            StepVerdict::Pass,
            format!("{written} 项写入成功，{skipped} 项已一致跳过"),
            t0,
        );
        None
    }

    /// 写入键取值：sn=扫码值；uuid/auth_key=领用凭据；其余拒。
    fn value_for(&self, key: &str) -> Result<String, String> {
        match key {
            "sn" => Ok(self.sn.clone()),
            "uuid" | "auth_key" => match &self.cred {
                Some(c) => Ok(if key == "uuid" {
                    c.uuid.clone()
                } else {
                    c.auth_key.clone()
                }),
                None => Err(format!("键 {key} 需要凭据，但档案未配置凭据池/领取失败")),
            },
            other => Err(format!("档案键 {other} 未知（支持 sn/uuid/auth_key）")),
        }
    }

    // —— ⑨ 4G 模组配置写入（eSIM） ——

    fn fourg(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::FourG);
        let Some(fg) = self.profile.fourg.clone() else {
            self.done(
                ShipStep::FourG,
                StepVerdict::Skip,
                "未配置 4G 参数（跳过）",
                t0,
            );
            return None;
        };
        if !fg.enable {
            self.done(ShipStep::FourG, StepVerdict::Skip, "未启用 4G 参数写入", t0);
            return None;
        }
        if fg.template.trim().is_empty() {
            return self.fail_step(
                ShipStep::FourG,
                "已启用 4G 参数写入，但模板路径为空".into(),
                t0,
            );
        }
        let tpl_path = self.ctx.data_dir.join(&fg.template);
        let text = match std::fs::read_to_string(&tpl_path) {
            Ok(t) => t,
            Err(e) => {
                return self.fail_step(
                    ShipStep::FourG,
                    format!("4G 模板读取失败（{}）: {e}", tpl_path.display()),
                    t0,
                )
            }
        };
        let lines = match parse_fourg_template(&text, &self.sn) {
            Ok(l) => l,
            Err(e) => return self.fail_step(ShipStep::FourG, e, t0),
        };
        // 整体 60s 墙钟上限（含整轮重试；单行严禁原地重试，防 config.tmp 重复行）
        let deadline = Instant::now() + FOURG_TOTAL_BUDGET;
        let mut attempt = 0;
        loop {
            match self.fourg_transaction(&fg, &lines, &deadline) {
                Ok(()) => break,
                Err(e) if attempt == 0 && Instant::now() < deadline => {
                    attempt += 1;
                    self.log(format!("4G 事务失败（{e}），CFGSTART 整轮重试 1 次"));
                }
                Err(e) => {
                    return self.fail_step(
                        ShipStep::FourG,
                        format!(
                            "4G 写入失败（已执行 {} 轮）: {e}——不切正式版，留产测态返修",
                            attempt + 1
                        ),
                        t0,
                    )
                }
            }
        }
        self.fourg_written = Some(lines.len());
        let diff_note = if self.fourg_diff.is_empty() {
            String::new()
        } else {
            format!("，diff: {}", self.fourg_diff.join("；"))
        };
        self.done(
            ShipStep::FourG,
            StepVerdict::Pass,
            format!("写入 {} 行，回读一致{diff_note}", lines.len()),
            t0,
        );
        None
    }

    /// 一轮 4G 事务：PROD=1 → CFGSTART → CFGSET×N → CFGSAVE → CFGREAD，
    /// 尾部无条件 best-effort PROD=0 退产测；回读比对结果存 `fourg_diff`。
    fn fourg_transaction(
        &mut self,
        fg: &FourGCfg,
        lines: &[String],
        deadline: &Instant,
    ) -> Result<(), String> {
        self.fourg_diff.clear(); // 重试轮清掉上一轮的失败 diff
        let phase = (|| -> Result<String, String> {
            self.fourg_at_ok(fg, "AT+PROD=1", 5, deadline)?;
            self.fourg_at_ok(fg, "AT+CFGSTART", 5, deadline)?;
            for l in lines {
                let at = format!("AT+CFGSET=\"{l}\"");
                self.fourg_at_ok(fg, &at, 5, deadline)?;
            }
            self.fourg_at_ok(fg, "AT+CFGSAVE", 10, deadline)?;
            self.fourg_at_ok(fg, "AT+CFGREAD?", 10, deadline)
        })();
        // 退出产测 best-effort：无论事务成败都尝试（失败只记日志）
        let exit_deadline = Instant::now() + Duration::from_secs(6);
        if let Err(e) = self.fourg_at_ok(fg, "AT+PROD=0", 5, &exit_deadline) {
            self.log(format!("4g-test 退产测失败（忽略）: {e}"));
        }
        let readback = phase?;
        match compare_fourg_readback(&readback, lines) {
            None => Ok(()),
            Some(diff) => {
                self.fourg_diff = diff.clone();
                Err(format!("回读不一致（{} 处差异）", diff.len()))
            }
        }
    }

    /// 单条 4g-test 调用（原始输出）。
    fn fourg_at(
        &self,
        fg: &FourGCfg,
        at: &str,
        secs: u64,
        deadline: &Instant,
    ) -> Result<String, String> {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return Err("整体 60s 上限耗尽".into());
        }
        if at.chars().any(|c| c == '\'' || c.is_control()) {
            return Err(format!("AT 命令含单引号/控制字符（禁）: {at}"));
        }
        let mut cmd = String::from("4g-test");
        let port = fg.port.trim();
        if !port.is_empty() {
            if port
                .chars()
                .any(|c| c.is_whitespace() || c == '\'' || c.is_control())
            {
                return Err(format!("4G 串口路径含非法字符: {port:?}"));
            }
            cmd.push_str(&format!(" -d '{port}'"));
        }
        if fg.baud > 0 {
            cmd.push_str(&format!(" -b {}", fg.baud));
        }
        // AT 内含双引号（CFGSET="..."）：外层单引号包裹交给远端 shell
        let cmd = format!("{cmd} '{at}'");
        self.log(format!("4g-test: {at}"));
        let timeout = Duration::from_secs(secs).min(remain);
        adb::shell_timeout(&cmd, timeout).map_err(|e| format!("{at} 失败: {e}"))
    }

    fn fourg_at_ok(
        &self,
        fg: &FourGCfg,
        at: &str,
        secs: u64,
        deadline: &Instant,
    ) -> Result<String, String> {
        let out = self.fourg_at(fg, at, secs, deadline)?;
        // 老版 adb 不透传退出码：按独立响应行判定（有 OK 行且无 ERROR/TIMEOUT 行）
        let has_ok = out.lines().any(|l| l.trim() == "OK");
        let has_err = out.lines().any(|l| {
            let t = l.trim();
            t == "ERROR"
                || t == "TIMEOUT"
                || t.starts_with("ERROR:")
                || t.contains(":ERR,")
                || t.contains(": ERROR")
        });
        if has_err || !has_ok {
            return Err(format!("{at} 模组未回 OK: {}", clip(&out, 80)));
        }
        Ok(out)
    }

    // —— ⑩ 终检 ——

    fn verify(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Verify);
        let text = match self.cat_config() {
            Ok(t) => t,
            Err(e) => return self.fail_step(ShipStep::Verify, format!("终检 {e}"), t0),
        };
        for key in self.profile.keys.clone() {
            let value = match self.value_for(&key) {
                Ok(v) => v,
                Err(e) => return self.fail_step(ShipStep::Verify, e, t0),
            };
            let r = protocol::parse_cf30_response(&text, SECTION, &key);
            if r.echo_value.as_deref() != Some(value.as_str()) {
                return self.fail_step(
                    ShipStep::Verify,
                    format!(
                        "终检不符：{key} 现值 {} ≠ 目标 {}",
                        clip_secret(r.echo_value.as_deref().unwrap_or("（缺失）")),
                        clip_secret(&value)
                    ),
                    t0,
                );
            }
        }
        self.done(
            ShipStep::Verify,
            StepVerdict::Pass,
            "全部写入项回读一致",
            t0,
        );
        None
    }

    // —— ⑪ 对时 + cf4 + 重启 ——

    fn finalize(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Finalize);
        // 对时（辅助步骤，失败只记日志）
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        for (cmd, what) in [
            (format!("date -s \"{now}\""), "时间写入"),
            (CameraFactory::TimeSync.device_cmd(), "固件校时"),
        ] {
            match self.shell(&cmd) {
                Ok(out) => self.log(format!("{what}: {}", clip(&out, 60))),
                Err(e) => self.log(format!("{what}失败（继续）: {e}")),
            }
        }
        let cf4_cmd = CameraFactory::FactoryReset.device_cmd();
        let out = match self.shell(&cf4_cmd) {
            Ok(o) => o,
            Err(e) => {
                return self.fail_step(
                    ShipStep::Finalize,
                    format!("切换出厂模式（cf4）失败，已保留产测状态: {e}"),
                    t0,
                )
            }
        };
        if !protocol::output_ok(&out) {
            return self.fail_step(
                ShipStep::Finalize,
                format!("cf4 返回失败: {}", clip(&out, 80)),
                t0,
            );
        }
        let _ = adb::shell_timeout("sync", Duration::from_secs(5));
        REBOOTING.store(true, Ordering::Release);
        let _ = self.tx.send(ShipEvent::Rebooting);
        self.log("cf4 完成，已刷盘并下发重启");
        let _ = adb::shell_timeout("reboot", Duration::from_secs(10));
        self.done(
            ShipStep::Finalize,
            StepVerdict::Pass,
            "已切换出厂模式并重启",
            t0,
        );
        None
    }

    // —— ⑫ 重启回读 ——

    fn probe(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Probe);
        let secs = self.ctx.cfg.provision.reboot_verify_secs;
        if secs == 0 {
            self.done(ShipStep::Probe, StepVerdict::Skip, "已跳过重启回读", t0);
            return None;
        }
        let deadline = Instant::now() + Duration::from_secs(secs);
        let is_online = |devs: &[String]| match &self.serial {
            Some(s) => devs.iter().any(|d| d == s),
            None => !devs.is_empty(),
        };
        // 阶段 1：先等设备真正离线（防 adb shell reboot 异步返回导致立刻误检旧连接）
        let mut saw_offline = false;
        while Instant::now() < deadline {
            if exec::abort_requested() {
                return Some(format!("手动停止：{}", exec::abort_reason().text()));
            }
            match adb::devices() {
                Ok(devs) if !is_online(&devs) => {
                    saw_offline = true;
                    break;
                }
                Err(_) => {
                    saw_offline = true;
                    break;
                }
                Ok(_) => {}
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if !saw_offline {
            return self.fail_step(
                ShipStep::Probe,
                format!("{secs}s 内设备未断开离线（reboot 未生效，未完成重启复核）"),
                t0,
            );
        }
        // 阶段 2：离线后再等待设备重新上线
        let mut back = false;
        while Instant::now() < deadline {
            if exec::abort_requested() {
                return Some(format!("手动停止：{}", exec::abort_reason().text()));
            }
            if let Ok(devs) = adb::devices() {
                if is_online(&devs) {
                    back = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if !back {
            // 超时未回：记日志跳过（正式版无 adb 属预期配置），不算失败
            self.done(
                ShipStep::Probe,
                StepVerdict::Warn,
                format!("{secs}s 内未探测到设备回来（跳过——若正式版仍有 adb 请人工核对）"),
                t0,
            );
            return None;
        }
        // 回来了：核对蓝牙 MAC（防换机串号）+ 复核目标键
        let mac_cmd = CameraFactory::BtMac.device_cmd();
        let out = match self.shell(&mac_cmd) {
            Ok(o) => o,
            Err(e) => return self.fail_step(ShipStep::Probe, format!("重启后 cf10 失败: {e}"), t0),
        };
        match parse_dev(&out) {
            Ok(d) if d.bt_mac.eq_ignore_ascii_case(&self.bt_mac) => {}
            Ok(d) => {
                return self.fail_step(
                    ShipStep::Probe,
                    format!(
                        "重启后蓝牙 MAC 不一致：{} ≠ {}（换机/串号？）",
                        d.bt_mac, self.bt_mac
                    ),
                    t0,
                )
            }
            Err(e) => {
                return self.fail_step(
                    ShipStep::Probe,
                    format!("重启后 cf10 输出无法解析: {e}"),
                    t0,
                )
            }
        }
        let text = match self.cat_config() {
            Ok(t) => t,
            Err(e) => return self.fail_step(ShipStep::Probe, format!("重启后 {e}"), t0),
        };
        for key in self.profile.keys.clone() {
            let value = match self.value_for(&key) {
                Ok(v) => v,
                Err(e) => return self.fail_step(ShipStep::Probe, e, t0),
            };
            let r = protocol::parse_cf30_response(&text, SECTION, &key);
            if r.echo_value.as_deref() != Some(value.as_str()) {
                return self.fail_step(
                    ShipStep::Probe,
                    format!("重启后复核不符：{key} 现值 ≠ 目标"),
                    t0,
                );
            }
        }
        self.done(
            ShipStep::Probe,
            StepVerdict::Pass,
            "重启后探测通过（MAC 与目标键一致）",
            t0,
        );
        None
    }

    // —— 收尾：ship 报告 + PASS 落账 ——

    fn finish(&mut self, pass: bool, fail_detail: Option<&str>) -> Option<PathBuf> {
        let now = report::time_str(SystemTime::now());
        if pass {
            // stock 推进 committed（重读磁盘：导入/作废可能中途发生）
            let mut stock = ProvisionStock::load(&self.ctx.stock_path);
            stock.commit_sn(&self.sn, &now);
            if let Some(c) = &self.cred {
                stock.commit_credential(&c.uuid, &now);
            }
            if let Err(e) = stock.save(&self.ctx.stock_path) {
                self.log(format!("库存落盘失败（凭据仍为已分配，可手工核对）: {e:#}"));
            }
            // 台账回填（upsert：warn 闸门过渡期无记录的机器自动建档）
            let partner = if self.model.has_wifi() {
                self.device.wifi_mac.clone()
            } else {
                self.device.imei.clone()
            };
            let mut ledger = Ledger::load(&self.ctx.ledger_path);
            match ledger.mark_shipped(&ShipMeta {
                bt_mac: &self.bt_mac,
                partner: &partner,
                model: self.model.label(),
                sn: &self.sn,
                ccid: &self.device.ccid,
                verdict: "PASS",
                now: &now,
            }) {
                Ok(()) => {
                    if let Err(e) = ledger.save(&self.ctx.ledger_path) {
                        self.log(format!("台账落盘失败: {e:#}"));
                    }
                }
                Err(e) => self.log(format!("台账回填被拒: {e}")),
            }
        }
        let text = self.render_report(pass, fail_detail, &now);
        let ts = report::time_str(self.started_wall);
        let (day, hms) = ts.split_once('-').unwrap_or((ts.as_str(), "unknown"));
        let name = report::ship_file_name(day, &self.bt_mac, hms);
        let path = self.ctx.reports_dir.join(name);
        match std::fs::create_dir_all(&self.ctx.reports_dir)
            .map_err(|e| e.to_string())
            .and_then(|_| std::fs::write(&path, text).map_err(|e| e.to_string()))
        {
            Ok(_) => Some(path),
            Err(e) => {
                self.log(format!("ship 报告写入失败: {e}"));
                None
            }
        }
    }

    /// ship 报告正文（`结果:`/`工位:` 行与报告中心统计约定一致）。
    fn render_report(&self, pass: bool, fail_detail: Option<&str>, now: &str) -> String {
        let mut s = String::new();
        s.push_str(&format!("{} 出厂写号报告\n", self.model.label()));
        s.push_str(&format!("结果: {}\n", if pass { "PASS" } else { "FAIL" }));
        s.push_str(&format!(
            "工位: ship    开始: {}    耗时: {:02}:{:02}:{:02}\n",
            report::time_str(self.started_wall),
            self.started.elapsed().as_secs() / 3600,
            self.started.elapsed().as_secs() % 3600 / 60,
            self.started.elapsed().as_secs() % 60
        ));
        s.push_str(&format!("sn-address:{}\n", self.sn));
        s.push_str(&format!("bt-mac:{}\n", self.bt_mac));
        if !self.device.wifi_mac.is_empty() {
            s.push_str(&format!("wifi-mac:{}\n", self.device.wifi_mac));
        }
        if !self.device.imei.is_empty() {
            s.push_str(&format!("imei:{}\n", self.device.imei));
        }
        if !self.device.ccid.is_empty() {
            s.push_str(&format!("ccid:{}\n", self.device.ccid));
        }
        s.push_str(&format!("soft-ware:{}\n", self.device.t32_ver));
        if !self.bt_ver_seen.is_empty() {
            s.push_str(&format!("bt-ver:{}\n", self.bt_ver_seen));
        }
        if let Some(v) = self.battery_volts {
            s.push_str(&format!("battery:{:.3}V\n", v));
        }
        // 凭据全值（交付档案；确认卡同款，不脱敏）
        if let Some(c) = &self.cred {
            s.push_str(&format!("uuid:{}\n", c.uuid));
            s.push_str(&format!("auth_key:{}\n", c.auth_key));
        }
        if let Some(n) = self.fourg_written {
            s.push_str(&format!(
                "fourg: 写入 {n} 行，回读{}\n",
                if self.fourg_diff.is_empty() {
                    "一致"
                } else {
                    "不一致"
                }
            ));
            for d in &self.fourg_diff {
                s.push_str(&format!("fourg-diff:{d}\n"));
            }
        }
        if !self.warnings.is_empty() {
            s.push_str("\n告警:\n");
            for w in &self.warnings {
                s.push_str(&format!("- {w}\n"));
            }
        }
        if let Some(d) = fail_detail {
            s.push_str(&format!("\n失败原因: {d}\n"));
        }
        s.push('\n');
        for r in &self.results {
            s.push_str(&format!("{}:{}", r.step.label(), r.verdict.label()));
            if !r.detail.is_empty() {
                s.push(' ');
                s.push_str(&r.detail);
            }
            if r.secs > 0 {
                s.push_str(&format!("（{}s）", r.secs));
            }
            s.push('\n');
        }
        let ends_at = now;
        s.push_str(&format!("\n结束: {ends_at}\n"));
        report::sanitize(&s)
    }

    /// 步骤失败落行 + 返回 Some(detail)。
    fn fail_step(&mut self, step: ShipStep, detail: String, t0: Instant) -> Option<String> {
        self.done(step, StepVerdict::Fail, detail.clone(), t0);
        Some(detail)
    }

    /// 安全读取设备端 ini 文件（防路径为空导致 `cat ` 卡死或注入字符）。
    fn cat_config(&self) -> Result<String, String> {
        let path = self.ctx.cfg.provision.config_path.trim();
        if path.is_empty() {
            return Err("配置缺失：config_path 未填".into());
        }
        if path
            .chars()
            .any(|c| c.is_whitespace() || c == '\'' || c.is_control())
        {
            return Err(format!("config_path 含非法字符: {path:?}"));
        }
        self.shell(&format!("cat '{path}'"))
            .map_err(|e| format!("cat {path} 失败: {e}"))
    }

    fn shell(&self, cmd: &str) -> Result<String, String> {
        adb::shell(cmd).map_err(|e| format!("{e:#}"))
    }
}

/// 秒数 → 超时选项（0 = 不限制；与 worker 同语义）。
fn timeout_opt(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// cf 输出 → 设备信息（错误统一字符串化，步骤内拼上下文）。
fn parse_dev(out: &str) -> Result<DeviceInfo, String> {
    protocol::parse_device_info(out).map_err(|e| format!("{e}"))
}

/// 日志/detail 截断（尾部省略号；worker::clip 同款）。
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}...")
    }
}

/// 4G 模板解析：剥空行/`#` 注释，有效行须含 `=` 且禁双引号/单引号/
/// 控制字符（防截断 `AT+CFGSET="..."`），`{{sn}}` 占位替换。
pub fn parse_fourg_template(text: &str, sn: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if !l.contains('=') {
            return Err(format!("模板行缺 `=`: {l}"));
        }
        if let Some(bad) = l
            .chars()
            .find(|c| *c == '"' || *c == '\'' || c.is_control())
        {
            return Err(format!("模板行含禁字符 {bad:?}: {l}"));
        }
        out.push(l.replace("{{sn}}", sn));
    }
    if out.is_empty() {
        return Err("模板无有效配置行".into());
    }
    Ok(out)
}

/// 4G 回读比对：剥 `+CFGREAD:` 前缀（含带引号包裹的变体）、滤空行/注释、
/// trim 归一后校验：1) 目标行全部在回读中；2) 目标键在回读中无重复/冲突行
/// （防止重复写同一键导致旧值/新值并存）。返回差异列表；None = 一致。
pub fn compare_fourg_readback(readback: &str, expected: &[String]) -> Option<Vec<String>> {
    let mut seen: Vec<String> = Vec::new();
    for raw in readback.lines() {
        let mut l = raw.trim();
        if l == "OK" {
            continue;
        }
        if let Some(v) = l.strip_prefix("+CFGREAD:") {
            l = v.trim();
        }
        // 带引号包裹的回读变体：+CFGREAD:"apn=x"
        let l = l.strip_prefix('"').unwrap_or(l);
        let l = l.strip_suffix('"').unwrap_or(l).trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        seen.push(l.to_string());
    }
    let mut diff: Vec<String> = Vec::new();
    for exp in expected {
        let count = seen.iter().filter(|s| *s == exp).count();
        if count == 0 {
            diff.push(format!("缺失 {exp}"));
            continue;
        }
        if let Some((ek, _)) = exp.split_once('=') {
            let ek = ek.trim();
            let same_key_rows: Vec<&String> = seen
                .iter()
                .filter(|s| s.split_once('=').is_some_and(|(k, _)| k.trim() == ek))
                .collect();
            if same_key_rows.len() > 1 {
                diff.push(format!("重复键 {ek}（出现 {} 次）", same_key_rows.len()));
            }
        }
    }
    (!diff.is_empty()).then_some(diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯函数：模板解析与回读比对（含重复键/冲突行校验）。
    #[test]
    fn fourg_template_parse_and_compare() {
        let tpl = "# 服务器配置\n\nserver=ota.example.com\napn=cmnet\nsn_line={{sn}}\n";
        let lines = parse_fourg_template(tpl, "6302W1000288").unwrap();
        assert_eq!(
            lines,
            vec![
                "server=ota.example.com",
                "apn=cmnet",
                "sn_line=6302W1000288"
            ]
        );
        // 禁字符：双引号 / 单引号 / 控制
        assert!(parse_fourg_template("apn=\"x\"\n", "S").is_err());
        assert!(parse_fourg_template("apn=x'y\n", "S").is_err());
        assert!(parse_fourg_template("apn=a\u{11}b\n", "S").is_err());
        // 缺 =
        let err = parse_fourg_template("no-equal\n", "S").unwrap_err();
        assert!(err.contains("缺 `=`"));
        // 全空模板
        assert!(parse_fourg_template("# only comment\n", "S").is_err());

        // 回读比对：前缀剥除 + 引号变体 + 注释滤除 + 缺失 diff
        let rb = "+CFGREAD: server=ota.example.com\n+CFGREAD:\"apn=cmnet\"\n# noise\nsn_line=6302W1000288\nOK";
        assert_eq!(compare_fourg_readback(rb, &lines), None);
        let missing =
            compare_fourg_readback("+CFGREAD: server=ota.example.com\nOK", &lines).unwrap();
        assert_eq!(missing.len(), 2);
        assert!(missing.iter().any(|m| m.contains("apn=cmnet")));
        // 模组多余无关行不算失败
        assert_eq!(
            compare_fourg_readback(
                "+CFGREAD: extra=1\n+CFGREAD: apn=cmnet\n",
                &["apn=cmnet".to_string()]
            ),
            None
        );
        // 同键重复/冲突行应检出（防单行重复写入残留旧值）
        let dup = compare_fourg_readback(
            "+CFGREAD: apn=old\n+CFGREAD: apn=cmnet\nOK",
            &["apn=cmnet".to_string()],
        )
        .unwrap();
        assert!(dup.iter().any(|d| d.contains("重复键 apn")), "{dup:?}");
    }
}

/// 端到端测试（假 `#!/bin/sh` adb 桩，仅 Unix 构建；Windows 下不编入避免
/// `exec::TEST_LOCK` 编译错误及 sh 不可执行问题）。
#[cfg(all(unix, test))]
mod unix_e2e_tests {
    use super::*;
    use crate::config::ProvisionCfg;
    use std::sync::mpsc;

    /// 进程级 `ADB` 环境变量与临时目录守卫：无论通过或 panic 均自动还原清理。
    struct FakeAdbGuard {
        dir: PathBuf,
    }

    impl FakeAdbGuard {
        fn new(tag: &str, script_body: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "e02-ship-{tag}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(dir.join("reports")).unwrap();
            let script = dir.join("adb");
            std::fs::write(&script, script_body).unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::env::set_var("ADB", &script);
            Self { dir }
        }

        fn ctx(&self, cfg: Config) -> ShipCtx {
            ShipCtx {
                data_dir: self.dir.clone(),
                reports_dir: self.dir.join("reports"),
                stock_path: self.dir.join("stock.json"),
                ledger_path: self.dir.join("ledger.json"),
                cfg,
            }
        }
    }

    impl Drop for FakeAdbGuard {
        fn drop(&mut self) {
            std::env::remove_var("ADB");
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 构造默认 E02W 测试配置（已填工单版本、关闭重启等待）。
    fn e02w_cfg(gate_mode: GateMode) -> Config {
        let mut prov = ProvisionCfg {
            gate_mode,
            auto_commit: true,
            reboot_verify_secs: 0,
            ..Default::default()
        };
        prov.e02w.t32_ver = "E02W-1.0.1".into();
        prov.e02w.bt_ver = "V1_7_4".into();
        Config {
            station: Config::STATION_SHIP.into(),
            model: Some(ModelKind::E02w),
            provision: prov,
            ..Default::default()
        }
    }

    /// 通用假 adb：cf30 剥单引号写入 state.ini 并回显，cat 读取 state.ini。
    const STANDARD_E02W_SCRIPT: &str = "\
#!/bin/sh
st=\"$(dirname \"$0\")/state.ini\"
case \"$*\" in
  *'camera-factory 30'*)
    clean=$(printf '%s' \"$*\" | tr -d \"'\")
    k=$(printf '%s' \"$clean\" | awk '{print $(NF-1)}')
    v=$(printf '%s' \"$clean\" | awk '{print $NF}')
    grep -v \"^$k=\" \"$st\" > \"$st.tmp\" 2>/dev/null; mv \"$st.tmp\" \"$st\"
    echo \"$k=$v\" >> \"$st\"
    printf '[ProvisionInfo]\\n'; cat \"$st\"; printf 'config-update:true\\n'
    ;;
  *'camera-factory 10'*) printf 'MAC:0012A3B4C5D6\\n';;
  *'camera-factory 3'*) printf 'sn-address:00001\\nwifi-mac:0060A70B9CF2\\nsoft-ware:E02W-1.0.1\\n';;
  *'camera-factory 7'*) printf 'bt-ver:V1_7_4\\n';;
  *'camera-factory 12'*) printf 'Bat vol:4.326\\n';;
  *'camera-factory 8'*) printf 'true\\n';;
  *'camera-factory 4'*) printf 'factory-reset true\\n';;
  *'camera-factory 19'*) printf 'ok\\n';;
  *'cat '*) printf '[ProvisionInfo]\\n'; cat \"$st\" 2>/dev/null; printf '\\n';;
  *reboot*) exit 0;;
  *) printf 'ok\\n';;
esac
";

    fn run_and_collect(
        sn: &str,
        ctx: ShipCtx,
        answer_confirm: bool,
    ) -> (Option<PathBuf>, bool, Vec<ConfirmCard>, Vec<ShipStepResult>) {
        let (tx, rx) = mpsc::channel();
        let (atx, arx) = mpsc::channel();
        let sn = sn.to_string();
        let handle = std::thread::spawn(move || run_ship(&sn, None, &tx, &arx, &ctx));
        let mut pass = false;
        let mut confirms = Vec::new();
        let mut steps = Vec::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            match ev {
                ShipEvent::Confirm(card) => {
                    confirms.push(card);
                    let _ = atx.send(ShipAnswer::Confirm(answer_confirm));
                }
                ShipEvent::StepResult(r) => steps.push(r),
                ShipEvent::Finished { pass: p, .. } => pass = p,
                _ => {}
            }
        }
        (handle.join().unwrap(), pass, confirms, steps)
    }

    /// 端到端 E02W PASS：warn 整机校验触发确认卡 → cf30 写 sn → 回读校验 → cf4 → 台账 + ship 报告。
    #[test]
    fn ship_flow_e02_pass_end_to_end() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let env = FakeAdbGuard::new("e02w-pass", STANDARD_E02W_SCRIPT);
        let cfg = e02w_cfg(GateMode::Warn);
        let (report, pass, confirms, _) = run_and_collect("6472w1000016", env.ctx(cfg), true);

        let report = report.expect("应产出 ship 报告");
        assert!(pass, "E02W 全流程应 PASS");
        assert_eq!(confirms.len(), 1, "未过整机校验时必须人工确认");
        assert_eq!(confirms[0].sn, "6472W1000016");
        assert_eq!(confirms[0].bt_mac, "0012A3B4C5D6");
        assert!(!confirms[0].re_run);

        let text = std::fs::read_to_string(&report).unwrap();
        assert!(text.contains("结果: PASS"));
        assert!(text.contains("工位: ship"));
        assert!(text.contains("sn-address:6472W1000016"));
        assert!(text.contains("bt-mac:0012A3B4C5D6"));
        assert!(text.contains("写入配置:PASS 1 项写入成功，0 项已一致跳过"));

        let ledger = Ledger::load(&env.dir.join("ledger.json"));
        let rec = ledger.find_by_shipped_sn("6472W1000016").unwrap();
        assert_eq!(rec.bt_mac, "0012A3B4C5D6");
        assert_eq!(rec.model, "E02W");
        assert_eq!(rec.ship_verdict, "PASS");
    }

    /// 失败路径：版本不符 → FAIL、严禁执行 cf4。
    #[test]
    fn ship_flow_version_mismatch_stops_before_cf4() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let dir_hint = std::env::temp_dir().join(format!("cf4-mark-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir_hint);
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             *'camera-factory 10'*) printf 'MAC:0012A3B4C5D6\\n';;\n\
             *'camera-factory 3'*) printf 'sn-address:00001\\nwifi-mac:0060A70B9CF2\\nsoft-ware:E02W-9.9.9\\n';;\n\
             *'camera-factory 4'*) touch '{}';;\n\
             *'cat '*) printf '[ProvisionInfo]\\nsn=\\n';;\n\
             *) printf 'ok\\n';;\nesac\n",
            dir_hint.display()
        );
        let env = FakeAdbGuard::new("ver-fail", &script);
        let (report, pass, _, _) =
            run_and_collect("6472W1000016", env.ctx(e02w_cfg(GateMode::Off)), true);

        assert!(report.is_some(), "失败也落 ship 报告留痕");
        assert!(!pass);
        assert!(!dir_hint.exists(), "失败路径严禁执行 cf4");
        let _ = std::fs::remove_file(&dir_hint);
    }

    /// SN 冲突拦截 + 占位 SN 放行。
    #[test]
    fn ship_flow_different_formal_sn_rejected_and_placeholder_allowed() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let env = FakeAdbGuard::new("diff-sn", STANDARD_E02W_SCRIPT);
        std::fs::write(env.dir.join("state.ini"), "sn=6472W1000024\n").unwrap();
        let (_, pass, confirms, steps) =
            run_and_collect("6472W1000016", env.ctx(e02w_cfg(GateMode::Off)), true);
        assert!(!pass, "设备已有其他正式 SN 必须禁止写入");
        assert!(confirms.is_empty());
        assert!(steps
            .iter()
            .any(|s| s.step == ShipStep::SnCheck && s.detail.contains("冲突")));

        std::fs::write(env.dir.join("state.ini"), "sn=00001\n").unwrap();
        let mut cfg = e02w_cfg(GateMode::Off);
        cfg.provision.e02w.sn_pattern = r"^[0-9A-Z]{5,12}$".into();
        let (_, pass, _, _) = run_and_collect("6472W1000016", env.ctx(cfg), true);
        assert!(pass, "命中 placeholder_sns 的设备应按未写号放行");
    }

    /// 整机校验 strict：未过整机全 PASS 禁止写入，过整机全 PASS 自动放行。
    #[test]
    fn ship_flow_strict_gate_blocks_until_complete_pass() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let env = FakeAdbGuard::new("strict-gate", STANDARD_E02W_SCRIPT);
        let (_, pass, _, steps) =
            run_and_collect("6472W1000016", env.ctx(e02w_cfg(GateMode::Strict)), true);
        assert!(!pass, "无整机 PASS 记录时 strict 必须禁止写入");
        assert!(steps
            .iter()
            .any(|s| s.step == ShipStep::Gate && s.verdict == StepVerdict::Fail));

        let mut ledger = Ledger::default();
        ledger.bind(
            "0012A3B4C5D6",
            "0060A70B9CF2",
            "E02W",
            "00001",
            "complete",
            "t1",
        );
        ledger.set_verdict("0012A3B4C5D6", "PASS", "complete", true, "t1");
        ledger.save(&env.dir.join("ledger.json")).unwrap();
        let (_, pass, confirms, _) =
            run_and_collect("6472W1000016", env.ctx(e02w_cfg(GateMode::Strict)), true);
        assert!(pass, "整机全 PASS 后 strict 放行");
        assert!(confirms.is_empty(), "auto_commit 且无提醒直接写入");
    }

    /// 无可用凭据前置拦截；同 SN 重跑复用凭据 + 覆盖已有凭据提醒。
    #[test]
    fn ship_flow_cred_empty_reject_and_rerun_idempotent_and_self_gen_warn() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let env = FakeAdbGuard::new("cred-cases", STANDARD_E02W_SCRIPT);

        let mut cfg = e02w_cfg(GateMode::Warn);
        cfg.provision.e02w.keys = vec!["sn".into(), "uuid".into(), "auth_key".into()];

        let (_, pass, confirms, steps) =
            run_and_collect("6472W1000016", env.ctx(cfg.clone()), true);
        assert!(!pass);
        assert!(confirms.is_empty(), "无可用凭据必须在确认前直接拦截");
        assert!(steps
            .iter()
            .any(|s| s.step == ShipStep::Claim && s.detail.contains("无可用凭据")));

        let mut st = ProvisionStock::default();
        st.import_credentials(
            "uuid95fe79695a6df72f,B7uTB4QjoSLw55oHLk2wPSokAAwWXENH\n\
             uuid95fe79695a6df730,C7uTB4QjoSLw55oHLk2wPSokAAwWXENH\n",
            r"^uuid[0-9a-f]{16}$",
            r"^[A-Za-z0-9]{32}$",
            "t0",
        );
        st.save(&env.dir.join("stock.json")).unwrap();
        std::fs::write(
            env.dir.join("state.ini"),
            "sn=00001\nuuid=uuid0000000000000000\nauth_key=ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ\n",
        )
        .unwrap();
        let mut cfg_off = cfg.clone();
        cfg_off.provision.gate_mode = GateMode::Off;
        let (_, pass, confirms, _) =
            run_and_collect("6472W1000016", env.ctx(cfg_off.clone()), true);
        assert!(pass);
        assert_eq!(confirms.len(), 1);
        assert!(
            confirms[0]
                .warnings
                .iter()
                .any(|w| w.contains("设备已有不同 uuid")),
            "{:?}",
            confirms[0].warnings
        );

        let (_, pass, confirms, steps) = run_and_collect("6472W1000016", env.ctx(cfg_off), true);
        assert!(pass);
        assert_eq!(confirms.len(), 1);
        assert!(confirms[0].re_run);
        assert_eq!(
            confirms[0]
                .warnings
                .iter()
                .filter(|w| w.contains("同 SN 重写"))
                .count(),
            1,
            "同 SN 重写提醒不得重复出现两条"
        );
        assert!(steps.iter().any(
            |s| s.step == ShipStep::Write && s.detail.contains("0 项写入成功，3 项已一致跳过")
        ));
        let st_after = ProvisionStock::load(&env.dir.join("stock.json"));
        assert_eq!(
            st_after.available_credentials(),
            1,
            "重跑必须复用原凭据，不消耗第二对"
        );
    }

    /// cf30 失败/回显不符 → 立即判 FAIL 且严禁执行 cf4。
    #[test]
    fn ship_flow_cf30_false_or_echo_mismatch_stops_before_cf4() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let cf4_mark = std::env::temp_dir().join(format!("cf4-cf30-{}", std::process::id()));
        let _ = std::fs::remove_file(&cf4_mark);
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             *'camera-factory 30'*) printf '[ProvisionInfo]\\nsn=WRONG\\nconfig-update:true\\n';;\n\
             *'camera-factory 10'*) printf 'MAC:0012A3B4C5D6\\n';;\n\
             *'camera-factory 3'*) printf 'sn-address:00001\\nwifi-mac:0060A70B9CF2\\nsoft-ware:E02W-1.0.1\\n';;\n\
             *'camera-factory 7'*) printf 'bt-ver:V1_7_4\\n';;\n\
             *'camera-factory 12'*) printf 'Bat vol:4.200\\n';;\n\
             *'camera-factory 8'*) printf 'true\\n';;\n\
             *'camera-factory 4'*) touch '{}';;\n\
             *'cat '*) printf '[ProvisionInfo]\\nsn=\\n';;\n\
             *) printf 'ok\\n';;\nesac\n",
            cf4_mark.display()
        );
        let env = FakeAdbGuard::new("cf30-mismatch", &script);
        let (_, pass, _, steps) =
            run_and_collect("6472W1000016", env.ctx(e02w_cfg(GateMode::Off)), true);
        assert!(!pass);
        assert!(steps
            .iter()
            .any(|s| s.step == ShipStep::Write && s.detail.contains("回显不符")));
        assert!(!cf4_mark.exists(), "cf30 失败严禁执行 cf4");
        let _ = std::fs::remove_file(&cf4_mark);
    }

    /// 步骤 ⑫ 重启回读：先经历离线再回连 → PASS；重启后换机 MAC 不符 → FAIL。
    #[test]
    fn ship_flow_probe_waits_offline_and_verifies_mac_and_keys() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let script = "\
#!/bin/sh
dir=\"$(dirname \"$0\")\"
st=\"$dir/state.ini\"
devcnt=\"$dir/devcnt\"
case \"$*\" in
  devices)
    n=$(cat \"$devcnt\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$devcnt\"
    if [ $n -eq 1 ]; then
      printf 'List of devices attached\\n123456\\tdevice\\n'
    elif [ $n -eq 2 ]; then
      printf 'List of devices attached\\n'
    else
      printf 'List of devices attached\\n123456\\tdevice\\n'
    fi
    ;;
  *'camera-factory 30'*)
    clean=$(printf '%s' \"$*\" | tr -d \"'\")
    k=$(printf '%s' \"$clean\" | awk '{print $(NF-1)}')
    v=$(printf '%s' \"$clean\" | awk '{print $NF}')
    grep -v \"^$k=\" \"$st\" > \"$st.tmp\" 2>/dev/null; mv \"$st.tmp\" \"$st\"
    echo \"$k=$v\" >> \"$st\"
    printf '[ProvisionInfo]\\n'; cat \"$st\"; printf 'config-update:true\\n'
    ;;
  *'camera-factory 10'*)
    if [ -f \"$dir/swap-mac\" ] && [ -f \"$devcnt\" ]; then
      printf 'MAC:AABBCCDDEEFF\\n'
    else
      printf 'MAC:0012A3B4C5D6\\n'
    fi
    ;;
  *'camera-factory 3'*) printf 'sn-address:00001\\nwifi-mac:0060A70B9CF2\\nsoft-ware:E02W-1.0.1\\n';;
  *'camera-factory 7'*) printf 'bt-ver:V1_7_4\\n';;
  *'camera-factory 12'*) printf 'Bat vol:4.326\\n';;
  *'camera-factory 8'*) printf 'true\\n';;
  *'camera-factory 4'*) printf 'factory-reset true\\n';;
  *'cat '*) printf '[ProvisionInfo]\\n'; cat \"$st\" 2>/dev/null; printf '\\n';;
  *reboot*) exit 0;;
  *) printf 'ok\\n';;
esac
";
        let env = FakeAdbGuard::new("probe", script);
        let mut cfg = e02w_cfg(GateMode::Off);
        cfg.provision.reboot_verify_secs = 10;

        let (_, pass, _, steps) = run_and_collect("6472W1000016", env.ctx(cfg.clone()), true);
        assert!(pass, "正常重启回连应 PASS");
        assert!(steps
            .iter()
            .any(|s| s.step == ShipStep::Probe && s.verdict == StepVerdict::Pass));
        assert!(!is_rebooting(), "run_ship 结束后原子重启位必须自动复位");

        let _ = std::fs::remove_file(env.dir.join("devcnt"));
        let _ = std::fs::remove_file(env.dir.join("state.ini"));
        let _ = std::fs::remove_file(env.dir.join("ledger.json"));
        std::fs::write(env.dir.join("swap-mac"), "1").unwrap();
        let (_, pass, _, steps) = run_and_collect("6472W1000016", env.ctx(cfg), true);
        assert!(!pass, "重启后蓝牙 MAC 不一致应判 FAIL");
        assert!(steps
            .iter()
            .any(|s| s.step == ShipStep::Probe && s.detail.contains("蓝牙 MAC 不一致")));
    }

    /// 端到端 eSIM：凭据 + 4G 参数写入（首轮回读缺行自动整轮重试通过）。
    #[test]
    fn ship_flow_e02e_credentials_and_4g_retry() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let script = "\
#!/bin/sh
dir=\"$(dirname \"$0\")\"
st=\"$dir/state.ini\"
cnt=\"$dir/cfgread-cnt\"
case \"$*\" in
  *'camera-factory 30'*)
    clean=$(printf '%s' \"$*\" | tr -d \"'\")
    k=$(printf '%s' \"$clean\" | awk '{print $(NF-1)}')
    v=$(printf '%s' \"$clean\" | awk '{print $NF}')
    grep -v \"^$k=\" \"$st\" > \"$st.tmp\" 2>/dev/null; mv \"$st.tmp\" \"$st\"
    echo \"$k=$v\" >> \"$st\"
    printf '[ProvisionInfo]\\n'; cat \"$st\"; printf 'config-update:true\\n'
    ;;
  *'camera-factory 10'*) printf 'MAC:9999A3B4C5D6\\n';;
  *'camera-factory 3'*) printf 'sn-address:00001\\nsoft-ware:E02E-2.0.1\\n';;
  *'camera-factory 20'*) printf 'imei:864192051234567\\nccid:898608691025D0768737\\n';;
  *'camera-factory 7'*) printf 'bt-ver:V2_0_1\\n';;
  *'camera-factory 12'*) printf 'Vol:4.100,cur:0.200\\n';;
  *'camera-factory 8'*) printf 'true\\n';;
  *'camera-factory 4'*) printf 'true\\n';;
  *'cat '*) printf '[ProvisionInfo]\\n'; cat \"$st\" 2>/dev/null; printf '\\n';;
  *CFGREAD*)
    n=$(cat \"$cnt\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$cnt\"
    if [ $n -lt 2 ]; then
      printf '+CFGREAD: server=ota.example.com\\nOK\\n'
    else
      printf '+CFGREAD: server=ota.example.com\\n+CFGREAD: apn=cmnet\\n+CFGREAD: sn_line=6472E1000012\\nOK\\n'
    fi
    ;;
  *4g-test*) printf 'OK\\n';;
  *reboot*) exit 0;;
  *) printf 'ok\\n';;
esac
";
        let env = FakeAdbGuard::new("e02e-4g", script);
        let tpl_path = env.dir.join("templates").join("test_e02e_4g.ini");
        std::fs::create_dir_all(tpl_path.parent().unwrap()).unwrap();
        std::fs::write(
            &tpl_path,
            "# eSIM 4G\nserver=ota.example.com\napn=cmnet\nsn_line={{sn}}\n",
        )
        .unwrap();

        let mut stock = ProvisionStock::default();
        stock
            .import_sns("6472E1000012\n", r"^\d{4}E\d{7}$", "t0")
            .ok();
        stock
            .import_credentials(
                "uuid95fe79695a6df72f,B7uTB4QjoSLw55oHLk2wPSokAAwWXENH\n",
                r"^uuid[0-9a-f]{16}$",
                r"^[A-Za-z0-9]{32}$",
                "t0",
            )
            .ok();
        stock.save(&env.dir.join("stock.json")).unwrap();

        let mut prov = ProvisionCfg {
            gate_mode: GateMode::Off,
            auto_commit: true,
            reboot_verify_secs: 0,
            ..Default::default()
        };
        prov.e02e.t32_ver = "E02E-2.0.1".into();
        prov.e02e.bt_ver = "V2_0_1".into();
        prov.e02e.keys = vec!["sn".into(), "uuid".into(), "auth_key".into()];
        prov.e02e.fourg = Some(FourGCfg {
            enable: true,
            port: String::new(),
            baud: 0,
            template: "templates/test_e02e_4g.ini".into(),
        });
        let cfg = Config {
            station: Config::STATION_SHIP.into(),
            model: Some(ModelKind::E02e),
            provision: prov,
            ..Default::default()
        };

        let (report, pass, _, steps) = run_and_collect("6472E1000012", env.ctx(cfg), true);
        let report = report.expect("eSIM 全流程应产出报告");
        assert!(pass);
        assert!(steps.iter().any(|s| s.step == ShipStep::FourG));
        let n: u32 = std::fs::read_to_string(env.dir.join("cfgread-cnt"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(n >= 2, "首轮回读缺行应触发 CFGSTART 整轮重试，实测 {n} 次");

        let text = std::fs::read_to_string(&report).unwrap();
        assert!(text.contains("uuid:uuid95fe79695a6df72f"), "凭据全值落报告");
        assert!(text.contains("auth_key:B7uTB4QjoSLw55oHLk2wPSokAAwWXENH"));
        assert!(text.contains("imei:864192051234567"));
        assert!(text.contains("fourg: 写入 3 行，回读一致"));

        let stock = ProvisionStock::load(&env.dir.join("stock.json"));
        assert_eq!(
            stock.sn_entry("6472E1000012").unwrap().status,
            EntryStatus::Committed
        );
        let cred = stock.credential_by_sn("6472E1000012").unwrap();
        assert_eq!(cred.status, EntryStatus::Committed);
        assert_eq!(cred.bound_bt_mac, "9999A3B4C5D6");

        let ledger = Ledger::load(&env.dir.join("ledger.json"));
        let rec = ledger.find_by_shipped_sn("6472E1000012").unwrap();
        assert_eq!(rec.partner, "864192051234567");
    }

    /// 安全校验：缺 IMEI/CCID、硬件地址冲突、4G 开启但模板为空均必须拦截。
    #[test]
    fn ship_flow_blocks_missing_esim_identity_and_bt_conflict_and_empty_fourg() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();

        let no_imei_script = "\
#!/bin/sh
case \"$*\" in
  *'camera-factory 10'*) printf 'MAC:9999A3B4C5D6\\n';;
  *'camera-factory 3'*) printf 'sn-address:00001\\nsoft-ware:E02E-2.0.1\\n';;
  *'camera-factory 20'*) printf 'imei:\\nccid:\\n';;
  *'cat '*) printf '[ProvisionInfo]\\nsn=00001\\n';;
  *) printf 'ok\\n';;
esac
";
        let env1 = FakeAdbGuard::new("e02e-no-imei", no_imei_script);
        let mut e02e_cfg = Config {
            station: Config::STATION_SHIP.into(),
            model: Some(ModelKind::E02e),
            ..Default::default()
        };
        e02e_cfg.provision.gate_mode = GateMode::Off;
        e02e_cfg.provision.reboot_verify_secs = 0;
        e02e_cfg.provision.e02e.t32_ver = "E02E-2.0.1".into();
        e02e_cfg.provision.e02e.bt_ver = "V2_0_1".into();
        let (_, pass, _, steps) = run_and_collect("6472E1000012", env1.ctx(e02e_cfg.clone()), true);
        assert!(!pass, "E02E 缺 IMEI 必须拦截");
        assert!(steps
            .iter()
            .any(|s| s.step == ShipStep::Identity && s.verdict == StepVerdict::Fail));

        let env2 = FakeAdbGuard::new("bt-conflict", STANDARD_E02W_SCRIPT);
        let mut ledger = Ledger::default();
        ledger
            .mark_shipped(&ShipMeta {
                bt_mac: "0012A3B4C5D6",
                partner: "AA:BB:CC:DD:EE:FF",
                model: "E02W",
                sn: "6472W1000024",
                ccid: "",
                verdict: "PASS",
                now: "t0",
            })
            .unwrap();
        ledger.save(&env2.dir.join("ledger.json")).unwrap();
        let (_, pass, _, steps) =
            run_and_collect("6472W1000016", env2.ctx(e02w_cfg(GateMode::Off)), true);
        assert!(!pass, "蓝牙地址冲突必须拦截");
        assert!(steps
            .iter()
            .any(|s| s.step == ShipStep::Identity && s.detail.contains("硬件地址冲突")));
        let after = Ledger::load(&env2.dir.join("ledger.json"));
        assert_eq!(
            after.find_by_shipped_sn("6472W1000024").unwrap().partner,
            "AA:BB:CC:DD:EE:FF"
        );

        let ok_esim_script = "\
#!/bin/sh
st=\"$(dirname \"$0\")/state.ini\"
case \"$*\" in
  *'camera-factory 30'*) printf '[ProvisionInfo]\\nsn=6472E1000012\\nconfig-update:true\\n';;
  *'camera-factory 10'*) printf 'MAC:9999A3B4C5D6\\n';;
  *'camera-factory 3'*) printf 'sn-address:00001\\nsoft-ware:E02E-2.0.1\\n';;
  *'camera-factory 20'*) printf 'imei:864192051234567\\nccid:898608691025D0768737\\n';;
  *'camera-factory 7'*) printf 'bt-ver:V2_0_1\\n';;
  *'camera-factory 12'*) printf 'Vol:4.100\\n';;
  *'camera-factory 8'*) printf 'true\\n';;
  *'cat '*) printf '[ProvisionInfo]\\nsn=00001\\n';;
  *) printf 'ok\\n';;
esac
";
        let env3 = FakeAdbGuard::new("fourg-empty-tpl", ok_esim_script);
        e02e_cfg.provision.e02e.fourg = Some(FourGCfg {
            enable: true,
            port: String::new(),
            baud: 0,
            template: String::new(),
        });
        let (_, pass, _, steps) = run_and_collect("6472E1000012", env3.ctx(e02e_cfg), true);
        assert!(!pass, "已启用 4G 但模板为空必须拦截");
        assert!(steps
            .iter()
            .any(|s| s.step == ShipStep::FourG && s.verdict == StepVerdict::Fail));
    }
}
