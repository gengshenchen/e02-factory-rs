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
    /// 产线 SSID：吞吐测试时 wlan0 未入网，则经 camera-factory 20 下发连接
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
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("解析 {} 失败", path.display()))?;
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

    /// 当前工位启用的测试项（未知 slug 忽略）。
    pub fn active_items(&self) -> Vec<TestItem> {
        self.active_profile()
            .items
            .iter()
            .filter_map(|s| TestItem::from_slug(s))
            .collect()
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
        assert_eq!(cfg.active_items().len(), TestItem::ALL.len());
        let mut module = Config::builtin();
        module.station = "module".into();
        // 模组工位无电池/按键/MIC/录像/陀螺仪
        let items = module.active_items();
        assert!(!items.contains(&TestItem::Battery));
        assert!(!items.contains(&TestItem::Keys));
        assert!(items.contains(&TestItem::Wifi));
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
