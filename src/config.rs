//! 工位配置（V2.2）：`config.toml` 加载/内置默认/序列化。
//!
//! 两个内置工位档案：模组工位（无整机，浅测）与整机工位（全项）。
//! 文件不存在时用内置默认；字段全部带默认值，用户文件可只写差异项。

use crate::engine::TestItem;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// WiFi 测试深度：driver=驱动通（camera-factory 5）；
/// throughput=驱动 + iperf3 吞吐（上行/下行 Mbps 与 TCP 重传，阈值判定）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WifiDepth {
    #[default]
    Driver,
    Throughput,
}

/// 吞吐测试方向。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IperfDir {
    Up,
    Down,
    Both,
}

/// iperf3 吞吐测试配置（V3.12，替代 V2.2 的 ping 丢包/延迟）。
///
/// 无屏蔽箱/共享 AP 环境下阈值要宽松 + 复测兜底：主判每秒窗口吞吐的
/// 中位数（抗共享信道瞬时抢道），TCP 重传对弱射频最灵敏但共享环境
/// 波动大，默认宽（500，0=不判），建议用金机实测校准。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThroughputCfg {
    /// 测试方向（耳机产品方向是下行，但虚焊两个方向都能暴露）
    #[serde(default = "d_dir")]
    pub direction: IperfDir,
    /// 单方向时长（秒）
    #[serde(default = "d_duration")]
    pub duration_secs: u32,
    /// 上行最低吞吐（Mbps，窗口中位数，0=不判）
    #[serde(default = "d_min_up")]
    pub min_mbps_up: f32,
    /// 下行最低吞吐（Mbps，窗口中位数，0=不判）
    #[serde(default = "d_min_down")]
    pub min_mbps_down: f32,
    /// TCP 重传上限（0=不判）
    #[serde(default = "d_max_retr")]
    pub max_retr: u32,
    /// 边缘失败（低于阈值但在 50% 裕量内）自动复测一次，仍低才 FAIL
    #[serde(default = "d_retest")]
    pub retest: bool,
    /// PC 端 iperf3 服务端口（0=自动找空闲）
    #[serde(default = "d_port")]
    pub server_port: u16,
}

fn d_dir() -> IperfDir {
    IperfDir::Both
}
fn d_duration() -> u32 {
    5
}
fn d_min_up() -> f32 {
    10.0
}
fn d_min_down() -> f32 {
    15.0
}
fn d_max_retr() -> u32 {
    500
}
fn d_retest() -> bool {
    true
}
fn d_port() -> u16 {
    0
}

impl Default for ThroughputCfg {
    fn default() -> Self {
        Self {
            direction: d_dir(),
            duration_secs: d_duration(),
            min_mbps_up: d_min_up(),
            min_mbps_down: d_min_down(),
            max_retr: d_max_retr(),
            retest: d_retest(),
            server_port: d_port(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WifiCfg {
    #[serde(default)]
    pub depth: WifiDepth,
    /// 产线 SSID：吞吐测试时 wlan0 未入网，则推送
    /// /config/IConfig/wpa_supplicant.conf 并跑 camera-factory 5 重连
    /// （耳机是 MIPS/uClibc Linux，无 Android cmd wifi 可用）
    #[serde(default)]
    pub ssid: String,
    /// 产线 AP 密码（开放网络留空）
    #[serde(default)]
    pub psk: String,
    #[serde(default)]
    pub throughput: ThroughputCfg,
}

impl Default for WifiCfg {
    fn default() -> Self {
        Self {
            depth: WifiDepth::Driver,
            ssid: String::new(),
            psk: String::new(),
            throughput: ThroughputCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeysCfg {
    /// 必测键码：4=多功能键 5=开机键/音量-（复用按键） 6=音量+
    #[serde(default = "d_keys_required")]
    pub required: Vec<u8>,
}

fn d_keys_required() -> Vec<u8> {
    vec![4, 5, 6]
}

impl Default for KeysCfg {
    fn default() -> Self {
        Self {
            required: d_keys_required(),
        }
    }
}

/// 超时（V2.3 真机反馈：固件命令可能永久阻塞导致序列卡死）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeoutsCfg {
    /// 单项测试命令最长执行时间（秒）；超时强杀进程判 FAIL 并继续；0 = 不限制
    #[serde(default = "d_item_secs")]
    pub item_secs: u64,
    /// 人工判定/按键应答等待（秒），超时自动判失败；0 = 不限制
    #[serde(default = "d_manual_secs")]
    pub manual_secs: u64,
}

fn d_item_secs() -> u64 {
    60
}
fn d_manual_secs() -> u64 {
    60
}

impl Default for TimeoutsCfg {
    fn default() -> Self {
        Self {
            item_secs: d_item_secs(),
            manual_secs: d_manual_secs(),
        }
    }
}

/// 连接（V3.8 设备在位监测）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConnCfg {
    /// 检测到设备插入（USB/adb 上线）即自动开始全测
    #[serde(default)]
    pub auto_start: bool,
}

/// 产品型号（V3.14）：绑定对与测试项按型号区分。
/// E02W = WiFi 版（蓝牙↔WiFi MAC 绑定）；E02E = eSIM 版（无 WiFi 模组，
/// 蓝牙↔IMEI 绑定，`camera-factory 20` 读 IMEI/CCID，测陀螺仪/IMEI/CCID）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelKind {
    #[default]
    #[serde(rename = "E02W", alias = "E02")]
    E02w,
    #[serde(rename = "E02E")]
    E02e,
}

impl ModelKind {
    pub fn label(self) -> &'static str {
        match self {
            ModelKind::E02w => "E02W",
            ModelKind::E02e => "E02E",
        }
    }

    /// 报告文件名前缀（e02w- 与 e02e- 互不覆盖；扫描/列表统一匹配 `e02` 前缀族以兼容历史 `e02-`）。
    pub fn file_prefix(self) -> &'static str {
        match self {
            ModelKind::E02w => "e02w",
            ModelKind::E02e => "e02e",
        }
    }

    pub fn has_wifi(self) -> bool {
        matches!(self, ModelKind::E02w)
    }

    /// SN 示例（界面与设置提示用）。
    pub fn sn_example(self) -> &'static str {
        match self {
            ModelKind::E02w => "6472W1000016",
            ModelKind::E02e => "6472E1000012",
        }
    }

    /// 按固件名识别型号：`soft-ware:E02E-1.0.1` / `E02W-1.0.1` / `E02-1.0.1`
    /// 长前缀优先（E02E / E02W 先于旧固件前缀 E02）。仅用于型号不符提醒。
    pub fn detect(soft_ver: &str) -> Option<ModelKind> {
        let v = soft_ver.trim().to_ascii_uppercase();
        if v.starts_with("E02E") {
            Some(ModelKind::E02e)
        } else if v.starts_with("E02W") || v.starts_with("E02") {
            Some(ModelKind::E02w)
        } else {
            None
        }
    }
}

/// 设备绑定台账（V3.13）：SN 未烧写（全厂默认 00001）不可作报告键/设备
/// 标识，报告文件名改用蓝牙 MAC；台账记录"蓝牙 MAC ↔ WiFi MAC"绑定，
/// 防工厂烧错地址（多台机器同蓝牙 MAC）。同对复测放行，同址不同对告警。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BindingCfg {
    /// 启用绑定校验（关闭后不产生"MAC 绑定"结果行，报告键仍为蓝牙 MAC）
    #[serde(default = "d_binding_enable")]
    pub enable: bool,
    /// 台账文件路径；缺省 = 数据目录 ledger.json（config.toml 同目录）。
    /// 多台测试 PC 共账时指到共享盘。
    #[serde(default)]
    pub ledger_path: Option<String>,
}

fn d_binding_enable() -> bool {
    true
}

impl Default for BindingCfg {
    fn default() -> Self {
        Self {
            enable: d_binding_enable(),
            ledger_path: None,
        }
    }
}

/// 工位档案：启用的测试项（slug 列表，顺序即执行顺序参考）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub items: Vec<String>,
}

/// 出厂闸门模式（FQC §3 ①）：写号前查本机整机工位是否全测 PASS。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateMode {
    /// 硬拦：查无台账记录或整机非全 PASS 一律拒写（台账覆盖全后切此档）
    Strict,
    /// **默认**：警告可继续（过渡期旧工具测完的机器无台账记录）
    #[default]
    Warn,
    /// 跳过闸门检查
    Off,
}

/// 4G 模组配置写入（FQC §3.1，E02E eSIM 型号专用）。
/// 4g-test 是设备端 CLI（随固件落地，同 camera-factory），PC 经
/// `adb shell` 转发，PC 不直接碰串口（2026-11-24 确认）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FourGCfg {
    /// 显式启用开关：开启后模板为空/缺失/写入失败一律判 FAIL 硬拦，绝不静默漏写；
    /// 关闭时整步明确跳过（4g-test 固件未到位过渡期关闭）。
    #[serde(default)]
    pub enable: bool,
    /// 设备端串口；留空 = 用 4g-test 自身默认（/dev/ttyS2）
    #[serde(default = "d_fourg_port")]
    pub port: String,
    /// 波特率；0 = 用 4g-test 自身默认（115200）
    #[serde(default = "d_fourg_baud")]
    pub baud: u32,
    /// 静态模板（数据目录相对路径，如 `templates/e02e_4g.ini`）
    #[serde(default = "d_fourg_tpl")]
    pub template: String,
}

fn d_fourg_port() -> String {
    "/dev/ttyS2".into()
}
fn d_fourg_baud() -> u32 {
    115200
}
fn d_fourg_tpl() -> String {
    "templates/e02e_4g.ini".into()
}

impl Default for FourGCfg {
    fn default() -> Self {
        Self {
            enable: false,
            port: d_fourg_port(),
            baud: d_fourg_baud(),
            template: d_fourg_tpl(),
        }
    }
}

/// 单个产品型号下的出厂写号参数（跟随全局 `Config.model` 切换，消除双轨型号打架）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionProfile {
    /// 所属产品型号（兼容旧 profiles 表反序列化）
    #[serde(default)]
    pub model: ModelKind,
    /// 工单 T32 软件版本（完全相等判定；留空 = 配置缺失拒写）
    #[serde(default)]
    pub t32_ver: String,
    /// 工单蓝牙固件版本（完全相等判定；留空 = 配置缺失拒写）
    #[serde(default)]
    pub bt_ver: String,
    /// 电池最低电压（V）
    #[serde(default = "d_min_batt_volts")]
    pub min_batt_volts: f32,
    /// 是否校验 SD 卡在位
    #[serde(default = "d_check_sdcard")]
    pub check_sdcard: bool,
    /// 正式 SN 格式（TOML 单引号字面量串，单反斜杠）
    #[serde(default = "d_sn_pattern")]
    pub sn_pattern: String,
    /// 写入键集（顺序即写序；凭据模式 = ["sn","uuid","auth_key"]）
    #[serde(default = "d_prov_keys")]
    pub keys: Vec<String>,
    /// 凭据格式校验（启用 uuid/auth_key 时生效）
    #[serde(default = "d_uuid_pattern")]
    pub uuid_pattern: String,
    #[serde(default = "d_key_pattern")]
    pub key_pattern: String,
    /// 4G 模组配置（E02E 型号使用）
    #[serde(default)]
    pub fourg: Option<FourGCfg>,
}

fn d_min_batt_volts() -> f32 {
    3.5
}
fn d_check_sdcard() -> bool {
    true
}
fn d_sn_pattern() -> String {
    r"^\d{4}W\d{7}$".into()
}
fn d_prov_keys() -> Vec<String> {
    vec!["sn".into()]
}
fn d_uuid_pattern() -> String {
    r"^uuid[0-9a-f]{16}$".into()
}
fn d_key_pattern() -> String {
    r"^[A-Za-z0-9]{32}$".into()
}

impl ProvisionProfile {
    pub fn default_e02w() -> Self {
        Self {
            model: ModelKind::E02w,
            t32_ver: String::new(),
            bt_ver: String::new(),
            min_batt_volts: d_min_batt_volts(),
            check_sdcard: d_check_sdcard(),
            sn_pattern: r"^\d{4}W\d{7}$".into(),
            keys: vec!["sn".into()],
            uuid_pattern: d_uuid_pattern(),
            key_pattern: d_key_pattern(),
            fourg: None,
        }
    }

    pub fn default_e02e() -> Self {
        Self {
            model: ModelKind::E02e,
            t32_ver: String::new(),
            bt_ver: String::new(),
            min_batt_volts: d_min_batt_volts(),
            check_sdcard: d_check_sdcard(),
            sn_pattern: r"^\d{4}E\d{7}$".into(),
            keys: vec!["sn".into()],
            uuid_pattern: d_uuid_pattern(),
            key_pattern: d_key_pattern(),
            fourg: Some(FourGCfg::default()),
        }
    }

    /// 是否启用客户凭据写入（uuid/auth_key）。
    pub fn needs_credentials(&self) -> bool {
        self.keys.iter().any(|k| k == "uuid" || k == "auth_key")
    }
}

/// 出厂写号配置（公共参数 + 按 `ModelKind` 分档的 `e02w` / `e02e`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionCfg {
    #[serde(default)]
    pub gate_mode: GateMode,
    /// 校验无异常时自动开始写入（有提醒仍暂停确认）；false = 每台人工确认
    #[serde(default = "d_auto_commit")]
    pub auto_commit: bool,
    /// 重启后回读等待秒数；0 = 跳过
    #[serde(default = "d_reboot_verify")]
    pub reboot_verify_secs: u64,
    /// 设备端 ini 绝对路径
    #[serde(default = "d_config_path")]
    pub config_path: String,
    /// 出厂前占位 SN（未写号默认值）
    #[serde(default = "d_placeholder_sns")]
    pub placeholder_sns: Vec<String>,
    /// E02W（WiFi 版）写号参数（alias 兼容旧 `[provision.e02]` 配置）
    #[serde(default = "ProvisionProfile::default_e02w", alias = "e02")]
    pub e02w: ProvisionProfile,
    /// E02E（eSIM 版）写号参数
    #[serde(default = "ProvisionProfile::default_e02e")]
    pub e02e: ProvisionProfile,
    /// 兼容迁移早期 `[provision.profiles.*]` 表（加载时并入 e02w/e02e，不再写出）
    #[serde(default, skip_serializing)]
    pub profiles: BTreeMap<String, ProvisionProfile>,
}

fn d_auto_commit() -> bool {
    true
}
fn d_reboot_verify() -> u64 {
    60
}
fn d_config_path() -> String {
    "/config/webrtc_serno_file.ini".into()
}
fn d_placeholder_sns() -> Vec<String> {
    vec!["00001".into(), "002".into()]
}

impl Default for ProvisionCfg {
    fn default() -> Self {
        Self {
            gate_mode: GateMode::default(),
            auto_commit: d_auto_commit(),
            reboot_verify_secs: d_reboot_verify(),
            config_path: d_config_path(),
            placeholder_sns: d_placeholder_sns(),
            e02w: ProvisionProfile::default_e02w(),
            e02e: ProvisionProfile::default_e02e(),
            profiles: BTreeMap::new(),
        }
    }
}

impl ProvisionCfg {
    /// 按全局选定型号获取对应的出厂参数。
    pub fn for_model(&self, model: ModelKind) -> &ProvisionProfile {
        match model {
            ModelKind::E02w => &self.e02w,
            ModelKind::E02e => &self.e02e,
        }
    }

    pub fn for_model_mut(&mut self, model: ModelKind) -> &mut ProvisionProfile {
        match model {
            ModelKind::E02w => &mut self.e02w,
            ModelKind::E02e => &mut self.e02e,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// 当前工位名（须存在于 profiles）
    #[serde(default = "d_station")]
    pub station: String,
    #[serde(default = "d_profiles")]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub wifi: WifiCfg,
    #[serde(default)]
    pub keys: KeysCfg,
    /// 超时（V2.3）
    #[serde(default)]
    pub timeouts: TimeoutsCfg,
    /// 连接/在位监测（V3.8）
    #[serde(default)]
    pub conn: ConnCfg,
    /// 设备绑定台账（V3.13）
    #[serde(default)]
    pub binding: BindingCfg,
    /// 型号（V3.14：E02 WiFi 型 / E02E eSIM 型；手动选定，缺省 E02）
    #[serde(default)]
    pub model: Option<ModelKind>,
    /// 出厂写号（FQC）
    #[serde(default)]
    pub provision: ProvisionCfg,
}

fn d_station() -> String {
    "complete".into()
}

fn d_profiles() -> BTreeMap<String, Profile> {
    let mut m = BTreeMap::new();
    m.insert(
        "module".into(),
        Profile {
            items: vec!["info", "btmac", "wifi", "sdcard", "photo", "led"]
                .into_iter()
                .map(String::from)
                .collect(),
        },
    );
    // 整机工位 = 全项（V3.14.1：工位只有模组/整机两档，型号是产品属性
    // 不建工位——项集按型号在展示/执行层过滤：E02 跑 WiFi，E02E 跑
    // IMEI/CCID，互不可用项各自隐藏）
    m.insert(
        "complete".into(),
        Profile {
            items: TestItem::ALL.iter().map(|i| i.slug().to_string()).collect(),
        },
    );
    m
}

impl Default for Config {
    fn default() -> Self {
        Self {
            station: d_station(),
            profiles: d_profiles(),
            wifi: WifiCfg::default(),
            keys: KeysCfg::default(),
            timeouts: TimeoutsCfg::default(),
            conn: ConnCfg::default(),
            binding: BindingCfg::default(),
            model: None,
            provision: ProvisionCfg::default(),
        }
    }
}

/// 默认配置文件路径：数据目录 `config.toml`。
/// 数据目录 = exe 同目录（可写时，绿色包习惯）否则系统应用数据目录
/// （MSI/deb 安装场景，见 [`crate::paths`]）。
pub fn default_path() -> PathBuf {
    crate::paths::data_dir().join("config.toml")
}

impl Config {
    /// 出厂写号工位固定标识（与 `module` / `complete` 并列，在设置中选择并持久化）。
    pub const STATION_SHIP: &'static str = "ship";

    /// 工位中文名（UI 顶栏/设置页/报告中心统一展示）。
    pub fn station_label(station: &str) -> &str {
        match station {
            "module" => "模组工位",
            "complete" => "整机工位",
            Self::STATION_SHIP => "出厂写号",
            other => other,
        }
    }

    /// 当前是否处于出厂写号工位。
    pub fn is_ship_station(&self) -> bool {
        self.station == Self::STATION_SHIP
    }

    /// 内置默认（含模组/整机测试档及出厂写号配置）。
    pub fn builtin() -> Self {
        Self::default()
    }

    /// 从 `path` 加载；文件不存在返回内置默认（首启场景）。
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::builtin());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("读取 {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("解析 {} 失败", path.display()))?;
        // 迁移（V3.14.2）：早期未发布版把 E02E 误建成内置工位档、且当时
        // 写出的整机档不含 esim——加载时清残留。
        if cfg.profiles.remove("e02e").is_some() && cfg.station == "e02e" {
            cfg.station = "complete".into();
        }
        if cfg.station == "fqc" {
            cfg.station = Self::STATION_SHIP.into();
        }
        if let Some(complete) = cfg.profiles.get_mut("complete") {
            if !complete.items.iter().any(|s| s == "esim") {
                complete.items.push("esim".into());
            }
        }
        // 迁移早期 `[provision.profiles.e02/e02w/e02e]` 表结构到 `provision.e02w/e02e`
        if !cfg.provision.profiles.is_empty() {
            let old = std::mem::take(&mut cfg.provision.profiles);
            if let Some(p) = old.get("e02w").or_else(|| old.get("e02")) {
                cfg.provision.e02w = p.clone();
                cfg.provision.e02w.model = ModelKind::E02w;
            }
            if let Some(mut p) = old.get("e02e").cloned() {
                p.model = ModelKind::E02e;
                if p.fourg.is_none() {
                    p.fourg = Some(FourGCfg::default());
                }
                cfg.provision.e02e = p;
            }
        }
        cfg.provision.e02w.model = ModelKind::E02w;
        cfg.provision.e02e.model = ModelKind::E02e;
        if cfg.provision.e02e.fourg.is_none() {
            cfg.provision.e02e.fourg = Some(FourGCfg::default());
        }
        if cfg.station != Self::STATION_SHIP && !cfg.profiles.contains_key(&cfg.station) {
            let mut opts: Vec<String> = cfg.profiles.keys().cloned().collect();
            opts.push(Self::STATION_SHIP.into());
            anyhow::bail!(
                "配置工位 `{}` 不存在（可选: {}）",
                cfg.station,
                opts.join(", ")
            );
        }
        Ok(cfg)
    }

    /// 当前测试工位档案；若当前工位为 `ship`（出厂写号无测试项表），安全回退 `complete`。
    pub fn active_profile(&self) -> &Profile {
        self.profiles
            .get(&self.station)
            .or_else(|| self.profiles.get("complete"))
            .or_else(|| self.profiles.values().next())
            .expect("profiles 至少含内置档")
    }

    /// 当前工位启用的测试项（未知 slug 忽略）。设备信息与蓝牙 MAC 是
    /// 设备身份来源（报告键与绑定对的一半，V3.14.1 必测）：工位档案
    /// 漏勾也强制带上。
    pub fn active_items(&self) -> Vec<TestItem> {
        let mut items: Vec<TestItem> = self
            .active_profile()
            .items
            .iter()
            .filter_map(|s| TestItem::from_slug(s))
            .collect();
        for must in [TestItem::DeviceInfo, TestItem::BtMac] {
            if !items.contains(&must) {
                items.push(must);
            }
        }
        items
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_profiles_and_items() {
        let cfg = Config::builtin();
        assert_eq!(cfg.station, "complete");
        // 整机工位 = 全项（型号过滤在执行/展示层：E02 跑 WiFi、E02E 跑 IMEI/CCID）
        assert_eq!(cfg.active_items().len(), TestItem::ALL.len());
        assert!(cfg.active_items().contains(&TestItem::Wifi));
        assert!(cfg.active_items().contains(&TestItem::Esim));
        let mut module = Config::builtin();
        module.station = "module".into();
        // 模组工位无电池/按键/MIC/录像/陀螺仪/eSIM
        let items = module.active_items();
        assert!(!items.contains(&TestItem::Battery));
        assert!(!items.contains(&TestItem::Keys));
        assert!(items.contains(&TestItem::Wifi));
        assert!(!items.contains(&TestItem::Esim));
        // 必测项兜底（V3.14.1）：自建档漏勾 info/btmac 也强制补上
        let mut hand = Config::builtin();
        hand.profiles.insert(
            "hand".into(),
            Profile {
                items: vec!["led".into()],
            },
        );
        hand.station = "hand".into();
        let items = hand.active_items();
        assert!(items.contains(&TestItem::DeviceInfo));
        assert!(items.contains(&TestItem::BtMac));
    }

    #[test]
    fn v3141_e02e_station_migrated_on_load() {
        // 早期未发布版写出的现场：e02e 工位档 + 整机档缺 esim → 加载即迁移
        let dir = std::env::temp_dir().join(format!("e02-cfg-mig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(
            &p,
            "station = \"e02e\"\n\
             [profiles.e02e]\nitems = [\"info\", \"btmac\", \"gyro\", \"esim\"]\n\
             [profiles.complete]\nitems = [\"info\", \"btmac\", \"wifi\"]\n",
        )
        .unwrap();
        let cfg = Config::load(&p).unwrap();
        assert!(!cfg.profiles.contains_key("e02e"), "e02e 工位档应被清掉");
        assert_eq!(cfg.station, "complete");
        assert!(
            cfg.active_items().contains(&TestItem::Esim),
            "整机档应补上 esim"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_detect_longest_prefix_wins() {
        use super::ModelKind;
        assert_eq!(ModelKind::detect("E02E-1.0.1"), Some(ModelKind::E02e));
        assert_eq!(ModelKind::detect("E02W-1.0.1"), Some(ModelKind::E02w));
        assert_eq!(ModelKind::detect("e02-1.0.1"), Some(ModelKind::E02w));
        assert_eq!(ModelKind::detect(" E02E-V2 "), Some(ModelKind::E02e));
        assert_eq!(ModelKind::detect("E03-9.9"), None);
    }

    #[test]
    fn default_sn_patterns_match_real_wifi_and_esim_batches() {
        let cfg = Config::builtin();
        let re_w = regex_lite::Regex::new(&cfg.provision.e02w.sn_pattern).unwrap();
        let re_e = regex_lite::Regex::new(&cfg.provision.e02e.sn_pattern).unwrap();
        for sn in [
            "6472W1000016",
            "6472W1000024",
            "6472W1000032",
            "6472W1000040",
            "6472W1000057",
        ] {
            assert!(re_w.is_match(sn), "E02W 正则应匹配 {sn}");
            assert!(!re_e.is_match(sn), "E02E 正则不得误匹配 WiFi SN {sn}");
        }
        for sn in [
            "6472E1000012",
            "6472E1000020",
            "6472E1000038",
            "6472E1000046",
        ] {
            assert!(re_e.is_match(sn), "E02E 正则应匹配 {sn}");
            assert!(!re_w.is_match(sn), "E02W 正则不得误匹配 eSIM SN {sn}");
        }
    }

    #[test]
    fn legacy_e02_model_and_provision_section_migrate_to_e02w() {
        let legacy = "station = \"ship\"\nmodel = \"E02\"\n[provision.e02]\nt32_ver = \"V2.2.9\"\n";
        let cfg: Config = toml::from_str(legacy).unwrap();
        assert_eq!(cfg.model, Some(ModelKind::E02w));
        assert_eq!(cfg.provision.e02w.t32_ver, "V2.2.9");
        let dumped = cfg.to_toml().unwrap();
        assert!(dumped.contains("model = \"E02W\""));
        assert!(dumped.contains("[provision.e02w]"));
    }

    #[test]
    fn partial_file_uses_defaults() {
        // 用户文件只写差异项，其余走默认
        let cfg: Config = toml::from_str("station = \"module\"\n").unwrap();
        assert_eq!(cfg.station, "module");
        assert_eq!(cfg.wifi.depth, WifiDepth::Driver);
        assert_eq!(cfg.keys.required, vec![4, 5, 6]);
        assert_eq!(cfg.timeouts.item_secs, 60);
        assert!(!cfg.conn.auto_start);
        assert_eq!(cfg.active_items().len(), 6);
    }

    #[test]
    fn conn_auto_start_parses() {
        let cfg: Config = toml::from_str("[conn]\nauto_start = true\n").unwrap();
        assert!(cfg.conn.auto_start);
        // roundtrip 保持
        let back: Config = toml::from_str(&cfg.to_toml().unwrap()).unwrap();
        assert!(back.conn.auto_start);
    }

    #[test]
    fn station_validation_in_load() {
        // station 在 profiles 中存在 → 正常
        let ok: Config =
            toml::from_str("station = \"nope\"\n[profiles.nope]\nitems = [\"info\"]\n").unwrap();
        assert_eq!(ok.station, "nope");
        // station 不在 profiles 且非 ship → Config::load 报错
        let dir = std::env::temp_dir().join(format!("e02-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(&p, "station = \"nope\"\n").unwrap();
        assert!(Config::load(&p).is_err());
        // station = "ship"（出厂写号工位）→ 合法加载且 active_items 安全回退不 panic
        std::fs::write(&p, "station = \"ship\"\nmodel = \"E02E\"\n").unwrap();
        let ship_cfg = Config::load(&p).unwrap();
        assert!(ship_cfg.is_ship_station());
        assert_eq!(Config::station_label(&ship_cfg.station), "出厂写号");
        assert!(!ship_cfg.active_items().is_empty());
        assert_eq!(
            ship_cfg
                .provision
                .for_model(ship_cfg.model.unwrap())
                .sn_pattern,
            r"^\d{4}E\d{7}$"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrip_toml() {
        let cfg = Config::builtin();
        let text = cfg.to_toml().unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn load_missing_file_gives_builtin() {
        let cfg = Config::load(Path::new("/nonexistent/e02-config.toml")).unwrap();
        assert_eq!(cfg, Config::builtin());
    }
}
