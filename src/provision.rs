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
use crate::ledger::Ledger;
use crate::protocol::{self, CameraFactory, DeviceInfo};
use crate::report;
use crate::stock::{self, CredEntry, EntryStatus, ProvisionStock};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime};

/// cf30 写入的 ini 节名（2026-11-23 真机实测样例 `[ProvisionInfo]`）。
const SECTION: &str = "ProvisionInfo";

/// 4G 写入事务整体上限（FQC §3.1：含整轮重试的兜底墙钟）。
const FOURG_TOTAL_BUDGET: Duration = Duration::from_secs(60);

/// 出厂页 ↔ worker 事件。
#[derive(Debug)]
pub enum ShipEvent {
    Started,
    /// 步骤开始（进度指示）
    Current(ShipStep),
    /// 步骤结论（出厂页步骤行）
    StepResult(ShipStepResult),
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
            ShipStep::Identity => "身份采集",
            ShipStep::Gate => "整机闸门",
            ShipStep::SnCheck => "SN 校验",
            ShipStep::Versions => "版本核对",
            ShipStep::Hardware => "硬件终检",
            ShipStep::Claim => "凭据领用",
            ShipStep::Write => "写入配置",
            ShipStep::FourG => "4G 配置",
            ShipStep::Verify => "写后终检",
            ShipStep::Finalize => "切正式版",
            ShipStep::Probe => "重启探测",
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
    /// 报告/步骤行文字。
    pub fn label(self) -> &'static str {
        match self {
            StepVerdict::Pending => "未测",
            StepVerdict::Pass => "PASS",
            StepVerdict::Warn => "告警",
            StepVerdict::Fail => "FAIL",
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

/// 出厂流程执行上下文（路径可注入——测试指到临时目录）。
pub struct ShipCtx {
    pub cfg: Config,
    pub reports_dir: PathBuf,
    pub stock_path: PathBuf,
    pub ledger_path: PathBuf,
}

impl ShipCtx {
    pub fn new(cfg: &Config, reports_dir: &Path) -> Self {
        Self {
            cfg: cfg.clone(),
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
    let _target = adb::TargetGuard::new(serial.clone());
    let prev_timeout = exec::default_timeout();
    exec::set_default_timeout(timeout_opt(ctx.cfg.timeouts.item_secs));
    let _ = tx.send(ShipEvent::Started);
    // 档案表为空 = 配置残缺，直接判失败（不 panic 在 worker 线程里）
    let Some(profile) = ctx.cfg.provision.active().cloned() else {
        let _ = tx.send(ShipEvent::Log(
            "出厂档案表为空（config.toml [provision.profiles]），无法写号".into(),
        ));
        let _ = tx.send(ShipEvent::Finished {
            report: None,
            pass: false,
        });
        return None;
    };
    let model = profile.model;
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
    run.log(format!(
        "出厂写号开始：SN {}，档案 {}（{}），闸门 {:?}",
        run.sn,
        ctx.cfg.provision.active_profile,
        run.model.label(),
        ctx.cfg.provision.gate_mode,
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
    if let Some(fail) = run.fourg() {
        return Some(fail);
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

    /// 步骤开始 + 返回计时起点。
    fn begin(&self, step: ShipStep) -> Instant {
        let _ = self.tx.send(ShipEvent::Current(step));
        Instant::now()
    }

    /// 步骤结论（广播 + 留档进 ship 报告）。
    fn done(&mut self, step: ShipStep, verdict: StepVerdict, detail: impl Into<String>, t0: Instant) {
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
        if self.bt_mac.is_empty() {
            return self.fail_step(
                ShipStep::Identity,
                "cf10 未解析到蓝牙 MAC——出厂写号以蓝牙 MAC 为身份键，读不到拒写".into(),
                t0,
            );
        }
        // cf3 设备信息（SN/WiFi MAC/软件版本）
        let info_cmd = CameraFactory::DeviceInfo.device_cmd();
        match self.shell(&info_cmd).and_then(|o| parse_dev(&o)) {
            Ok(d) => self.device.merge(d),
            Err(e) => {
                self.log(format!("cf3 设备信息读取失败（继续，版本核对项兜底拦）: {e}"));
            }
        }
        // eSIM 型：cf20 读 IMEI/CCID（出货明细列）
        if !self.model.has_wifi() {
            let esim_cmd = CameraFactory::CcidImei.device_cmd();
            match self.shell(&esim_cmd).and_then(|o| parse_dev(&o)) {
                Ok(d) => self.device.merge(d),
                Err(e) => self.log(format!("cf20 IMEI/CCID 读取失败（继续）: {e}")),
            }
        }
        // 读设备端配置现值（写入键的目标区）
        let cat = format!("cat {}", self.ctx.cfg.provision.config_path);
        match self.shell(&cat) {
            Ok(text) => {
                for key in self.profile.keys.clone() {
                    let v = protocol::parse_cf30_response(&text, SECTION, &key)
                        .echo_value
                        .unwrap_or_default();
                    self.current.insert(key, v);
                }
                let sn_now = self
                    .current
                    .get("sn")
                    .cloned()
                    .unwrap_or_default();
                self.done(
                    ShipStep::Identity,
                    StepVerdict::Pass,
                    format!(
                        "蓝牙 {}，固件 [{}]，配置现值 sn={}",
                        self.bt_mac,
                        self.device.t32_ver,
                        if sn_now.is_empty() { "（空）".into() } else { sn_now }
                    ),
                    t0,
                );
                None
            }
            Err(e) => self.fail_step(
                ShipStep::Identity,
                format!("cat {} 失败: {e}", self.ctx.cfg.provision.config_path),
                t0,
            ),
        }
    }

    // —— ① 整机闸门 ——

    fn gate(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Gate);
        let mode = self.ctx.cfg.provision.gate_mode;
        if mode == GateMode::Off {
            self.done(ShipStep::Gate, StepVerdict::Skip, "闸门关闭（off）", t0);
            return None;
        }
        let ledger = Ledger::load(&self.ctx.ledger_path);
        if ledger.complete_pass(&self.bt_mac) {
            self.done(ShipStep::Gate, StepVerdict::Pass, "整机工位全测 PASS", t0);
            return None;
        }
        let msg = "台账无整机工位（complete）全测 PASS 记录";
        match mode {
            GateMode::Strict => self.fail_step(
                ShipStep::Gate,
                format!("{msg}——闸门 strict 硬拦（先过整机工位全测）"),
                t0,
            ),
            _ => {
                // warn：过渡期旧工具测完的机器无台账记录，警告可继续
                self.warnings.push(format!("闸门警告：{msg}（旧工具测的机器属正常）"));
                self.done(ShipStep::Gate, StepVerdict::Warn, msg, t0);
                None
            }
        }
    }

    // —— ②③ SN 校验 + 台账反查 + SN 池 ——

    fn sn_checks(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::SnCheck);
        let re = match regex_lite::Regex::new(&self.profile.sn_pattern) {
            Ok(re) => re,
            Err(e) => return self.fail_step(ShipStep::SnCheck, format!("档案 SN 正则无效: {e}"), t0),
        };
        // ②-1 格式
        if !re.is_match(&self.sn) {
            return self.fail_step(
                ShipStep::SnCheck,
                format!("SN 格式不符（{}）: {}", self.profile.sn_pattern, self.sn),
                t0,
            );
        }
        // ②-2 异号防呆：配置现值优先（写号目标区），cf3 兜底
        let cfg_sn = self.current.get("sn").cloned().unwrap_or_default();
        let existing = if cfg_sn.is_empty() {
            self.device.sn.clone()
        } else {
            cfg_sn
        }
        .to_uppercase();
        let placeholders = &self.ctx.cfg.provision.placeholder_sns;
        let is_formal = |s: &str| !s.is_empty() && re.is_match(s);
        if is_formal(&existing) && existing != self.sn {
            return self.fail_step(
                ShipStep::SnCheck,
                format!("设备已写有另一正式 SN {existing}，目标 {}（异号硬拒——请核对工单/换机）", self.sn),
                t0,
            );
        }
        if is_formal(&existing) && existing == self.sn {
            self.re_run = true;
            self.warnings
                .push(format!("同号重写：设备已写有 SN {existing}（返修/断电续写场景）"));
        } else if placeholders.iter().any(|p| p.eq_ignore_ascii_case(&existing)) {
            // 占位 SN（工厂默认 00001 等）= 未写号，放行
        }
        // ②-3 台账反查
        let ledger = Ledger::load(&self.ctx.ledger_path);
        if let Some(rec) = ledger.find_by_shipped_sn(&self.sn) {
            if !rec.bt_mac.eq_ignore_ascii_case(&self.bt_mac) {
                return self.fail_step(
                    ShipStep::SnCheck,
                    format!(
                        "SN {} 已出厂绑到蓝牙 {}（首见 {}）——重号，发号系统核查",
                        self.sn, rec.bt_mac, rec.shipped_at
                    ),
                    t0,
                );
            }
            self.re_run = true;
            self.warnings.push(format!(
                "同号重写：SN {} 此前已在本机出厂写号（{}）",
                self.sn, rec.shipped_at
            ));
        }
        // ③ SN 池（已导入才启用）
        let stock = ProvisionStock::load(&self.ctx.stock_path);
        let note = if stock.has_sn_pool() {
            match stock.sn_entry(&self.sn) {
                None => {
                    return self.fail_step(
                        ShipStep::SnCheck,
                        format!("SN {} 不在池（本机已导入 SN 池，发号系统核查批次）", self.sn),
                        t0,
                    )
                }
                Some(e) if e.status == EntryStatus::Voided => {
                    return self.fail_step(ShipStep::SnCheck, format!("SN {} 已作废", self.sn), t0)
                }
                Some(e) if !e.bound_bt_mac.is_empty() && !e.bound_bt_mac.eq_ignore_ascii_case(&self.bt_mac) => {
                    return self.fail_step(
                        ShipStep::SnCheck,
                        format!("SN {} 已绑其他设备 {}", self.sn, e.bound_bt_mac),
                        t0,
                    )
                }
                Some(e) => format!("（池内 {}）", e.status.label()),
            }
        } else {
            "（无 SN 池，仅格式+台账校验）".to_string()
        };
        let v = if self.re_run { StepVerdict::Warn } else { StepVerdict::Pass };
        self.done(ShipStep::SnCheck, v, format!("校验通过{note}"), t0);
        None
    }

    // —— ④ 版本核对 ——

    fn versions(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Versions);
        if self.profile.t32_ver.trim().is_empty() {
            return self.fail_step(ShipStep::Versions, "配置缺失：档案 t32_ver 未填（工单版本）".into(), t0);
        }
        if self.device.t32_ver.trim() != self.profile.t32_ver.trim() {
            return self.fail_step(
                ShipStep::Versions,
                format!(
                    "固件版本不符：设备 [{}] ≠ 工单 [{}]",
                    self.device.t32_ver, self.profile.t32_ver
                ),
                t0,
            );
        }
        if self.profile.bt_ver.trim().is_empty() {
            return self.fail_step(ShipStep::Versions, "配置缺失：档案 bt_ver 未填".into(), t0);
        }
        let bt_cmd = CameraFactory::BtVersion.device_cmd();
        let bt = match self.shell(&bt_cmd) {
            Ok(o) => match protocol::parse_bt_version(&o) {
                Some(v) => v,
                None => {
                    return self.fail_step(
                        ShipStep::Versions,
                        format!("cf7 未解析到蓝牙版本（输出格式联调校准）: {}", clip(&o, 80)),
                        t0,
                    )
                }
            },
            Err(e) => return self.fail_step(ShipStep::Versions, format!("cf7 执行失败: {e}"), t0),
        };
        self.bt_ver_seen = bt.clone();
        if bt.trim() != self.profile.bt_ver.trim() {
            return self.fail_step(
                ShipStep::Versions,
                format!("蓝牙固件版本不符：设备 [{}] ≠ 工单 [{}]", bt, self.profile.bt_ver),
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

    // —— ⑤ 硬件终检 ——

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
                    format!("cf12 未解析到电压: {}", clip(&out, 80)),
                    t0,
                )
            }
        };
        self.battery_volts = Some(volts);
        if volts < self.profile.min_batt_volts {
            return self.fail_step(
                ShipStep::Hardware,
                format!("电池 {volts:.3}V < 阈值 {}V（充电后重试）", self.profile.min_batt_volts),
                t0,
            );
        }
        if self.profile.check_sdcard {
            let sd_cmd = CameraFactory::SdCard.device_cmd();
            let out = match self.shell(&sd_cmd) {
                Ok(o) => o,
                Err(e) => return self.fail_step(ShipStep::Hardware, format!("cf8 执行失败: {e}"), t0),
            };
            if !protocol::sdcard_ok(&out) {
                return self.fail_step(ShipStep::Hardware, "SD 卡不在位/失败".into(), t0);
            }
            self.done(
                ShipStep::Hardware,
                StepVerdict::Pass,
                format!("电池 {volts:.3}V ≥ {}V，SD 卡在位", self.profile.min_batt_volts),
                t0,
            );
        } else {
            self.done(
                ShipStep::Hardware,
                StepVerdict::Pass,
                format!("电池 {volts:.3}V ≥ {}V（SD 卡校验关闭）", self.profile.min_batt_volts),
                t0,
            );
        }
        None
    }

    // —— ⑥ 确认闸门 ——

    fn confirm_gate(&mut self) -> Option<String> {
        // 覆盖设备自生成凭据 = 告警（uuid/auth_key 现值非空且与目标不同）
        let cred = self.cred_preview();
        if self.profile.needs_credentials() {
            if let Some((uuid, key)) = &cred {
                for (key_name, target) in [("uuid", uuid), ("auth_key", key)] {
                    if let Some(cur) = self.current.get(key_name) {
                        if !cur.is_empty() && cur != target {
                            self.warnings.push(format!(
                                "覆盖设备自生成 {key_name}（现值 {}）",
                                clip_secret(cur)
                            ));
                        }
                    }
                }
            }
        }
        let auto = self.ctx.cfg.provision.auto_commit && self.warnings.is_empty();
        if auto {
            self.log("auto_commit 且无告警，自动开写");
            return None;
        }
        let card = ConfirmCard {
            sn: self.sn.clone(),
            bt_mac: self.bt_mac.clone(),
            model: self.model.label().into(),
            current: self.profile.keys.iter().map(|k| {
                let v = self.current.get(k).cloned().unwrap_or_default();
                (k.clone(), v)
            }).collect(),
            credential: cred,
            warnings: self.warnings.clone(),
            re_run: self.re_run,
        };
        let _ = self.tx.send(ShipEvent::Confirm(card));
        // 阻塞等应答；期间轮询停止请求（手动停止=取消）
        loop {
            match self.answers.recv_timeout(Duration::from_millis(200)) {
                Ok(ShipAnswer::Confirm(true)) => {
                    self.log("操作员确认，开写");
                    return None;
                }
                Ok(ShipAnswer::Confirm(false)) => return Some("操作员取消写号".into()),
                Err(RecvTimeoutError::Timeout) => {
                    if exec::abort_requested() {
                        return Some(format!("手动停止：{}", exec::abort_reason().text()));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Some("应答通道已关闭（取消）".into()),
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

    // —— ⑦ 凭据领用与预落盘 ——

    fn claim(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Claim);
        let now = report::time_str(SystemTime::now());
        let mut stock = ProvisionStock::load(&self.ctx.stock_path);
        if self.profile.needs_credentials() && self.cred.is_none() {
            if let Some(c) = stock.credential_bound_to(&self.sn).cloned() {
                self.cred = Some(c);
                self.log(format!("复原既有分配凭据 {}（同号重写）", clip_secret(&self.cred.as_ref().unwrap().uuid)));
            } else {
                match stock.claim_next_credential(&self.sn, &self.bt_mac, &now) {
                    Some(c) => self.cred = Some(c),
                    None => {
                        return self.fail_step(
                            ShipStep::Claim,
                            "凭据池空（导入凭据 txt 后重试；凭据档案缺池拒写）".into(),
                            t0,
                        )
                    }
                }
            }
        }
        stock.mark_sn_allocated(&self.sn, &self.bt_mac, &now);
        // 领用即落盘（FQC ⑦：首条写入前原子预落盘，断电不双发）
        if let Err(e) = stock.save(&self.ctx.stock_path) {
            return self.fail_step(ShipStep::Claim, format!("库存预落盘失败: {e:#}"), t0);
        }
        let detail = match &self.cred {
            Some(c) => format!("凭据 {} 已预落盘 allocated", clip_secret(&c.uuid)),
            None => "普通档案（无凭据）".into(),
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
                Err(e) => return self.fail_step(ShipStep::Write, format!("cf30 命令构造失败: {e:#}"), t0),
            };
            self.log(format!("执行 adb shell {cmd}"));
            let out = match self.shell(&cmd) {
                Ok(o) => o,
                Err(e) => return self.fail_step(ShipStep::Write, format!("cf30 写 {key} 执行失败: {e}"), t0),
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
        self.done(
            ShipStep::Write,
            StepVerdict::Pass,
            format!("{written} 键写入，{skipped} 键已一致跳过"),
            t0,
        );
        None
    }

    /// 写入键取值：sn=扫码值；uuid/auth_key=领用凭据；其余拒。
    fn value_for(&self, key: &str) -> Result<String, String> {
        match key {
            "sn" => Ok(self.sn.clone()),
            "uuid" | "auth_key" => match &self.cred {
                Some(c) => Ok(if key == "uuid" { c.uuid.clone() } else { c.auth_key.clone() }),
                None => Err(format!("键 {key} 需要凭据，但档案未配置凭据池/领取失败")),
            },
            other => Err(format!("档案键 {other} 未知（支持 sn/uuid/auth_key）")),
        }
    }

    // —— ⑨ 4G 模组配置写入（eSIM） ——

    fn fourg(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::FourG);
        let Some(fg) = self.profile.fourg.clone() else {
            self.done(ShipStep::FourG, StepVerdict::Skip, "档案未配置 4G（跳过）", t0);
            return None;
        };
        if fg.template.trim().is_empty() {
            // 空模板路径 = 未配置（内置默认即此态，工厂放好模板文件后填路径）
            self.done(ShipStep::FourG, StepVerdict::Skip, "4G 模板路径为空（跳过）", t0);
            return None;
        }
        let tpl_path = crate::paths::data_dir().join(&fg.template);
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
        // 整轮重试（CFGSTART 清空临时文件后从头跑；单行严禁原地重试）
        let mut attempt = 0;
        loop {
            match self.fourg_transaction(&fg, &lines) {
                Ok(()) => break,
                Err(e) if attempt == 0 => {
                    attempt += 1;
                    self.log(format!("4G 事务失败（{e}），CFGSTART 整轮重试 1 次"));
                }
                Err(e) => {
                    return self.fail_step(
                        ShipStep::FourG,
                        format!("4G 写入失败（已整轮重试 1 次）: {e}——不切正式版，留产测态返修"),
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
    fn fourg_transaction(&mut self, fg: &FourGCfg, lines: &[String]) -> Result<(), String> {
        self.fourg_diff.clear(); // 重试轮清掉上一轮的失败 diff
        let deadline = Instant::now() + FOURG_TOTAL_BUDGET;
        let phase = (|| -> Result<String, String> {
            self.fourg_at_ok(fg, "AT+PROD=1", 5, &deadline)?;
            self.fourg_at_ok(fg, "AT+CFGSTART", 5, &deadline)?;
            for l in lines {
                let at = format!("AT+CFGSET=\"{l}\"");
                self.fourg_at_ok(fg, &at, 5, &deadline)?;
            }
            self.fourg_at_ok(fg, "AT+CFGSAVE", 10, &deadline)?;
            self.fourg_at(fg, "AT+CFGREAD?", 10, &deadline)
        })();
        // 退出产测 best-effort：无论事务成败都尝试（失败只记日志）
        if let Err(e) = self.fourg_at_ok(fg, "AT+PROD=0", 5, &deadline) {
            self.log(format!("4g-test 退产测失败（忽略）: {e}"));
        }
        let readback = phase?;
        match compare_fourg_readback(&readback, lines) {
            None => Ok(()),
            Some(diff) => {
                self.fourg_diff = diff.clone();
                Err(format!("回读不一致（{} 行缺失）", diff.len()))
            }
        }
    }

    /// 单条 4g-test 调用（原始输出）。
    fn fourg_at(&self, fg: &FourGCfg, at: &str, secs: u64, deadline: &Instant) -> Result<String, String> {
        if Instant::now() >= *deadline {
            return Err("整体 60s 上限耗尽".into());
        }
        if at.chars().any(|c| c == '\'' || c.is_control()) {
            return Err(format!("AT 命令含单引号/控制字符（禁）: {at}"));
        }
        let mut cmd = String::from("4g-test");
        if !fg.port.trim().is_empty() {
            cmd.push_str(&format!(" -d {}", fg.port.trim()));
        }
        if fg.baud > 0 {
            cmd.push_str(&format!(" -b {}", fg.baud));
        }
        // AT 内含双引号（CFGSET="..."）：外层单引号包裹交给远端 shell
        let cmd = format!("{cmd} '{at}'");
        self.log(format!("4g-test: {at}"));
        adb::shell_timeout(&cmd, Duration::from_secs(secs)).map_err(|e| format!("{at} 失败: {e}"))
    }

    fn fourg_at_ok(&self, fg: &FourGCfg, at: &str, secs: u64, deadline: &Instant) -> Result<String, String> {
        let out = self.fourg_at(fg, at, secs, deadline)?;
        // 老版 adb 不透传退出码：按输出内容判定（OK 且无 ERROR）
        if out.contains("ERROR") || !out.contains("OK") {
            return Err(format!("{at} 模组未回 OK: {}", clip(&out, 80)));
        }
        Ok(out)
    }

    // —— ⑩ 终检 ——

    fn verify(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Verify);
        let cat = format!("cat {}", self.ctx.cfg.provision.config_path);
        let text = match self.shell(&cat) {
            Ok(t) => t,
            Err(e) => return self.fail_step(ShipStep::Verify, format!("终检 cat 失败: {e}"), t0),
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
        self.done(ShipStep::Verify, StepVerdict::Pass, "全部目标键复核一致", t0);
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
        // cf4 清 U 盘 + 切正式版：最后执行；失败不重启（留产测态返修）
        let cf4_cmd = CameraFactory::FactoryReset.device_cmd();
        let out = match self.shell(&cf4_cmd) {
            Ok(o) => o,
            Err(e) => {
                return self.fail_step(
                    ShipStep::Finalize,
                    format!("cf4 切正式版失败（不重启，留产测态）: {e}"),
                    t0,
                )
            }
        };
        if !protocol::output_ok(&out) {
            return self.fail_step(
                ShipStep::Finalize,
                format!("cf4 设备回报失败: {}", clip(&out, 80)),
                t0,
            );
        }
        // 预期重启标记：UI 屏蔽 devwatch 拔出中止
        let _ = self.tx.send(ShipEvent::Rebooting);
        self.log("cf4 完成，adb reboot（正式版重启后生效）");
        // reboot 命令常以连接断开报错——属预期，容忍
        let _ = adb::shell_timeout("reboot", Duration::from_secs(10));
        self.done(ShipStep::Finalize, StepVerdict::Pass, "已切正式版并重启", t0);
        None
    }

    // —— ⑫ 重启后探测 ——

    fn probe(&mut self) -> Option<String> {
        let t0 = self.begin(ShipStep::Probe);
        let secs = self.ctx.cfg.provision.reboot_verify_secs;
        if secs == 0 {
            self.done(
                ShipStep::Probe,
                StepVerdict::Skip,
                "reboot_verify_secs=0 跳过（正式版无 adb 场景）",
                t0,
            );
            return None;
        }
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut back = false;
        while Instant::now() < deadline {
            if exec::abort_requested() {
                return Some(format!("手动停止：{}", exec::abort_reason().text()));
            }
            if let Ok(devs) = adb::devices() {
                let hit = match &self.serial {
                    Some(s) => devs.iter().any(|d| d == s),
                    None => !devs.is_empty(),
                };
                if hit {
                    back = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_secs(2));
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
                    format!("重启后蓝牙 MAC 不一致：{} ≠ {}（换机/串号？）", d.bt_mac, self.bt_mac),
                    t0,
                )
            }
            Err(e) => return self.fail_step(ShipStep::Probe, format!("重启后 cf10 输出无法解析: {e}"), t0),
        }
        let cat = format!("cat {}", self.ctx.cfg.provision.config_path);
        let text = match self.shell(&cat) {
            Ok(t) => t,
            Err(e) => return self.fail_step(ShipStep::Probe, format!("重启后 cat 失败: {e}"), t0),
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
        self.done(ShipStep::Probe, StepVerdict::Pass, "重启后探测通过（MAC 与目标键一致）", t0);
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
            ledger.mark_shipped(
                &self.bt_mac,
                &partner,
                self.model.label(),
                &self.sn,
                &self.device.ccid,
                "PASS",
                &now,
            );
            if let Err(e) = ledger.save(&self.ctx.ledger_path) {
                self.log(format!("台账落盘失败: {e:#}"));
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
            s.push_str(&format!("fourg: 写入 {n} 行，回读{}\n", if self.fourg_diff.is_empty() { "一致" } else { "不一致" }));
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
            s.push_str(&format!(
                "{}:{}",
                r.step.label(),
                r.verdict.label()
            ));
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

/// 凭据脱敏截断（日志/错误行不落全值；报告与确认卡走全值）。
fn clip_secret(s: &str) -> String {
    let n = s.chars().count();
    if n <= 6 {
        "****".into()
    } else {
        format!("{}…（{} 字符）", &s[..4], n)
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
        if let Some(bad) = l.chars().find(|c| *c == '"' || *c == '\'' || c.is_control()) {
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
/// trim 归一后校验目标行全部在回读中（模组自带多余行不算失败）。
/// 返回缺失行（=diff，落报告）；None = 一致。
pub fn compare_fourg_readback(readback: &str, expected: &[String]) -> Option<Vec<String>> {
    let mut seen: Vec<String> = Vec::new();
    for raw in readback.lines() {
        let mut l = raw.trim();
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
    let missing: Vec<String> = expected
        .iter()
        .filter(|e| !seen.iter().any(|s| s == *e))
        .cloned()
        .collect();
    (!missing.is_empty()).then_some(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProvisionCfg;
    use std::sync::mpsc;

    /// 纯函数：模板解析与回读比对。
    #[test]
    fn fourg_template_parse_and_compare() {
        let tpl = "# 服务器配置\n\nserver=ota.example.com\napn=cmnet\nsn_line={{sn}}\n";
        let lines = parse_fourg_template(tpl, "6302W1000288").unwrap();
        assert_eq!(lines, vec!["server=ota.example.com", "apn=cmnet", "sn_line=6302W1000288"]);
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
        let missing = compare_fourg_readback("+CFGREAD: server=ota.example.com\nOK", &lines).unwrap();
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&"apn=cmnet".to_string()));
        // 模组多余行不算失败
        assert_eq!(
            compare_fourg_readback(
                "+CFGREAD: extra=1\n+CFGREAD: apn=cmnet\n",
                &["apn=cmnet".to_string()]
            ),
            None
        );
    }

    /// 端到端（fake adb + fake 4g-test 由同一 adb 桩内分发）：
    /// E02 全流程 PASS：身份→闸门(warn 过渡期)→SN→版本→硬件→确认(自动)
    /// →cf30 写 sn→终检→cf4→reboot(桩容错)→探测(60s 太长，置 0 跳过)
    /// →stock committed + ledger shipped + ship 报告。
    #[test]
    fn ship_flow_e02_pass_end_to_end() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let base = std::env::temp_dir().join(format!("e02-ship-{}", std::process::id()));
        let reports = base.join("reports");
        std::fs::create_dir_all(&reports).unwrap();

        // 假 adb：按子命令分发样例输出；cf30 写进状态文件、cat 回读状态
        // （30 的 case 必须排在 3 之前——cf30 命令串包含 "camera-factory 3" 前缀）
        let script = base.join("adb");
        std::fs::write(
            &script,
            &format!(
                "#!/bin/sh\nst=\"$(dirname \"$0\")/state.ini\"\ncase \"$*\" in\n\
                 *'camera-factory 30'*) k=$(echo \"$*\" | awk '{{print $(NF-1)}}'); v=$(echo \"$*\" | awk '{{print $NF}}'); \
                 grep -v \"^$k=\" \"$st\" > \"$st.tmp\" 2>/dev/null; mv \"$st.tmp\" \"$st\"; echo \"$k=$v\" >> \"$st\"; \
                 printf '[ProvisionInfo]\\n'; cat \"$st\"; printf 'config-update:true\\n';;\n\
                 *'camera-factory 10'*) printf 'MAC:0012A3B4C5D6\\n';;\n\
                 *'camera-factory 3'*) printf 'sn-address:00001\\nwifi-mac:0060A70B9CF2\\nsoft-ware:E02-1.0.1\\n';;\n\
                 *'camera-factory 7'*) printf 'bt-ver:V1_7_4\\n';;\n\
                 *'camera-factory 12'*) printf 'Bat vol:4.326\\n';;\n\
                 *'camera-factory 8'*) printf 'true\\n';;\n\
                 *'camera-factory 4'*) printf 'factory-reset true\\n';;\n\
                 *'camera-factory 19'*) printf 'ok\\n';;\n\
                 *'cat '* ) printf '[ProvisionInfo]\\n'; cat \"$st\" 2>/dev/null; printf '\\n';;\n\
                 *reboot*) exit 0;;\n\
                 *) printf 'ok\\n';;\nesac\n"
            ),
        )
        .unwrap();
        make_exec(&script);
        std::env::set_var("ADB", &script);

        // 配置：闸门 warn（台账为空=告警但继续）→ auto_commit=false 强制走确认
        let mut cfg = Config::default();
        cfg.binding.ledger_path = Some(base.join("ledger.json").display().to_string());
        let mut prov = ProvisionCfg::default();
        prov.active_profile = "e02".into();
        prov.gate_mode = GateMode::Warn;
        prov.auto_commit = true; // 闸门 warn 有告警 → 仍会弹确认（告警强制确认）
        prov.reboot_verify_secs = 0;
        prov.profiles.get_mut("e02").unwrap().t32_ver = "E02-1.0.1".into();
        prov.profiles.get_mut("e02").unwrap().bt_ver = "V1_7_4".into();
        cfg.provision = prov;
        let ctx = ShipCtx {
            stock_path: base.join("stock.json"),
            ledger_path: base.join("ledger.json"),
            reports_dir: reports.clone(),
            cfg: cfg.clone(),
        };

        let (tx, rx) = mpsc::channel();
        let (atx, arx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            run_ship("6302w1000288", None, &tx, &arx, &ctx)
        });

        let mut confirmed = false;
        let mut finished_pass = None;
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            match ev {
                ShipEvent::Confirm(card) => {
                    // 确认卡内容核对
                    assert_eq!(card.sn, "6302W1000288");
                    assert_eq!(card.bt_mac, "0012A3B4C5D6");
                    assert!(!card.re_run, "设备现值为空（未写号），非同号重写");
                    assert!(!card.warnings.is_empty(), "闸门 warn 应有告警");
                    confirmed = true;
                    atx.send(ShipAnswer::Confirm(true)).unwrap();
                }
                ShipEvent::Finished { pass, .. } => finished_pass = Some(pass),
                _ => {}
            }
        }
        let report = handle.join().unwrap().expect("应产出 ship 报告");
        assert!(finished_pass.unwrap(), "E02 全流程应 PASS");
        assert!(confirmed, "存在告警 → 必须人工确认（auto_commit 不越权）");

        // ship 报告内容
        let text = std::fs::read_to_string(&report).unwrap();
        assert!(text.contains("结果: PASS"));
        assert!(text.contains("工位: ship"));
        assert!(text.contains("sn-address:6302W1000288"));
        assert!(text.contains("bt-mac:0012A3B4C5D6"));
        assert!(text.contains("写入配置:PASS 1 键写入，0 键已一致跳过"));
        assert!(report.file_name().unwrap().to_string_lossy().starts_with("ship-"));

        // 台账回填（upsert：台账原本为空）
        let ledger = Ledger::load(&base.join("ledger.json"));
        let rec = ledger.find_by_shipped_sn("6302W1000288").unwrap();
        assert_eq!(rec.bt_mac, "0012A3B4C5D6");
        assert_eq!(rec.ship_verdict, "PASS");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 端到端失败路径：版本不符 → FAIL、不执行 cf4（桩里 cf4 会写标记文件）。
    #[test]
    fn ship_flow_version_mismatch_stops_before_cf4() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let base = std::env::temp_dir().join(format!("e02-ship-fail-{}", std::process::id()));
        let reports = base.join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        let cf4_mark = base.join("cf4-ran");

        let script = base.join("adb");
        std::fs::write(
            &script,
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n\
                 *'camera-factory 10'*) printf 'MAC:0012A3B4C5D6\\n';;\n\
                 *'camera-factory 3'*) printf 'sn-address:00001\\nsoft-ware:E02-9.9.9\\n';;\n\
                 *'camera-factory 4'*) touch {mark};;\n\
                 *'cat '* ) printf '[ProvisionInfo]\\nsn=\\n';;\n\
                 *) printf 'ok\\n';;\nesac\n",
                mark = cf4_mark.display()
            ),
        )
        .unwrap();
        make_exec(&script);
        std::env::set_var("ADB", &script);

        let mut cfg = Config::default();
        let mut prov = ProvisionCfg::default();
        prov.gate_mode = GateMode::Off; // 聚焦版本步骤
        prov.reboot_verify_secs = 0;
        prov.profiles.get_mut("e02").unwrap().t32_ver = "E02-1.0.1".into();
        prov.profiles.get_mut("e02").unwrap().bt_ver = "V1_7_4".into();
        cfg.provision = prov;
        let ctx = ShipCtx {
            stock_path: base.join("stock.json"),
            ledger_path: base.join("ledger.json"),
            reports_dir: reports.clone(),
            cfg,
        };

        let (tx, rx) = mpsc::channel();
        let (_atx, arx) = mpsc::channel();
        let handle = std::thread::spawn(move || run_ship("6302W1000288", None, &tx, &arx, &ctx));

        let mut failed = false;
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            if let ShipEvent::Finished { pass, .. } = ev {
                failed = !pass;
            }
        }
        assert!(handle.join().unwrap().is_some(), "失败也落 ship 报告留痕");
        assert!(failed);
        assert!(!cf4_mark.exists(), "失败路径严禁执行 cf4（留产测态）");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 端到端 eSIM：凭据档案 + 4G 事务（CFGREAD 首轮不一致 → 整轮重试通过）。
    #[test]
    fn ship_flow_e02e_credentials_and_4g_retry() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
        let base = std::env::temp_dir().join(format!("e02-ship-e02e-{}", std::process::id()));
        let reports = base.join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        // 4G 模板放"数据目录"——data_dir 不可注入，模板路径按 ShipCtx 无关的
        // data_dir 解析，测试里直接把模板写到 data_dir()/templates/ 下（进程内
        // 缓存的 exe 目录，测试二进制可写）。为避免污染，先读原值事后清理。
        let tpl_path = crate::paths::data_dir().join("templates").join("test_e02e_4g.ini");
        std::fs::create_dir_all(tpl_path.parent().unwrap()).unwrap();
        std::fs::write(&tpl_path, "# eSIM 4G\nserver=ota.example.com\napn=cmnet\nsn_line={{sn}}\n").unwrap();

        // 计数文件：CFGREAD 第 1 轮缺 apn 行，第 2 轮齐全（触发整轮重试）
        let cnt = base.join("cfgread-cnt");
        let script = base.join("adb");
        std::fs::write(
            &script,
            &format!(
                "#!/bin/sh\nst=\"$(dirname \"$0\")/state.ini\"\ncase \"$*\" in\n\
                 *'camera-factory 30'*) k=$(echo \"$*\" | awk '{{print $(NF-1)}}'); v=$(echo \"$*\" | awk '{{print $NF}}'); \
                 grep -v \"^$k=\" \"$st\" > \"$st.tmp\" 2>/dev/null; mv \"$st.tmp\" \"$st\"; echo \"$k=$v\" >> \"$st\"; \
                 printf '[ProvisionInfo]\\n'; cat \"$st\"; printf 'config-update:true\\n';;\n\
                 *'camera-factory 10'*) printf 'MAC:9999A3B4C5D6\\n';;\n\
                 *'camera-factory 3'*) printf 'sn-address:00001\\nsoft-ware:E02E-2.0.1\\n';;\n\
                 *'camera-factory 20'*) printf 'imei:864192051234567\\nccid:898608691025D0768737\\n';;\n\
                 *'camera-factory 7'*) printf 'bt-ver:V2_0_1\\n';;\n\
                 *'camera-factory 12'*) printf 'Vol:4.100,cur:0.200\\n';;\n\
                 *'camera-factory 8'*) printf 'true\\n';;\n\
                 *'camera-factory 4'*) printf 'true\\n';;\n\
                 *'cat '* ) printf '[ProvisionInfo]\\n'; cat \"$st\" 2>/dev/null; printf '\\n';;\n\
                 *CFGREAD*) n=$(cat \"{cnt}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{cnt}\"; \
                 if [ $n -lt 2 ]; then printf '+CFGREAD: server=ota.example.com\\nOK\\n'; \
                 else printf '+CFGREAD: server=ota.example.com\\n+CFGREAD: apn=cmnet\\n+CFGREAD: sn_line=6322E1001999\\nOK\\n'; fi;;\n\
                 *4g-test*) printf 'OK\\n';;\n\
                 *reboot*) exit 0;;\n\
                 *) printf 'ok\\n';;\nesac\n",
                cnt = cnt.display()
            ),
        )
        .unwrap();
        make_exec(&script);
        std::env::set_var("ADB", &script);

        // 凭据池 + 预置库存
        let mut stock = ProvisionStock::default();
        stock
            .import_sns("6322E1001999\n", r"^\d{4}E\d{7}$", "t0")
            .ok();
        stock
            .import_credentials(
                "uuid95fe79695a6df72f,B7uTB4QjoSLw55oHLk2wPSokAAwWXENH\n",
                r"^uuid[0-9a-f]{16}$",
                r"^[A-Za-z0-9]{32}$",
                "t0",
            )
            .ok();
        let stock_path = base.join("stock.json");
        stock.save(&stock_path).unwrap();

        let mut cfg = Config::default();
        let mut prov = ProvisionCfg::default();
        prov.active_profile = "e02e".into();
        prov.gate_mode = GateMode::Off;
        prov.auto_commit = true;
        prov.reboot_verify_secs = 0;
        {
            let p = prov.profiles.get_mut("e02e").unwrap();
            p.t32_ver = "E02E-2.0.1".into();
            p.bt_ver = "V2_0_1".into();
            p.keys = vec!["sn".into(), "uuid".into(), "auth_key".into()];
            p.fourg = Some(FourGCfg {
                port: String::new(), // 空 = 用 4g-test 默认（-d 不拼）
                baud: 0,
                template: "templates/test_e02e_4g.ini".into(),
            });
        }
        cfg.provision = prov;
        let ctx = ShipCtx {
            stock_path: stock_path.clone(),
            ledger_path: base.join("ledger.json"),
            reports_dir: reports.clone(),
            cfg,
        };

        let (tx, rx) = mpsc::channel();
        let (atx, arx) = mpsc::channel();
        let ctx_c = ctx;
        let handle =
            std::thread::spawn(move || run_ship("6322E1001999", None, &tx, &arx, &ctx_c));

        let mut finished_pass = None;
        let mut fourg_seen = false;
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            match ev {
                ShipEvent::Confirm(_) => atx.send(ShipAnswer::Confirm(true)).unwrap(),
                ShipEvent::StepResult(r) if r.step == ShipStep::FourG => fourg_seen = true,
                ShipEvent::Finished { pass, .. } => finished_pass = Some(pass),
                _ => {}
            }
        }
        let report = handle.join().unwrap().expect("eSIM 全流程应产出报告");
        assert!(finished_pass.unwrap());
        assert!(fourg_seen, "4G 步骤应执行");
        // 整轮重试确实发生（CFGREAD 计数 ≥2）
        let n: u32 = std::fs::read_to_string(&cnt).unwrap().trim().parse().unwrap();
        assert!(n >= 2, "首轮回读缺行应触发 CFGSTART 整轮重试，实测 CFGREAD {n} 次");

        let text = std::fs::read_to_string(&report).unwrap();
        assert!(text.contains("uuid:uuid95fe79695a6df72f"), "凭据全值落报告");
        assert!(text.contains("auth_key:B7uTB4QjoSLw55oHLk2wPSokAAwWXENH"));
        assert!(text.contains("imei:864192051234567"));
        assert!(text.contains("fourg: 写入 3 行，回读一致"));
        // 库存推进 committed
        let stock = ProvisionStock::load(&stock_path);
        assert_eq!(
            stock.sn_entry("6322E1001999").unwrap().status,
            EntryStatus::Committed
        );
        let cred = stock.credential_by_sn("6322E1001999").unwrap();
        assert_eq!(cred.status, EntryStatus::Committed);
        assert_eq!(cred.bound_bt_mac, "9999A3B4C5D6");
        // 台账按 IMEI 建档（eSIM 绑定对）
        let ledger = Ledger::load(&base.join("ledger.json"));
        let rec = ledger.find_by_shipped_sn("6322E1001999").unwrap();
        assert_eq!(rec.partner, "864192051234567");

        let _ = std::fs::remove_file(&tpl_path);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    fn make_exec(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    fn make_exec(_p: &Path) {}
}
