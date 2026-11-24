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
    /// 绑定值（绑定对的另一半）：E02 = WiFi MAC；E02E = IMEI（V3.14）。
    /// serde alias 兼容 V3.13 旧字段名 wifi_mac，升级不丢已测台账。
    #[serde(alias = "wifi_mac")]
    pub partner: String,
    /// 型号（E02/E02E；空 = V3.13 旧账，视作 E02）
    #[serde(default)]
    pub model: String,
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
    /// 最近测试工位（module/complete；FQC §7 闸门分档依据）
    #[serde(default)]
    pub last_station: String,
    /// 整机工位（complete）最近全测判定（FQC §3 ① 闸门：仅此为 PASS
    /// 才算满足——防仅过模组工位或单项补测绕过）
    #[serde(default)]
    pub complete_verdict: String,
    /// 整机全 PASS 时间（yyyymmdd-hhmmss）
    #[serde(default)]
    pub complete_pass_at: String,
    /// 出厂写号 SN（FQC §7：与测试回读 `sn` 隔离——返修重刷默认固件
    /// 00001 时 `bind()` 只覆盖 `sn`，不冲掉正式出厂 SN）
    #[serde(default)]
    pub shipped_sn: String,
    /// 出厂写号时间（yyyymmdd-hhmmss）
    #[serde(default)]
    pub shipped_at: String,
    /// 出厂写号判定（PASS/FAIL）
    #[serde(default)]
    pub ship_verdict: String,
    /// eSIM 型出厂读到的 CCID（E02 留空；出货明细 CSV 用，V3.16）
    #[serde(default)]
    pub ccid: String,
}

/// 绑定校验结论。
#[derive(Debug, Clone, PartialEq)]
pub enum BindCheck {
    /// 全新设备，已入账（tests=1）
    New,
    /// 同机复测（tests 为含本次的累计次数）
    Retest { tests: u32 },
    /// 该蓝牙 MAC 已绑定别的值（other = 台账里既有那条）
    BtConflict { other: LedgerRecord },
    /// 该绑定值（WiFi/IMEI）已绑定别的蓝牙（对称检查）
    PartnerConflict { other: LedgerRecord },
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
    /// partner = 绑定对另一半：E02 为 WiFi MAC，E02E 为 IMEI（V3.14）。
    /// station = 测试工位（FQC §7 记 last_station，整机判定另记）。
    /// 调用方保证 bt/partner 非空（由设备信息/蓝牙/eSIM 项解析齐备）。
    pub fn bind(
        &mut self,
        bt: &str,
        partner: &str,
        model: &str,
        sn: &str,
        station: &str,
        now: &str,
    ) -> BindCheck {
        let (bt, partner) = (bt.trim().to_uppercase(), partner.trim().to_uppercase());
        if let Some(rec) = self.devices.get_mut(&bt) {
            if rec.partner == partner {
                rec.tests += 1;
                rec.last_seen = now.into();
                rec.last_station = station.into();
                if !sn.trim().is_empty() {
                    rec.sn = sn.trim().to_uppercase();
                }
                return BindCheck::Retest { tests: rec.tests };
            }
            return BindCheck::BtConflict { other: rec.clone() };
        }
        if let Some(other) = self
            .devices
            .values()
            .find(|r| r.partner == partner)
            .cloned()
        {
            return BindCheck::PartnerConflict { other };
        }
        self.devices.insert(
            bt.clone(),
            LedgerRecord {
                bt_mac: bt,
                partner,
                model: model.into(),
                sn: sn.trim().to_uppercase(),
                first_seen: now.into(),
                last_seen: now.into(),
                tests: 1,
                last_verdict: String::new(),
                last_station: station.into(),
                complete_verdict: String::new(),
                complete_pass_at: String::new(),
                shipped_sn: String::new(),
                shipped_at: String::new(),
                ship_verdict: String::new(),
                ccid: String::new(),
            },
        );
        BindCheck::New
    }

    /// 测后回填整机判定（bind 已入账/复测过的机器）。整机工位
    /// （complete）同时更新 complete_verdict（FQC 闸门依据）。
    pub fn set_verdict(&mut self, bt: &str, verdict: &str, station: &str, now: &str) {
        if let Some(rec) = self.devices.get_mut(bt.trim().to_uppercase().as_str()) {
            rec.last_verdict = verdict.into();
            rec.last_station = station.into();
            rec.last_seen = now.into();
            if station == "complete" {
                rec.complete_verdict = verdict.into();
                if verdict == "PASS" {
                    rec.complete_pass_at = now.into();
                }
            }
        }
    }

    /// FQC 闸门（§3 ①）：本机整机工位是否全测 PASS。
    pub fn complete_pass(&self, bt: &str) -> bool {
        self.devices
            .get(&bt.trim().to_uppercase())
            .is_some_and(|r| r.complete_verdict == "PASS")
    }

    /// 按出厂 SN 反查（FQC §3 ②：该 SN 已绑其他设备 = 硬拦）。
    pub fn find_by_shipped_sn(&self, sn: &str) -> Option<&LedgerRecord> {
        let sn = sn.trim().to_uppercase();
        self.devices.values().find(|r| r.shipped_sn == sn)
    }

    /// 全部已出厂记录（按出厂时间升序；出货明细 CSV 关联 stock 用）。
    pub fn shipped(&self) -> Vec<LedgerRecord> {
        let mut recs: Vec<LedgerRecord> = self
            .devices
            .values()
            .filter(|r| !r.shipped_sn.is_empty())
            .cloned()
            .collect();
        recs.sort_by(|a, b| a.shipped_at.cmp(&b.shipped_at));
        recs
    }

    /// 出厂回填 upsert（FQC §7）：台账尚无该 BT MAC 记录时按本次读到的
    /// 身份自动建档（warn 闸门下旧工具测完过渡期的机器），再写出厂字段。
    /// ccid = eSIM 型出厂读到的 CCID（E02 传空）。
    pub fn mark_shipped(
        &mut self,
        bt: &str,
        partner: &str,
        model: &str,
        sn: &str,
        ccid: &str,
        verdict: &str,
        now: &str,
    ) {
        let (bt, partner) = (bt.trim().to_uppercase(), partner.trim().to_uppercase());
        if let Some(rec) = self.devices.get_mut(&bt) {
            rec.shipped_sn = sn.trim().to_uppercase();
            rec.shipped_at = now.into();
            rec.ship_verdict = verdict.into();
            rec.last_seen = now.into();
            if !ccid.trim().is_empty() {
                rec.ccid = ccid.trim().to_uppercase();
            }
            if rec.partner.is_empty() {
                rec.partner = partner;
            }
        } else {
            self.devices.insert(
                bt.clone(),
                LedgerRecord {
                    bt_mac: bt,
                    partner,
                    model: model.into(),
                    sn: String::new(),
                    first_seen: now.into(),
                    last_seen: now.into(),
                    tests: 0,
                    last_verdict: String::new(),
                    last_station: String::new(),
                    complete_verdict: String::new(),
                    complete_pass_at: String::new(),
                    shipped_sn: sn.trim().to_uppercase(),
                    shipped_at: now.into(),
                    ship_verdict: verdict.into(),
                    ccid: ccid.trim().to_uppercase(),
                },
            );
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
    /// 不含凭据（FQC §7）；新增工位/整机判定/出厂列（工位统计与闸门留痕）。
    pub fn to_csv(&self) -> String {
        let mut s = String::from("\u{FEFF}蓝牙MAC,绑定值(WiFi/IMEI),型号,SN,首测,最近,测试次数,最近判定,最近工位,整机判定,出厂SN,出厂时间\n");
        let mut recs: Vec<&LedgerRecord> = self.devices.values().collect();
        recs.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        for r in recs {
            // MAC/IMEI/SN/时间均为 ASCII 安全字符，无引号转义需求
            s.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{}\n",
                r.bt_mac,
                r.partner,
                r.model,
                r.sn,
                r.first_seen,
                r.last_seen,
                r.tests,
                r.last_verdict,
                r.last_station,
                r.complete_verdict,
                r.shipped_sn,
                r.shipped_at
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
            l.bind("0011A3B4C5D6", "00:60:A7:0B:9C:F2", "E02", "00001", "complete", "t1"),
            BindCheck::New
        );
        // 同对再现 = 同机复测，计数累加
        assert_eq!(
            l.bind("0011a3b4c5d6", "00:60:a7:0b:9c:f2", "E02", "", "complete", "t2"),
            BindCheck::Retest { tests: 2 }
        );
        // 同蓝牙不同绑定值 = 烧错地址，冲突且不动台账
        let before = l.clone();
        assert!(matches!(
            l.bind("0011A3B4C5D6", "00:60:A7:0B:9C:99", "E02", "", "complete", "t3"),
            BindCheck::BtConflict { .. }
        ));
        assert_eq!(l, before);
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn partner_conflict_symmetric() {
        let mut l = Ledger::default();
        l.bind("0011A3B4C5D6", "00:60:A7:0B:9C:F2", "E02", "", "complete", "t1");
        // 不同蓝牙占同一绑定值同样告警
        assert!(matches!(
            l.bind("9999A3B4C5D6", "00:60:A7:0B:9C:F2", "E02", "", "complete", "t2"),
            BindCheck::PartnerConflict { .. }
        ));
        assert_eq!(l.len(), 1); // 冲突不入账
    }

    /// E02E 绑定对 = 蓝牙↔IMEI（V3.14）：同引擎换 partner 字段即可。
    #[test]
    fn esim_binds_bt_to_imei() {
        let mut l = Ledger::default();
        assert_eq!(
            l.bind("0011A3B4C5D6", "860123456789012", "E02E", "", "complete", "t1"),
            BindCheck::New
        );
        // 同蓝牙不同 IMEI（双机同蓝牙地址）冲突
        assert!(matches!(
            l.bind("0011A3B4C5D6", "860999999999999", "E02E", "", "complete", "t2"),
            BindCheck::BtConflict { .. }
        ));
        // 同对复测放行
        assert_eq!(
            l.bind("0011A3B4C5D6", "860123456789012", "E02E", "", "complete", "t3"),
            BindCheck::Retest { tests: 2 }
        );
    }

    #[test]
    fn verdict_roundtrip_and_remove() {
        let mut l = Ledger::default();
        l.bind("0011A3B4C5D6", "AA", "E02", "", "complete", "t1");
        l.set_verdict("0011A3B4C5D6", "FAIL", "complete", "t2");
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
        l.bind("0011A3B4C5D6", "AA", "E02", "00001", "complete", "t1");
        l.save(&path).unwrap();
        let loaded = Ledger::load(&path);
        assert_eq!(loaded, l);
        // 损坏文件 → 空台账不 panic
        std::fs::write(&path, "{broken").unwrap();
        assert!(Ledger::load(&path).is_empty());
        // CSV：BOM + 表头 + 倒序（最近在前）
        l.bind("9999A3B4C5D6", "BB", "E02E", "", "complete", "t2");
        let csv = l.to_csv();
        assert!(csv.starts_with('\u{FEFF}'));
        assert!(csv.contains("蓝牙MAC,绑定值(WiFi/IMEI),型号,SN,首测,最近,测试次数,最近判定,最近工位"));
        let b = csv.lines().nth(1).unwrap();
        assert!(b.starts_with("9999A3B4C5D6,BB,E02E"), "{csv}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// V3.13 旧台账（字段名 wifi_mac、无 model）升级加载不丢账（alias）。
    #[test]
    fn v313_ledger_migrates_via_alias() {
        let old = r#"{"devices":{"0011A3B4C5D6":{"bt_mac":"0011A3B4C5D6","wifi_mac":"AA","sn":"00001","first_seen":"t1","last_seen":"t1","tests":3,"last_verdict":"PASS"}}}"#;
        let dir = std::env::temp_dir().join(format!("e02-ledger-old-{}", std::process::id()));
        let path = dir.join("ledger.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, old).unwrap();
        let mut l = Ledger::load(&path);
        assert_eq!(l.len(), 1);
        let rec = l.recent(1)[0].clone();
        assert_eq!(rec.partner, "AA"); // 旧 wifi_mac 读进 partner
        assert_eq!(rec.model, ""); // 旧账无型号 → 视作 E02
        assert_eq!(rec.tests, 3);
        // 同对复测在升级后照常累加
        assert_eq!(
            l.bind("0011A3B4C5D6", "AA", "E02", "", "complete", "t2"),
            BindCheck::Retest { tests: 4 }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FQC §3 ① 闸门：仅整机工位（complete）全 PASS 算过；模组工位/单项
    /// 补测/未测不算。
    #[test]
    fn gate_requires_complete_pass() {
        let mut l = Ledger::default();
        l.bind("0011A3B4C5D6", "AA", "E02", "", "module", "t1");
        l.set_verdict("0011A3B4C5D6", "PASS", "module", "t1");
        assert!(!l.complete_pass("0011A3B4C5D6"), "模组工位 PASS 不满足闸门");
        l.set_verdict("0011A3B4C5D6", "PASS", "complete", "t2");
        assert!(l.complete_pass("0011A3B4C5D6"));
        assert_eq!(l.recent(1)[0].complete_pass_at, "t2");
        assert_eq!(l.recent(1)[0].last_station, "complete");
        // 整机 FAIL 覆盖 PASS（返修重测）
        l.set_verdict("0011A3B4C5D6", "FAIL", "complete", "t3");
        assert!(!l.complete_pass("0011A3B4C5D6"));
        // 查无此机 = 不满足
        assert!(!l.complete_pass("UNKNOWN"));
    }

    /// FQC §7 mark_shipped upsert + shipped_sn 与测试 sn 隔离：
    /// 返修重刷默认固件 00001 时 bind() 只覆盖 sn，不冲掉出厂 SN。
    #[test]
    fn mark_shipped_upsert_and_sn_isolation() {
        let mut l = Ledger::default();
        // 过渡期机器：台账无记录 → upsert 自动建档
        l.mark_shipped("0011A3B4C5D6", "AA", "E02", "6302W1000248", "", "PASS", "t1");
        let rec = &l.recent(1)[0];
        assert_eq!(rec.shipped_sn, "6302W1000248");
        assert_eq!(rec.ship_verdict, "PASS");
        assert_eq!(rec.partner, "AA");
        assert_eq!(rec.tests, 0);
        // 已有记录：出厂字段回填，绑定值不动；ccid 仅 eSIM 型落值
        l.bind("0011A3B4C5D6", "AA", "E02", "00001", "complete", "t2");
        l.mark_shipped("0011A3B4C5D6", "AA", "E02", "6302W1000248", "", "PASS", "t3");
        let rec = &l.recent(1)[0];
        assert_eq!(rec.shipped_sn, "6302W1000248");
        assert_eq!(rec.shipped_at, "t3");
        assert_eq!(rec.ccid, "");
        // 隔离：返修重刷后 bind 回读 00001 覆盖测试 sn，出厂 SN 保留
        l.bind("0011A3B4C5D6", "AA", "E02", "00001", "complete", "t4");
        let rec = &l.recent(1)[0];
        assert_eq!(rec.sn, "00001");
        assert_eq!(rec.shipped_sn, "6302W1000248");
        // §3 ② 反查：该 SN 已绑本机；绑别的机可检出
        assert_eq!(
            l.find_by_shipped_sn("6302W1000248").unwrap().bt_mac,
            "0011A3B4C5D6"
        );
        l.mark_shipped("9999A3B4C5D6", "BB", "E02E", "6322E1001999", "898608691025D0767784", "PASS", "t5");
        assert_eq!(
            l.find_by_shipped_sn("6322E1001999").unwrap().bt_mac,
            "9999A3B4C5D6"
        );
        assert!(l.find_by_shipped_sn("NOTEXIST").is_none());
        // shipped()：只含已出厂，按出厂时间升序（出货明细顺序）
        let shipped = l.shipped();
        assert_eq!(shipped.len(), 2);
        assert_eq!(shipped[0].shipped_sn, "6302W1000248");
        assert_eq!(shipped[1].ccid, "898608691025D0767784");
    }
}
