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
    /// 设备侧命令（不含 `adb shell` 前缀，由 adb 层负责拼接）。
    pub fn device_cmd(self) -> String {
        format!("camera-factory {}", self as u8)
    }
}

/// 设备侧媒体文件远端路径（原程序 adb pull 的参数）。
pub const REMOTE_PHOTO: &str = "/tmp/mnt/sdcard/F1/Videos/testjpeg.jpg";
pub const REMOTE_VIDEO: &str = "/tmp/mnt/sdcard/F1/Videos/testvideo.mp4";

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

impl DeviceInfo {
    /// 用非空字段覆盖合并（camera-factory 3 与 10 返回的字段子集不同）。
    pub fn merge(&mut self, other: DeviceInfo) {
        let DeviceInfo {
            sn,
            wifi_mac,
            bt_mac,
            t32_ver,
            hardware_ver,
            battery,
        } = other;
        for (dst, src) in [
            (&mut self.sn, sn),
            (&mut self.wifi_mac, wifi_mac),
            (&mut self.bt_mac, bt_mac),
            (&mut self.t32_ver, t32_ver),
            (&mut self.hardware_ver, hardware_ver),
            (&mut self.battery, battery),
        ] {
            if !src.is_empty() {
                *dst = src;
            }
        }
    }
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

/// 按键码 → 名称（沿用原程序判定：5=开机键，6=音量键，4=多功能键）。
pub fn key_name(code: u8) -> &'static str {
    match code {
        5 => "开机键",
        6 => "音量键",
        _ => "多功能键",
    }
}

/// 键码 → 对应数字字符（码 4 → '4'；u8 直转 char 会得到控制字符）。
fn digit_of(code: u8) -> char {
    (b'0' + code) as char
}

/// 按键测试（兼容模式）：输出含字符 4/5/6 即视为检测到，
/// 与原程序 `Find("5")` 行为一致（可能被版本号/电压数字误判）。
pub fn parse_keys_codes(output: &str) -> Vec<u8> {
    [4u8, 5, 6]
        .into_iter()
        .filter(|&c| output.contains(digit_of(c)))
        .collect()
}

/// 按键测试（严格模式）：只认独立数字行（`^\s*[456]\s*$`），
/// 避免 "E02-1.0.5"、"Voltage:4.2V" 这类文本假阳性。
pub fn parse_keys_strict(output: &str) -> Vec<u8> {
    [4u8, 5, 6]
        .into_iter()
        .filter(|&c| {
            let digit = digit_of(c).to_string();
            output.lines().any(|l| l.trim() == digit)
        })
        .collect()
}

use anyhow::Context as _;

/// 解析 `ip route` 的默认网关（WiFi 速率测试用）。
/// 样例：`default via 192.168.1.1 dev wlan0  proto static`
pub fn parse_default_gateway(output: &str) -> Option<String> {
    let line = output
        .lines()
        .find(|l| l.trim_start().starts_with("default"))?;
    let mut it = line.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == "via" {
            return it.next().map(str::to_string);
        }
    }
    None
}

/// 解析 ping 统计：`(丢包率 %, avg RTT ms)`。
///
/// 兼容两种格式（阈值判定见 config `[item.wifi.throughput]`）：
/// - toybox: `... 0% packet loss` + `rtt min/avg/max/mdev = 1.2/23.4/99.0/5.0 ms`
/// - busybox: `... 0% packet loss` + `round-trip min/avg/max = 1.2/23.4/99.0 ms`
pub fn parse_ping_stats(output: &str) -> Option<(f32, f32)> {
    let loss = output.lines().find_map(|l| {
        let idx = l.find("% packet loss").or_else(|| l.find("% loss"))?;
        // 取百分号前紧邻的数字（如 "..., 0% packet loss" → 0）
        let head: String = l[..idx]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let pct: String = head.chars().rev().collect();
        pct.parse().ok()
    });
    let avg = output
        .lines()
        .filter(|l| l.contains("rtt") || l.contains("round-trip"))
        .find_map(|l| {
            let eq = l.find('=')?;
            let nums = l[eq + 1..].trim().trim_end_matches("ms").trim();
            let parts: Vec<&str> = nums.split('/').collect();
            // min/avg/max(/mdev)：avg 恒为第 2 段
            if parts.len() >= 2 {
                parts[1].trim().parse().ok()
            } else {
                None
            }
        });
    match (loss, avg) {
        (Some(l), Some(a)) => Some((l, a)),
        _ => None,
    }
}

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
    fn device_cmd_format() {
        assert_eq!(CameraFactory::Video.device_cmd(), "camera-factory 1");
        assert_eq!(CameraFactory::Keys.device_cmd(), "camera-factory 15");
    }

    #[test]
    fn wifi_throughput_parsers() {
        let route = "default via 192.168.1.1 dev wlan0  proto static\n\
                     192.168.1.0/24 dev wlan0  proto kernel";
        assert_eq!(
            parse_default_gateway(route),
            Some("192.168.1.1".to_string())
        );
        assert_eq!(parse_default_gateway("192.168.1.0/24 dev wlan0"), None);

        // toybox 格式
        let ping = "PING 192.168.1.1 (192.168.1.1) 56(84) bytes of data.\n\
                    20 packets transmitted, 19 received, 5% packet loss, time 32ms\n\
                    rtt min/avg/max/mdev = 1.2/23.4/99.0/5.0 ms";
        assert_eq!(parse_ping_stats(ping), Some((5.0, 23.4)));
        // busybox 格式（round-trip 三段）
        let busy = "20 packets transmitted, 20 received, 0% packet loss\n\
                    round-trip min/avg/max = 1.0/8.5/40.0 ms";
        assert_eq!(parse_ping_stats(busy), Some((0.0, 8.5)));
        // 乱码/空输出
        assert_eq!(parse_ping_stats("nothing here"), None);
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
        assert_eq!(parse_keys_codes("456"), vec![4, 5, 6]);
        assert!(parse_keys_codes("9").is_empty());
        // 兼容模式与原程序一致：任意位置含数字即命中（含假阳性场景）
        assert_eq!(parse_keys_codes("soft-ware:E02-1.0.5"), vec![5]);
        // 严格模式只认独立数字行，排除版本号/电压数字
        assert_eq!(parse_keys_strict("5\n6\n4\n"), vec![4, 5, 6]);
        assert!(parse_keys_strict("soft-ware:E02-1.0.5").is_empty());
        assert!(parse_keys_strict("Voltage:4.2V").is_empty());
        assert_eq!(parse_keys_strict(" 6 \r\n"), vec![6]);
        assert_eq!(key_name(5), "开机键");
    }
}
