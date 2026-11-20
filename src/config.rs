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
/// E02 = WiFi 型（蓝牙↔WiFi MAC 绑定）；E02E = eSIM 型（无 WiFi 模组，
/// 蓝牙↔IMEI 绑定，`camera-factory 20` 读 IMEI/CCID，测陀螺仪/IMEI/CCID）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelKind {
    #[default]
    E02,
    #[serde(rename = "E02E")]
    E02e,
}

impl ModelKind {
    pub fn label(self) -> &'static str {
        match self {
            ModelKind::E02 => "E02",
            ModelKind::E02e => "E02E",
        }
    }

    /// 报告文件名前缀（e02e- 与 e02- 前缀互不覆盖）。
    pub fn file_prefix(self) -> &'static str {
        match self {
            ModelKind::E02 => "e02",
            ModelKind::E02e => "e02e",
        }
    }

    pub fn has_wifi(self) -> bool {
        matches!(self, ModelKind::E02)
    }

    /// 按固件名识别型号：`soft-ware:E02E-1.0.1` 前缀匹配，长前缀优先
    /// （E02E 先于 E02，否则 E02 恒中）。仅用于型号不符提醒（V3.14.1：
    /// E02E 固件名规律未证实，不作切换/判定依据）。
    pub fn detect(soft_ver: &str) -> Option<ModelKind> {
        let v = soft_ver.trim().to_ascii_uppercase();
        [ModelKind::E02e, ModelKind::E02]
            .into_iter()
            .find(|m| v.starts_with(m.label()))
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
    /// 内置默认（含两个工位档案）。
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
        // 写出的整机档不含 esim——加载时清残留，防止工位档案里一直显示
        // e02e、E02E 在整机工位跑不到 IMEI/CCID。下次设置改动落盘即固化。
        if cfg.profiles.remove("e02e").is_some() && cfg.station == "e02e" {
            cfg.station = "complete".into();
        }
        if let Some(complete) = cfg.profiles.get_mut("complete") {
            if !complete.items.iter().any(|s| s == "esim") {
                complete.items.push("esim".into());
            }
        }
        if !cfg.profiles.contains_key(&cfg.station) {
            anyhow::bail!(
                "配置工位 `{}` 不存在（可选: {}）",
                cfg.station,
                cfg.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        Ok(cfg)
    }

    /// 当前工位档案。
    pub fn active_profile(&self) -> &Profile {
        &self.profiles[&self.station]
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
        assert_eq!(ModelKind::detect("e02-1.0.1"), Some(ModelKind::E02));
        assert_eq!(ModelKind::detect(" E02E-V2 "), Some(ModelKind::E02e));
        assert_eq!(ModelKind::detect("E03-9.9"), None);
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
        // station 不在 profiles → Config::load 报错
        let dir = std::env::temp_dir().join(format!("e02-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(&p, "station = \"nope\"\n").unwrap();
        assert!(Config::load(&p).is_err());
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
