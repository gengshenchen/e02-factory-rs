//! 设备绑定台账（V3.13）：SN 未烧写时全厂是默认值（00001），不能作
//! 报告键/设备标识——报告文件名改用蓝牙 MAC；本模块持久化
//! "蓝牙 MAC ↔ WiFi MAC"绑定关系，防工厂烧错地址（多台机器烧了同一
//! 个蓝牙 MAC）。判定规则：
//! - 同 (蓝牙, WiFi) 对再现 = 同一台复测，放行并计数；
//! - 同蓝牙不同 WiFi = 不同设备烧了同一蓝牙地址，判冲突告警；
//! - 同 WiFi 不同蓝牙 = 对称检查，同样告警（烧错 WiFi 地址是同类事故）。
//!
//! 冲突不改动台账（原机绑定保留），冲突证据由报告结果行留档。
//! 台账默认存数据目录 `ledger.json`（config.toml 同目录，绿色包=exe
//! 同目录）；多台测试 PC 共账时把 `binding.ledger_path` 指到共享盘
//! （产线串行测试，写冲突风险低）。

use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 一台设备的绑定记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerRecord {
    /// 蓝牙 MAC（台账主键；报告文件名同款）
    pub bt_mac: String,
    /// 绑定的 WiFi MAC
    pub wifi_mac: String,
    /// 最近一次回读的 SN（仅留证，不作键）
    #[serde(default)]
    pub sn: String,
    /// 首次入账（yyyymmdd-hhmmss）
    pub first_seen: String,
    /// 最近测试（yyyymmdd-hhmmss）
    pub last_seen: String,
    /// 测试次数（同机复测累加；单项补测也是一次）
    pub tests: u32,
    /// 最近一次整机判定（PASS/FAIL/跳过）
    #[serde(default)]
    pub last_verdict: String,
}

/// 绑定校验结论。
#[derive(Debug, Clone, PartialEq)]
pub enum BindCheck {
    /// 全新设备，已入账（tests=1）
    New,
    /// 同机复测（tests 为含本次的累计次数）
    Retest { tests: u32 },
    /// 该蓝牙 MAC 已绑定别的 WiFi（other = 台账里既有那条）
    BtConflict { other: LedgerRecord },
    /// 该 WiFi MAC 已绑定别的蓝牙
    WifiConflict { other: LedgerRecord },
}

/// 台账（蓝牙 MAC → 记录）。记录量 = 产线产量，整文件读写足够。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Ledger {
    #[serde(default)]
    devices: BTreeMap<String, LedgerRecord>,
}

/// 台账文件路径：`binding.ledger_path` 优先，缺省数据目录 ledger.json。
pub fn path_of(cfg: &Config) -> PathBuf {
    cfg.binding
        .ledger_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::paths::data_dir().join("ledger.json"))
}

impl Ledger {
    /// 从磁盘加载；文件缺失/损坏返回空台账（首启或手删场景，不阻断测试）。
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// 原子落盘（tmp + rename，防中途断电写半截）。
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// 最近测试的 `n` 条（last_seen 倒序；台账卡展示用）。
    pub fn recent(&self, n: usize) -> Vec<LedgerRecord> {
        let mut recs: Vec<LedgerRecord> = self.devices.values().cloned().collect();
        recs.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        recs.truncate(n);
        recs
    }

    /// 校验并入账（同机复测试数 +1、新机插一条；冲突不动台账）。
    /// 调用方保证 bt/wifi 非空（两地址由设备信息/蓝牙两项解析齐备）。
    pub fn bind(&mut self, bt: &str, wifi: &str, sn: &str, now: &str) -> BindCheck {
        let (bt, wifi) = (bt.trim().to_uppercase(), wifi.trim().to_uppercase());
        if let Some(rec) = self.devices.get_mut(&bt) {
            if rec.wifi_mac == wifi {
                rec.tests += 1;
                rec.last_seen = now.into();
                if !sn.trim().is_empty() {
                    rec.sn = sn.trim().to_uppercase();
                }
                return BindCheck::Retest { tests: rec.tests };
            }
            return BindCheck::BtConflict { other: rec.clone() };
        }
        if let Some(other) = self.devices.values().find(|r| r.wifi_mac == wifi).cloned() {
            return BindCheck::WifiConflict { other };
        }
        self.devices.insert(
            bt.clone(),
            LedgerRecord {
                bt_mac: bt,
                wifi_mac: wifi,
                sn: sn.trim().to_uppercase(),
                first_seen: now.into(),
                last_seen: now.into(),
                tests: 1,
                last_verdict: String::new(),
            },
        );
        BindCheck::New
    }

    /// 测后回填整机判定（bind 已入账/复测过的机器）。
    pub fn set_verdict(&mut self, bt: &str, verdict: &str, now: &str) {
        if let Some(rec) = self.devices.get_mut(bt.trim().to_uppercase().as_str()) {
            rec.last_verdict = verdict.into();
            rec.last_seen = now.into();
        }
    }

    /// 删单条（误录/返修换地址后清理）。
    pub fn remove(&mut self, bt: &str) -> bool {
        self.devices
            .remove(bt.trim().to_uppercase().as_str())
            .is_some()
    }

    pub fn clear(&mut self) {
        self.devices.clear();
    }

    /// 导出 CSV（UTF-8 BOM，Excel 双击直开）。表头中文与 config 注释同级语义。
    pub fn to_csv(&self) -> String {
        let mut s = String::from("\u{FEFF}蓝牙MAC,WiFi MAC,SN,首测,最近,测试次数,最近判定\n");
        let mut recs: Vec<&LedgerRecord> = self.devices.values().collect();
        recs.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        for r in recs {
            // MAC/SN/时间均为 ASCII 安全字符，无引号转义需求
            s.push_str(&format!(
                "{},{},{},{},{},{},{}\n",
                r.bt_mac, r.wifi_mac, r.sn, r.first_seen, r.last_seen, r.tests, r.last_verdict
            ));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_retest_and_bt_conflict() {
        let mut l = Ledger::default();
        // 新机入账（SN 留证）
        assert_eq!(
            l.bind("0011A3B4C5D6", "00:60:A7:0B:9C:F2", "00001", "t1"),
            BindCheck::New
        );
        // 同对再现 = 同机复测，计数累加
        assert_eq!(
            l.bind("0011a3b4c5d6", "00:60:a7:0b:9c:f2", "", "t2"),
            BindCheck::Retest { tests: 2 }
        );
        // 同蓝牙不同 WiFi = 烧错地址，冲突且不动台账
        let before = l.clone();
        assert!(matches!(
            l.bind("0011A3B4C5D6", "00:60:A7:0B:9C:99", "", "t3"),
            BindCheck::BtConflict { .. }
        ));
        assert_eq!(l, before);
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn wifi_conflict_symmetric() {
        let mut l = Ledger::default();
        l.bind("0011A3B4C5D6", "00:60:A7:0B:9C:F2", "", "t1");
        // 不同蓝牙占同一 WiFi 地址同样告警
        assert!(matches!(
            l.bind("9999A3B4C5D6", "00:60:A7:0B:9C:F2", "", "t2"),
            BindCheck::WifiConflict { .. }
        ));
        assert_eq!(l.len(), 1); // 冲突不入账
    }

    #[test]
    fn verdict_roundtrip_and_remove() {
        let mut l = Ledger::default();
        l.bind("0011A3B4C5D6", "AA", "", "t1");
        l.set_verdict("0011A3B4C5D6", "FAIL", "t2");
        assert_eq!(l.recent(5)[0].last_verdict, "FAIL");
        assert_eq!(l.recent(5)[0].last_seen, "t2");
        assert!(l.remove("0011a3b4c5d6")); // 大小写不敏感
        assert!(l.is_empty());
        assert!(!l.remove("0011A3B4C5D6"));
    }

    #[test]
    fn persistence_and_csv() {
        let dir = std::env::temp_dir().join(format!("e02-ledger-{}", std::process::id()));
        let path = dir.join("sub").join("ledger.json"); // save 需自建目录
        let mut l = Ledger::default();
        l.bind("0011A3B4C5D6", "AA", "00001", "t1");
        l.save(&path).unwrap();
        let loaded = Ledger::load(&path);
        assert_eq!(loaded, l);
        // 损坏文件 → 空台账不 panic
        std::fs::write(&path, "{broken").unwrap();
        assert!(Ledger::load(&path).is_empty());
        // CSV：BOM + 表头 + 倒序（最近在前）
        l.bind("9999A3B4C5D6", "BB", "", "t2");
        let csv = l.to_csv();
        assert!(csv.starts_with('\u{FEFF}'));
        assert!(csv.contains("蓝牙MAC,WiFi MAC,SN,首测,最近,测试次数,最近判定"));
        let b = csv.lines().nth(1).unwrap();
        assert!(b.starts_with("9999A3B4C5D6"), "{csv}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
