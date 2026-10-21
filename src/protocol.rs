//! 被测设备侧 `camera-factory` 命令协议与输出解析。
//!
//! 原程序（VLCplayerDlg.cpp）用 `CString::Find` + 固定长度 `Mid` 截取；
//! 这里改为按行前缀解析，找不到字段返回 Err，由测试引擎判 FAIL，
//! 顺带修掉原程序"解析失败只提示不阻断"的问题。

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// `adb shell camera-factory N` 的子命令号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CameraFactory {
    Video = 1,
    Photo = 2,
    DeviceInfo = 3,
    Wifi = 5,
    SdCard = 8,
    Led = 9,
    BtMac = 10,
    Charger = 11,
    Battery = 12,
    MicSpeaker = 13,
    Keys = 15,
}

impl CameraFactory {
    /// 完整 adb 命令行。
    pub fn adb_cmd(self) -> String {
        format!("adb shell camera-factory {}", self as u8)
    }
}

/// 设备信息（原 Deviceinfo 结构）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub sn: String,
    pub wifi_mac: String,
    pub bt_mac: String,
    pub t32_ver: String,
    pub hardware_ver: String,
    pub battery: String,
}

/// 解析 `camera-factory 3` / `10` 的输出（样例见仓库 T32.txt）。
pub fn parse_device_info(output: &str) -> Result<DeviceInfo> {
    let mut info = DeviceInfo::default();
    for line in output.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("sn-address:") {
            info.sn = v.trim().to_uppercase();
        } else if let Some(v) = line.strip_prefix("wifi-mac:") {
            info.wifi_mac = v.trim().to_uppercase();
        } else if let Some(v) = line.strip_prefix("soft-ware:") {
            info.t32_ver = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("hard-ware:") {
            info.hardware_ver = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("MAC:") {
            info.bt_mac = v.trim().to_uppercase();
        } else if let Some(v) = line.strip_prefix("Battery voltage:") {
            info.battery = v.trim().to_string();
        }
    }
    let has_any = !(info.sn.is_empty()
        && info.wifi_mac.is_empty()
        && info.bt_mac.is_empty()
        && info.t32_ver.is_empty());
    if !has_any {
        bail!("输出中未找到任何设备信息字段: {output:?}");
    }
    Ok(info)
}

/// 充电测试：找到含 `Voltage` 的行（原逻辑 `Find("Voltage")`）。
pub fn parse_charger(output: &str) -> Result<String> {
    output
        .lines()
        .map(str::trim)
        .find(|l| l.contains("Voltage"))
        .map(str::to_string)
        .with_context(|| format!("未找到 Voltage 字段: {output:?}"))
}

/// 电池测试：`Battery voltage:` 后的百分数。
pub fn parse_battery(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("Battery voltage:"))
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

/// WiFi 测试：原逻辑为"输出不含 FAIL 即通过"。
pub fn wifi_ok(output: &str) -> bool {
    !output.contains("FAIL")
}

/// SD 卡测试：输出含 `true` 即成功。
pub fn sdcard_ok(output: &str) -> bool {
    output.contains("true")
}

/// 按键测试：5=录像键，6=拍照键，4=多功能键（沿用原判定字符）。
pub fn parse_keys(output: &str) -> Vec<&'static str> {
    let mut keys = Vec::new();
    for ch in ['5', '6', '4'] {
        if output.contains(ch) {
            keys.push(match ch {
                '5' => "录像按键正常",
                '6' => "拍照按键正常",
                _ => "多功能按键正常",
            });
        }
    }
    keys
}

use anyhow::Context as _;

#[cfg(test)]
mod tests {
    use super::*;

    /// 原仓库 T32.txt 实际样例（CRLF 已按真机输出保留）。
    const T32_SAMPLE: &str =
        "sn-address:002\r\nwifi-mac:00:60:a7:0b:9c:f2\r\nsoft-ware:E02-1.0.1\r\nhard-ware:E02-1.0.1\r\n";

    #[test]
    fn parse_device_info_from_t32_sample() {
        let d = parse_device_info(T32_SAMPLE).unwrap();
        assert_eq!(d.sn, "002");
        assert_eq!(d.wifi_mac, "00:60:A7:0B:9C:F2");
        assert_eq!(d.t32_ver, "E02-1.0.1");
        assert_eq!(d.hardware_ver, "E02-1.0.1");
    }

    #[test]
    fn parse_bt_mac_line() {
        let d = parse_device_info("MAC:0012a3b4c5d6").unwrap();
        assert_eq!(d.bt_mac, "0012A3B4C5D6");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_device_info("unknown output").is_err());
    }

    #[test]
    fn adb_cmd_format() {
        assert_eq!(CameraFactory::Video.adb_cmd(), "adb shell camera-factory 1");
        assert_eq!(CameraFactory::Keys.adb_cmd(), "adb shell camera-factory 15");
    }

    #[test]
    fn charger_battery_wifi_sdcard_keys() {
        assert!(parse_charger("Voltage: 3.9V").is_ok());
        assert!(parse_charger("nope").is_err());
        assert_eq!(parse_battery("Battery voltage: 88%"), "88%");
        assert!(wifi_ok("ok"));
        assert!(!wifi_ok("...FAIL..."));
        assert!(sdcard_ok("mount true"));
        assert!(!sdcard_ok("false"));
        assert_eq!(
            parse_keys("456"),
            ["录像按键正常", "拍照按键正常", "多功能按键正常"]
        );
        assert!(parse_keys("9").is_empty());
    }
}
