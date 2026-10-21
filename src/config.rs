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
/// throughput=驱动 + 速率（ping 网关统计丢包/延迟，阈值判定）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WifiDepth {
    #[default]
    Driver,
    Throughput,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThroughputCfg {
    #[serde(default = "d_ping_count")]
    pub ping_count: u32,
    #[serde(default = "d_loss_max")]
    pub loss_max_pct: f32,
    #[serde(default = "d_rtt_max")]
    pub rtt_max_ms: f32,
}

fn d_ping_count() -> u32 {
    20
}
fn d_loss_max() -> f32 {
    0.0
}
fn d_rtt_max() -> f32 {
    100.0
}

impl Default for ThroughputCfg {
    fn default() -> Self {
        Self {
            ping_count: d_ping_count(),
            loss_max_pct: d_loss_max(),
            rtt_max_ms: d_rtt_max(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WifiCfg {
    #[serde(default)]
    pub depth: WifiDepth,
    #[serde(default)]
    pub throughput: ThroughputCfg,
}

impl Default for WifiCfg {
    fn default() -> Self {
        Self {
            depth: WifiDepth::Driver,
            throughput: ThroughputCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeysCfg {
    /// 必测键码：4=多功能 5=开机 6=音量
    #[serde(default = "d_keys_required")]
    pub required: Vec<u8>,
    /// 严格解析：只认独立数字行（防版本号/电压假阳性）
    #[serde(default)]
    pub strict: bool,
    /// 提示窗口时长参考值（实际监听窗口由固件命令决定）
    #[serde(default = "d_listen_secs")]
    pub listen_secs: u32,
}

fn d_keys_required() -> Vec<u8> {
    vec![4, 5, 6]
}
fn d_listen_secs() -> u32 {
    10
}

impl Default for KeysCfg {
    fn default() -> Self {
        Self {
            required: d_keys_required(),
            strict: false,
            listen_secs: d_listen_secs(),
        }
    }
}

/// 陀螺仪：固件暂无 camera-factory 命令，用 shell 命令采样对比。
/// 固件日后支持后改 `cmd` 即可，程序不用重发版。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GyroCfg {
    #[serde(default = "d_gyro_cmd")]
    pub cmd: String,
    /// 两次采样间隔（毫秒）
    #[serde(default = "d_gyro_interval")]
    pub interval_ms: u64,
}

fn d_gyro_cmd() -> String {
    "dumpsys sensorservice | grep -i -A 3 'gyroscope'".into()
}
fn d_gyro_interval() -> u64 {
    500
}

impl Default for GyroCfg {
    fn default() -> Self {
        Self {
            cmd: d_gyro_cmd(),
            interval_ms: d_gyro_interval(),
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
    #[serde(default)]
    pub gyro: GyroCfg,
    /// 超时（V2.3）
    #[serde(default)]
    pub timeouts: TimeoutsCfg,
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
            gyro: GyroCfg::default(),
            timeouts: TimeoutsCfg::default(),
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
        // 模组工位无充电/电池/按键/MIC/录像/陀螺仪
        let items = module.active_items();
        assert!(!items.contains(&TestItem::Charger));
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
        assert_eq!(cfg.active_items().len(), 6);
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
