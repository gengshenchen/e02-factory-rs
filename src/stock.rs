//! 出厂写号库存（FQC §4）：`provision_stock.json` 单文件双池。
//!
//! - SN 池（发号系统 txt 导入，可选——未导入时流程降级，仅格式+台账校验）；
//! - 凭据池（客户 uuid/auth_key 对子 txt 导入，凭据档案强制——缺池拒写）。
//!
//! 取用制：客户凭据与 SN 无预先对应关系，写号时领"下一个未用凭据"绑给
//! 当前 SN。**领用即预落盘**（available → allocated，记录 bound_sn 与
//! bound_bt_mac）：首条 cf30 写入前先原子落盘，中途断电/崩溃不会把同一
//! 凭据分给两台（FQC §3 ⑦）；PASS 后推进 committed，返修重跑按
//! bound_sn 复原（幂等），报废机在库存页手动 voided。
//!
//! 状态用稳定英文枚举落盘（UI 层展示中文：未用/已分配/已写入/已作废）。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// SN/凭据条目状态机：available → allocated（预落盘）→ committed（PASS）；
/// 任一态可手动 voided（报废），voided 终态不复活。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    Available,
    Allocated,
    Committed,
    Voided,
}

impl EntryStatus {
    /// UI 中文标签。
    pub fn label(self) -> &'static str {
        match self {
            EntryStatus::Available => "未用",
            EntryStatus::Allocated => "占用中",
            EntryStatus::Committed => "已写入",
            EntryStatus::Voided => "已作废",
        }
    }
}

/// SN 池条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnEntry {
    pub sn: String,
    pub status: EntryStatus,
    #[serde(default)]
    pub bound_bt_mac: String,
    #[serde(default)]
    pub updated_at: String,
}

/// 凭据池条目（uuid/auth_key 对子，不带 SN——写号时领用绑定）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CredEntry {
    pub uuid: String,
    pub auth_key: String,
    pub status: EntryStatus,
    #[serde(default)]
    pub bound_sn: String,
    #[serde(default)]
    pub bound_bt_mac: String,
    #[serde(default)]
    pub updated_at: String,
}

/// 双池库存。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProvisionStock {
    #[serde(default)]
    pub sns: Vec<SnEntry>,
    #[serde(default)]
    pub credentials: Vec<CredEntry>,
}

/// 库存文件路径：数据目录 provision_stock.json（config.toml 同目录）。
pub fn stock_path() -> std::path::PathBuf {
    crate::paths::data_dir().join("provision_stock.json")
}

/// 导入结果（整批校验过才落库；重复条目静默跳过并计数）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    /// 实际新增条目数
    pub added: usize,
    /// 与现有库存完全一致而跳过的重复条目数
    pub skipped_dup: usize,
    /// 校验错误（非空 = 整批拒入，库存未变）；字符串含行号
    pub errors: Vec<String>,
}

impl ImportOutcome {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// 文本行清洗：剥 UTF-8 BOM 与 `\r`，去两端空白；纯空行与 `#` 注释行
/// 返回 None（FQC §4 导入规则）。
fn clean_line(line: &str) -> Option<String> {
    let line = line.trim_start_matches('\u{FEFF}').trim_end_matches('\r');
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        None
    } else {
        Some(line.to_string())
    }
}

impl ProvisionStock {
    /// 从磁盘加载；文件缺失/损坏返回空库存（首启不阻断）。
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// 原子落盘（tmp + rename，同 ledger 风格）。
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn available_sns(&self) -> usize {
        self.count_sn_status(EntryStatus::Available)
    }

    pub fn available_credentials(&self) -> usize {
        self.count_cred_status(EntryStatus::Available)
    }

    pub fn count_sn_status(&self, status: EntryStatus) -> usize {
        self.sns.iter().filter(|e| e.status == status).count()
    }

    pub fn count_cred_status(&self, status: EntryStatus) -> usize {
        self.credentials
            .iter()
            .filter(|e| e.status == status)
            .count()
    }

    /// 数量提醒：启用凭据写入时，未用 SN 多于未用凭据则提醒及时补充。
    pub fn mismatch_warning(&self, needs_cred: bool) -> Option<String> {
        if !needs_cred || !self.has_sn_pool() {
            return None;
        }
        let sns = self.available_sns();
        let creds = self.available_credentials();
        if sns > creds {
            Some(format!(
                "凭据余量不足：未用 SN（{sns}）多于未用凭据（{creds}），请及时补充凭据"
            ))
        } else {
            None
        }
    }

    /// SN 池是否已导入（池非空即视为已导入——空批次导入无意义）。
    pub fn has_sn_pool(&self) -> bool {
        !self.sns.is_empty()
    }

    /// SN 状态查询（写号流程 ③ 在池校验 + ⑥ 展示用）。
    pub fn sn_entry(&self, sn: &str) -> Option<&SnEntry> {
        self.sns.iter().find(|e| e.sn == sn)
    }

    /// 导入 SN txt：一行一个 SN。
    /// 整批校验（格式不符档案正则、批内重复 → 拒入列行号）；与库存完全
    /// 一致的重复（含已写状态）静默跳过不重置。校验失败库存不变。
    pub fn import_sns(&mut self, text: &str, sn_pattern: &str, now: &str) -> ImportOutcome {
        let mut out = ImportOutcome::default();
        let re = match regex_lite::Regex::new(sn_pattern) {
            Ok(re) => re,
            Err(e) => {
                out.errors.push(format!("档案 SN 正则无效: {e}"));
                return out;
            }
        };
        let mut batch: Vec<String> = Vec::new();
        for (idx, line) in text.lines().enumerate() {
            let Some(sn) = clean_line(line) else { continue };
            let lineno = idx + 1;
            if !re.is_match(&sn) {
                out.errors.push(format!("第 {lineno} 行 SN 格式不符: {sn}"));
                continue;
            }
            if batch.contains(&sn) {
                out.errors.push(format!("第 {lineno} 行 SN 批内重复: {sn}"));
                continue;
            }
            batch.push(sn);
        }
        if !out.ok() {
            return out;
        }
        for sn in batch {
            if self.sns.iter().any(|e| e.sn == sn) {
                out.skipped_dup += 1; // 已写/已分配状态永不重置
                continue;
            }
            self.sns.push(SnEntry {
                sn,
                status: EntryStatus::Available,
                bound_bt_mac: String::new(),
                updated_at: now.into(),
            });
            out.added += 1;
        }
        out
    }

    /// 导入凭据 txt：`uuid<分隔>auth_key` 对子（逗号/Tab/空白，恰好 2 列）。
    /// 整批校验（列数、格式、批内 uuid 重复、与库存同 uuid 不同 auth_key
    /// 冲突 → 拒入列行号）；与库存完全一致的条目静默跳过。
    pub fn import_credentials(
        &mut self,
        text: &str,
        uuid_pattern: &str,
        key_pattern: &str,
        now: &str,
    ) -> ImportOutcome {
        let mut out = ImportOutcome::default();
        let (re_uuid, re_key) = match (
            regex_lite::Regex::new(uuid_pattern),
            regex_lite::Regex::new(key_pattern),
        ) {
            (Ok(u), Ok(k)) => (u, k),
            (Err(e), _) | (_, Err(e)) => {
                out.errors.push(format!("档案凭据正则无效: {e}"));
                return out;
            }
        };
        let mut batch: Vec<(usize, String, String)> = Vec::new();
        for (idx, line) in text.lines().enumerate() {
            let Some(line) = clean_line(line) else {
                continue;
            };
            let lineno = idx + 1;
            // 分隔符：逗号 / Tab / 连续空白，取分割后恰好 2 列
            let cols: Vec<&str> = if line.contains(',') {
                line.split(',').map(str::trim).collect()
            } else {
                line.split_whitespace().collect()
            };
            if cols.len() != 2 {
                out.errors.push(format!(
                    "第 {lineno} 行应为 uuid/auth_key 两列（逗号/Tab/空格分隔）: {line}"
                ));
                continue;
            }
            // 常见 CSV 首行表头（uuid,auth_key）自动跳过，避免客户文件带表头被误拒
            if batch.is_empty()
                && out.errors.is_empty()
                && cols[0].eq_ignore_ascii_case("uuid")
                && cols[1].eq_ignore_ascii_case("auth_key")
            {
                continue;
            }
            let (uuid, key) = (cols[0].to_string(), cols[1].to_string());
            if !re_uuid.is_match(&uuid) {
                out.errors
                    .push(format!("第 {lineno} 行 uuid 格式不符: {uuid}"));
                continue;
            }
            if !re_key.is_match(&key) {
                out.errors
                    .push(format!("第 {lineno} 行 auth_key 格式不符: {key}"));
                continue;
            }
            if let Some((_, _, prev)) = batch.iter().find(|(_, u, _)| *u == uuid) {
                out.errors.push(format!(
                    "第 {lineno} 行 uuid 批内重复（首见 auth_key {}）: {uuid}",
                    clip_secret(prev)
                ));
                continue;
            }
            batch.push((lineno, uuid, key));
        }
        // 只读阶段二：检查与现有库存的 auth_key 冲突，确保任一错整批拒入且库存零变更
        for (lineno, uuid, key) in &batch {
            if let Some(prev) = self.credentials.iter().find(|e| e.uuid == *uuid) {
                if prev.auth_key != *key {
                    out.errors.push(format!(
                        "第 {lineno} 行 uuid {uuid} 与库存现有条目 auth_key 冲突（库存 {}）",
                        clip_secret(&prev.auth_key)
                    ));
                }
            }
        }
        if !out.ok() {
            return out;
        }
        for (_, uuid, key) in batch {
            if self.credentials.iter().any(|e| e.uuid == uuid) {
                out.skipped_dup += 1; // 完全一致 → 跳过，已写状态不重置
                continue;
            }
            self.credentials.push(CredEntry {
                uuid,
                auth_key: key,
                status: EntryStatus::Available,
                bound_sn: String::new(),
                bound_bt_mac: String::new(),
                updated_at: now.into(),
            });
            out.added += 1;
        }
        out
    }

    /// 领用前置校验通过后调用（FQC §3 ⑦）：按 SN 复原既有分配
    /// （allocated/committed 且 bound_sn 相同 → 幂等复用，不改库存）。
    pub fn credential_bound_to(&self, sn: &str) -> Option<&CredEntry> {
        self.credentials.iter().find(|c| {
            c.bound_sn == sn && matches!(c.status, EntryStatus::Allocated | EntryStatus::Committed)
        })
    }

    /// 领取下一个未用凭据并预落盘为 allocated（绑定 SN + 蓝牙 MAC）。
    /// 返回 None = 池空（凭据档案此时拒写）。调用方负责 save。
    pub fn claim_next_credential(
        &mut self,
        sn: &str,
        bt_mac: &str,
        now: &str,
    ) -> Option<CredEntry> {
        let idx = self
            .credentials
            .iter()
            .position(|c| c.status == EntryStatus::Available)?;
        let c = &mut self.credentials[idx];
        c.status = EntryStatus::Allocated;
        c.bound_sn = sn.into();
        c.bound_bt_mac = bt_mac.to_uppercase();
        c.updated_at = now.into();
        Some(c.clone())
    }

    /// SN 预落盘（同号重写保持 allocated，新机首次进入写号流程时标记）。
    pub fn mark_sn_allocated(&mut self, sn: &str, bt_mac: &str, now: &str) {
        if let Some(e) = self.sns.iter_mut().find(|e| e.sn == sn) {
            // 已 committed 的 SN 同号重写不回退状态
            if e.status == EntryStatus::Available {
                e.status = EntryStatus::Allocated;
                e.bound_bt_mac = bt_mac.to_uppercase();
                e.updated_at = now.into();
            }
        }
    }

    /// PASS 后推进：SN → committed。
    pub fn commit_sn(&mut self, sn: &str, now: &str) -> bool {
        if let Some(e) = self.sns.iter_mut().find(|e| e.sn == sn) {
            if e.status != EntryStatus::Voided {
                e.status = EntryStatus::Committed;
                e.updated_at = now.into();
                return true;
            }
        }
        false
    }

    /// PASS 后推进：凭据 → committed。
    pub fn commit_credential(&mut self, uuid: &str, now: &str) -> bool {
        if let Some(c) = self.credentials.iter_mut().find(|c| c.uuid == uuid) {
            if c.status != EntryStatus::Voided {
                c.status = EntryStatus::Committed;
                c.updated_at = now.into();
                return true;
            }
        }
        false
    }

    /// 手动作废（库存页维护：报废机等）。
    pub fn void_sn(&mut self, sn: &str, now: &str) -> bool {
        if let Some(e) = self.sns.iter_mut().find(|e| e.sn == sn) {
            e.status = EntryStatus::Voided;
            e.updated_at = now.into();
            true
        } else {
            false
        }
    }

    pub fn void_credential(&mut self, uuid: &str, now: &str) -> bool {
        if let Some(c) = self.credentials.iter_mut().find(|c| c.uuid == uuid) {
            c.status = EntryStatus::Voided;
            c.updated_at = now.into();
            true
        } else {
            false
        }
    }

    /// 清空未用批次（换批场景）：只删 available，绝不碰已分配/已写入/已作废。
    pub fn clear_available_sns(&mut self) -> usize {
        let before = self.sns.len();
        self.sns.retain(|e| e.status != EntryStatus::Available);
        before - self.sns.len()
    }

    pub fn clear_available_credentials(&mut self) -> usize {
        let before = self.credentials.len();
        self.credentials
            .retain(|e| e.status != EntryStatus::Available);
        before - self.credentials.len()
    }

    /// 挂起（已预分配但未完成出厂 PASS）的 SN 列表，供库存维护页释放或作废。
    pub fn allocated_sns(&self) -> Vec<&SnEntry> {
        self.sns
            .iter()
            .filter(|e| e.status == EntryStatus::Allocated)
            .collect()
    }

    /// 挂起（已预分配但未完成出厂 PASS）的凭据列表。
    pub fn allocated_credentials(&self) -> Vec<&CredEntry> {
        self.credentials
            .iter()
            .filter(|c| c.status == EntryStatus::Allocated)
            .collect()
    }

    /// 将挂起（Allocated）的 SN 释放回未用（误扫/未写入即中断换码场景）；
    /// 已出厂（Committed）或已作废（Voided）严禁释放。
    pub fn release_allocated_sn(&mut self, sn: &str, now: &str) -> bool {
        if let Some(e) = self.sns.iter_mut().find(|e| e.sn == sn) {
            if e.status == EntryStatus::Allocated {
                e.status = EntryStatus::Available;
                e.bound_bt_mac.clear();
                e.updated_at = now.into();
                return true;
            }
        }
        false
    }

    /// 将挂起（Allocated）的凭据释放回未用（确认设备未烧入该凭据时人工释放）；
    /// 已出厂（Committed）或已作废（Voided）严禁释放。
    pub fn release_allocated_credential(&mut self, uuid: &str, now: &str) -> bool {
        if let Some(c) = self.credentials.iter_mut().find(|c| c.uuid == uuid) {
            if c.status == EntryStatus::Allocated {
                c.status = EntryStatus::Available;
                c.bound_sn.clear();
                c.bound_bt_mac.clear();
                c.updated_at = now.into();
                return true;
            }
        }
        false
    }

    /// 凭据按绑定 SN 查找（出货 CSV / 展示用）。
    pub fn credential_by_sn(&self, sn: &str) -> Option<&CredEntry> {
        self.credentials
            .iter()
            .find(|c| c.bound_sn == sn && c.status != EntryStatus::Voided)
    }
}

/// 凭据脱敏截断（日志/错误信息不落全值；按字符边界切，避免非 ASCII panic）。
pub(crate) fn clip_secret(s: &str) -> String {
    let n = s.chars().count();
    if n <= 6 {
        "****".into()
    } else {
        let head: String = s.chars().take(4).collect();
        format!("{head}…（{n} 字符）")
    }
}

/// 出货明细行（FQC §7：ledger + stock 关联后的导出行）。
#[derive(Debug, Clone, Default)]
pub struct ShipCsvRow {
    pub sn: String,
    pub bt_mac: String,
    pub model: String,
    pub shipped_at: String,
    pub ship_verdict: String,
    /// E02 型落值，E02E 留空（按产品有无列）
    pub wifi_mac: String,
    pub imei: String,
    pub ccid: String,
    pub uuid: String,
    pub auth_key: String,
}

/// 出货明细 CSV（UTF-8 BOM，Excel 直开）。列按行数据自适应：
/// 任一行有值才出现该列；凭据默认只导 uuid，`include_auth_key` 勾选才带
/// auth_key（FQC §7：缩小凭据暴露面）。
pub fn ship_csv(rows: &[ShipCsvRow], include_auth_key: bool) -> String {
    let has = |f: fn(&ShipCsvRow) -> &String| rows.iter().any(|r| !f(r).is_empty());
    let mut cols: Vec<&str> = vec!["SN", "蓝牙MAC", "型号", "写号时间", "判定"];
    if has(|r| &r.wifi_mac) {
        cols.push("WiFi MAC");
    }
    if has(|r| &r.imei) {
        cols.push("IMEI");
    }
    if has(|r| &r.ccid) {
        cols.push("CCID");
    }
    if has(|r| &r.uuid) {
        cols.push("uuid");
    }
    if include_auth_key && has(|r| &r.auth_key) {
        cols.push("auth_key");
    }
    let mut s = String::from("\u{FEFF}");
    s.push_str(&cols.join(","));
    s.push('\n');
    for r in rows {
        let mut fields: Vec<String> = vec![
            r.sn.clone(),
            r.bt_mac.clone(),
            r.model.clone(),
            r.shipped_at.clone(),
            r.ship_verdict.clone(),
        ];
        if cols.contains(&"WiFi MAC") {
            fields.push(r.wifi_mac.clone());
        }
        if cols.contains(&"IMEI") {
            fields.push(r.imei.clone());
        }
        if cols.contains(&"CCID") {
            fields.push(r.ccid.clone());
        }
        if cols.contains(&"uuid") {
            fields.push(r.uuid.clone());
        }
        if cols.contains(&"auth_key") {
            fields.push(r.auth_key.clone());
        }
        s.push_str(&fields.join(","));
        s.push('\n');
    }
    s
}

/// SN txt 预检（导入预览：不落库，返回将新增/跳过/错误）。
pub fn preview_sn_import(stock: &ProvisionStock, text: &str, sn_pattern: &str) -> ImportOutcome {
    let mut clone = stock.clone();
    clone.import_sns(text, sn_pattern, "")
}

/// 凭据 txt 预检（同上，不落库）。
pub fn preview_cred_import(
    stock: &ProvisionStock,
    text: &str,
    uuid_pattern: &str,
    key_pattern: &str,
) -> ImportOutcome {
    let mut clone = stock.clone();
    clone.import_credentials(text, uuid_pattern, key_pattern, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SN_PAT: &str = r"^\d{4}[WE]\d{7}$";
    const UUID_PAT: &str = r"^uuid[0-9a-f]{16}$";
    const KEY_PAT: &str = r"^[A-Za-z0-9]{32}$";

    fn sn(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn sn_import_validates_and_merges() {
        let mut st = ProvisionStock::default();
        // BOM + CRLF + 空行 + 注释行
        let text = "\u{FEFF}6302W1000248\r\n\r\n# 批次注释\n6302W1000249\n6302W1000248\n";
        let out = st.import_sns(text, SN_PAT, "t1");
        assert!(!out.ok(), "批内重复应整批拒入: {out:?}");
        assert_eq!(out.errors.len(), 1);
        assert_eq!(st.sns.len(), 0, "拒入时库存不变");

        let out = st.import_sns("6302W1000248\n6302W1000249\nbad-sn\n", SN_PAT, "t1");
        assert!(!out.ok());
        assert!(out.errors[0].contains("第 3 行"));

        let out = st.import_sns("6302W1000248\n6302W1000249\n", SN_PAT, "t1");
        assert!(out.ok());
        assert_eq!(out.added, 2);
        assert_eq!(st.available_sns(), 2);

        // 重复导入：完全一致 → 跳过；已写状态不重置
        st.sns[0].status = EntryStatus::Committed;
        let out = st.import_sns("6302W1000248\n6302W1000250\n", SN_PAT, "t2");
        assert!(out.ok());
        assert_eq!(out.added, 1);
        assert_eq!(out.skipped_dup, 1);
        assert_eq!(st.sns[0].status, EntryStatus::Committed);
    }

    #[test]
    fn cred_import_column_separators_and_conflicts() {
        let mut st = ProvisionStock::default();
        let uuid1 = "uuid95fe79695a6df72f";
        let key1 = "B7uTB4QjoSLw55oHLk2wPSokAAwWXENH";
        // 首行 CSV 表头自动跳过 + 三种分隔符等价
        let text = format!("uuid,auth_key\n{uuid1},{key1}\nuuid95fe79695a6df730\t{key1}\n");
        let out = st.import_credentials(&text, UUID_PAT, KEY_PAT, "t1");
        assert!(out.ok(), "{:?}", out.errors);
        assert_eq!(out.added, 2);

        // 第 1 行新凭据 + 第 2 行与库存同 uuid 异 auth_key 冲突 → 整批拒入且库存零变更
        let before = st.credentials.clone();
        let bad =
            format!("uuid95fe79695a6df731,{key1}\n{uuid1},AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n");
        let out = st.import_credentials(&bad, UUID_PAT, KEY_PAT, "t2");
        assert!(!out.ok());
        assert!(out.errors[0].contains("第 2 行"));
        assert!(out.errors[0].contains("冲突"));
        assert_eq!(st.credentials, before, "冲突拒入不得残留前序新增条目");

        // clip_secret 非 ASCII 安全不 panic
        assert!(clip_secret("中文密钥一二三四五").starts_with("中文密钥…"));

        // 完全一致 → 跳过
        let same = format!("{uuid1},{key1}\n");
        let out = st.import_credentials(&same, UUID_PAT, KEY_PAT, "t3");
        assert!(out.ok());
        assert_eq!(out.skipped_dup, 1);
        assert_eq!(out.added, 0);

        // 列数错误
        let out = st.import_credentials("only-one-column\n", UUID_PAT, KEY_PAT, "t4");
        assert!(!out.ok());
        // 格式错误带行号
        let out = st.import_credentials("not-uuid,ABCDEFG\n", UUID_PAT, KEY_PAT, "t5");
        assert!(!out.ok());
        assert!(out.errors[0].contains("第 1 行 uuid"));
    }

    #[test]
    fn claim_is_idempotent_and_crash_safe() {
        let mut st = ProvisionStock::default();
        st.import_sns("6302W1000248\n", SN_PAT, "t1").ok();
        st.import_credentials(
            "uuid95fe79695a6df72f,B7uTB4QjoSLw55oHLk2wPSokAAwWXENH\n\
             uuid95fe79695a6df730,C7uTB4QjoSLw55oHLk2wPSokAAwWXENH\n",
            UUID_PAT,
            KEY_PAT,
            "t2",
        )
        .ok();
        // 首次领用 → allocated + 绑定；模拟崩溃后重跑 → 按 bound_sn 复原
        let c1 = st
            .claim_next_credential(&sn("6302W1000248"), "0012A3B4C5D6", "t3")
            .unwrap();
        assert_eq!(c1.status, EntryStatus::Allocated);
        assert_eq!(c1.bound_sn, "6302W1000248");
        let reuse = st.credential_bound_to("6302W1000248").unwrap();
        assert_eq!(reuse.uuid, c1.uuid, "重跑复原同一条，不领新的");
        // 断电后新开进程：available 只剩 1，allocated 那条不会再发给别机
        assert_eq!(st.available_credentials(), 1);
        let c2 = st
            .claim_next_credential(&sn("6302W1000249"), "9999A3B4C5D6", "t4")
            .unwrap();
        assert_ne!(c1.uuid, c2.uuid);
        assert!(
            st.claim_next_credential(&sn("6302W1000250"), "AA", "t5")
                .is_none(),
            "池空"
        );
    }

    #[test]
    fn commit_and_void_and_clear_available() {
        let mut st = ProvisionStock::default();
        st.import_sns("6302W1000248\n6302W1000249\n", SN_PAT, "t1");
        st.import_credentials(
            "uuid95fe79695a6df72f,B7uTB4QjoSLw55oHLk2wPSokAAwWXENH\n",
            UUID_PAT,
            KEY_PAT,
            "t2",
        );
        // 数量对账预警：SN 2 > 凭据 1
        assert!(st.mismatch_warning(true).is_some());
        assert!(st.mismatch_warning(false).is_none());
        st.mark_sn_allocated("6302W1000248", "0012A3B4C5D6", "t3");
        let c = st
            .claim_next_credential("6302W1000248", "0012A3B4C5D6", "t3")
            .unwrap();
        assert_eq!(st.allocated_sns().len(), 1);
        assert_eq!(st.allocated_credentials().len(), 1);
        // 挂起态允许释放回未用，重领后继续推进
        assert!(st.release_allocated_sn("6302W1000248", "t3r"));
        assert!(st.release_allocated_credential(&c.uuid, "t3r"));
        assert_eq!(st.available_credentials(), 1);
        st.mark_sn_allocated("6302W1000248", "0012A3B4C5D6", "t3b");
        let c = st
            .claim_next_credential("6302W1000248", "0012A3B4C5D6", "t3b")
            .unwrap();
        // PASS 推进
        assert!(st.commit_sn("6302W1000248", "t4"));
        assert!(st.commit_credential(&c.uuid, "t4"));
        // 已 committed 严禁释放回未用
        assert!(!st.release_allocated_sn("6302W1000248", "t4x"));
        assert!(!st.release_allocated_credential(&c.uuid, "t4x"));
        assert_eq!(
            st.sn_entry("6302W1000248").unwrap().status,
            EntryStatus::Committed
        );
        // committed 的 SN 同号重写 mark 不回退
        st.mark_sn_allocated("6302W1000248", "X", "t5");
        assert_eq!(
            st.sn_entry("6302W1000248").unwrap().status,
            EntryStatus::Committed
        );
        // 作废终态
        assert!(st.void_sn("6302W1000249", "t6"));
        assert_eq!(
            st.sn_entry("6302W1000249").unwrap().status,
            EntryStatus::Voided
        );
        assert!(!st.commit_sn("6302W1000249", "t7"), "voided 不再推进");
        // 清未用：只删 available
        st.import_sns("6302W1000250\n", SN_PAT, "t8");
        assert_eq!(st.clear_available_sns(), 1); // 6302W1000250
        assert_eq!(st.sns.len(), 2, "committed+voided 保留");
        assert_eq!(st.clear_available_credentials(), 0, "凭据已 committed 不清");
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = std::env::temp_dir().join(format!("e02-stock-{}", std::process::id()));
        let path = dir.join("sub").join("stock.json");
        let mut st = ProvisionStock::default();
        st.import_sns("6302W1000248\n", SN_PAT, "t1");
        st.import_credentials(
            "uuid95fe79695a6df72f,B7uTB4QjoSLw55oHLk2wPSokAAwWXENH\n",
            UUID_PAT,
            KEY_PAT,
            "t2",
        );
        st.claim_next_credential("6302W1000248", "0012A3B4C5D6", "t3")
            .unwrap();
        st.save(&path).unwrap();
        assert_eq!(ProvisionStock::load(&path), st);
        // 损坏文件 → 空库存
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "{broken").unwrap();
        let broken = ProvisionStock::load(&path);
        assert!(broken.sns.is_empty() && broken.credentials.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ship_csv_adaptive_columns() {
        let mut rows = vec![ShipCsvRow {
            sn: "6302W1000248".into(),
            bt_mac: "0012A3B4C5D6".into(),
            model: "E02".into(),
            shipped_at: "20261124-100000".into(),
            ship_verdict: "PASS".into(),
            wifi_mac: "0060A70B9CF2".into(),
            ..Default::default()
        }];
        let csv = ship_csv(&rows, false);
        assert!(csv.starts_with('\u{FEFF}'));
        assert!(csv.contains("SN,蓝牙MAC,型号,写号时间,判定,WiFi MAC"));
        assert!(!csv.contains("uuid"), "无凭据行不出现 uuid 列");
        // eSIM 行 → IMEI/CCID 列；凭据行 + 勾选 → auth_key
        rows.push(ShipCsvRow {
            sn: "6322E1001999".into(),
            bt_mac: "9999A3B4C5D6".into(),
            model: "E02E".into(),
            shipped_at: "20261124-100001".into(),
            ship_verdict: "PASS".into(),
            imei: "868781080025402".into(),
            ccid: "898608691025D0767784".into(),
            uuid: "uuid95fe79695a6df72f".into(),
            auth_key: "B7uTB4QjoSLw55oHLk2wPSokAAwWXENH".into(),
            ..Default::default()
        });
        let csv = ship_csv(&rows, true);
        assert!(csv.contains("WiFi MAC,IMEI,CCID,uuid,auth_key"));
        assert!(
            csv.contains("6322E1001999,9999A3B4C5D6,E02E,20261124-100001,PASS,,868781080025402")
        );
        let csv_no_auth = ship_csv(&rows, false);
        assert!(csv_no_auth.contains("uuid"));
        assert!(!csv_no_auth.contains("auth_key"));
    }

    #[test]
    fn preview_does_not_mutate() {
        let mut st = ProvisionStock::default();
        st.import_sns("6302W1000248\n", SN_PAT, "t1");
        let out = preview_sn_import(&st, "6302W1000248\n6302W1000250\n", SN_PAT);
        assert_eq!(out.added, 1);
        assert_eq!(out.skipped_dup, 1);
        assert_eq!(st.sns.len(), 1, "预检不落库");
    }
}
