//! 被测设备侧 `camera-factory` 命令协议与输出解析。
//!
//! 原程序（VLCplayerDlg.cpp）用 `CString::Find` + 固定长度 `Mid` 截取；
//! 这里改为按行前缀解析，找不到字段返回 Err，由测试引擎判 FAIL，
//! 顺带修掉原程序"解析失败只提示不阻断"的问题。

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// `adb shell camera-factory N` 的子命令号。
/// 11 充电已下线（V3.5 产线指令：充电并入电池项）；9 指示灯改纯人工判定
/// 不再发指令；14 副麦、17 陀螺仪、19 时间校准为固件新指令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CameraFactory {
    Video = 1,
    Photo = 2,
    DeviceInfo = 3,
    Wifi = 5,
    SdCard = 8,
    BtMac = 10,
    Battery = 12,
    MicSpeaker = 13,
    /// 副麦
    MicSecondary = 14,
    Keys = 15,
    Gyro = 17,
    /// 时间校准（配合宿主机 date -s 下发时间后执行）
    TimeSync = 19,
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

/// 电池测试（camera-factory 12，V3.5）：收集输出中含 电压/电流 关键词的行，
/// 供界面展示与判定（行存在即通过）。覆盖真机已见格式：`Bat vol:4.390`、
/// `Vol:4.326,cur:0.235`、`voltage:`/`current:`（V3.6 真机日志补 vol/cur 短写）。
pub fn parse_battery(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .filter(|l| {
            let low = l.to_lowercase();
            low.contains("vol") || low.contains("cur") || l.contains('压') || l.contains('流')
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 设备命令返回值判定约定（V3.5）：输出含 `FAIL` 即失败，否则通过。
/// WiFi/录像/拍照共用。
pub fn output_ok(output: &str) -> bool {
    // FAIL-BUSY 是 wpa_ctrl 的瞬态应答（SCAN 忙），固件无视它继续走完
    // 关联 + DHCP（2026-10-28 真机：输出带 FAIL-BUSY 但 wlan0-ip 正常
    // 拿到租约），判定前剔除
    !output.replace("FAIL-BUSY", "").contains("FAIL")
}

/// 陀螺仪自检（camera-factory 17）：固件明确回读 `gyroscope-test:true/false`
/// （V3.6 真机日志：失败时无 FAIL 字样、只有 `gyroscope-test:false`，
/// 按正结果严格判定，拿不到 true 即失败）。
pub fn gyro_ok(output: &str) -> bool {
    output.contains("gyroscope-test:true")
}

/// SD 卡测试：输出含 `true` 即成功。
pub fn sdcard_ok(output: &str) -> bool {
    output.contains("true")
}

/// 键码 → 名称（V3.5 固件回读格式 `key N`：4=多功能键，5=开机键/音量-
/// （复用按键），6=音量+）。
pub fn key_name(code: u8) -> &'static str {
    match code {
        5 => "开机键/音量-",
        6 => "音量+",
        _ => "多功能键",
    }
}

/// 按键测试：解析设备输出中的 `key N` 标记（V3.5 固件回读格式）。
///
/// V3.6.2 模糊匹配：真机日志显示固件串口存在粘包，垃圾串 HC0 会糊在
/// 报文前后——`Key 4HC0`、`HC0...Key test enable`。词级匹配（key 与数字
/// 须为独立分词）在粘包下永远失配。现改为：`key`（大小写不敏感）后跳过
/// 空白、紧跟目标数字即命中，数字后跟任意字符（含粘连垃圾）都算。
/// `keyboard 4` 仍不误判（key 后是字母非数字）；`key test enable/disable`
/// 也不误判（key 后是空格+字母）。
pub fn parse_keys_codes(output: &str) -> Vec<u8> {
    let low = output.to_lowercase();
    [4u8, 5, 6]
        .into_iter()
        .filter(|&c| {
            let digit = (b'0' + c) as char;
            let mut from = 0usize;
            while let Some(pos) = low[from..].find("key") {
                if low[from + pos + 3..].trim_start().starts_with(digit) {
                    return true;
                }
                from += pos + 3;
            }
            false
        })
        .collect()
}

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

/// 解析 `ip -4 addr show wlan0` 的首个 IPv4（join + DHCP 的连通性证明）。
/// 样例行：`    inet 192.168.3.42/24 brd 192.168.3.255 scope global wlan0`
pub fn parse_wlan_ip(output: &str) -> Option<String> {
    output.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("inet ")?;
        let tok = rest.split_whitespace().next()?;
        // busybox ifconfig 的 `inet addr:x.x.x.x` 行让给 parse_wlan_ifconfig，
        // 此处会截出残值 addr:x.x.x.x
        if tok.starts_with("addr:") {
            return None;
        }
        let ip = tok.split('/').next()?;
        (!ip.starts_with("127.")).then(|| ip.to_string())
    })
}

/// 解析 `ifconfig wlan0` 的 IPv4（busybox 格式；行样例：
/// `          inet addr:10.82.62.192  Bcast:10.82.62.255  Mask:...`）。
pub fn parse_wlan_ifconfig(output: &str) -> Option<String> {
    output.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("inet addr:")?;
        let ip = rest.split_whitespace().next()?;
        (!ip.starts_with("127.")).then(|| ip.to_string())
    })
}

/// ip / ifconfig 双格式合并解析（探测命令 `ip ...; ifconfig ...` 拼接执行，
/// 真机 busybox 未必编入 ip applet，2026-10-28 轮询全空的教训）。
pub fn parse_wlan_ip_any(output: &str) -> Option<String> {
    parse_wlan_ip(output).or_else(|| parse_wlan_ifconfig(output))
}

/// 解析固件 camera-factory 5 输出里的 `wifi-rssi:<dBm>`（真机样例：
/// `wifi-rssi:-30`；探测链（cmd wifi→/proc/net/wireless）全空时的退路）。
pub fn parse_fw_wifi_rssi(output: &str) -> Option<i32> {
    let v = output
        .lines()
        .find_map(|l| l.trim().strip_prefix("wifi-rssi:"))?
        .trim();
    v.split_whitespace().next()?.parse().ok()
}

/// 解析固件 camera-factory 5 输出里的 `wlan0-ip: <IPv4>`（关联 + DHCP
/// 成功时固件自己回读打印，可省去轮询 ip 命令；
/// 真机样例：`wlan0-ip: 10.82.62.192`）。
pub fn parse_fw_wlan_ip(output: &str) -> Option<String> {
    let v = output
        .lines()
        .find_map(|l| l.trim().strip_prefix("wlan0-ip:"))?
        .trim();
    let ip = v.split_whitespace().next()?;
    ip.parse::<std::net::Ipv4Addr>()
        .is_ok()
        .then(|| ip.to_string())
}

/// 解析 `cmd wifi status` 的 `(RSSI dBm, 链路速率 Mbps)`（探测链首选）。
/// 样例：`... Supplicant state: COMPLETED, RSSI: -52, Link speed: 72Mbps, ...`
pub fn parse_cmd_wifi_status(output: &str) -> Option<(i32, u32)> {
    let rssi = output.split("RSSI").nth(1).and_then(|rest| {
        rest.trim_start_matches([':', ' ', '='])
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect::<String>()
            .parse()
            .ok()
    });
    let speed = output.split("Link speed").nth(1).and_then(|rest| {
        rest.trim_start_matches([':', ' ', '='])
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()
    });
    rssi.zip(speed)
}

/// 解析 `/proc/net/wireless` 的 wlan0 电平 dBm（探测链兜底）。
/// 样例：`wlan0: 0000  70.0.  -52.  -108.  0  0  ...`（第 4 列 level）
pub fn parse_wireless_level(output: &str) -> Option<i32> {
    let line = output.lines().find(|l| l.starts_with("wlan0"))?;
    let mut cols = line.split_whitespace();
    let _face = cols.next()?; // wlan0:
    let _status = cols.next()?;
    let _link = cols.next()?;
    let level: f32 = cols.next()?.trim_end_matches('.').parse().ok()?;
    Some(level.round() as i32)
}

/// iperf3 `-J` 客户端结果摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct IperfSummary {
    /// 每秒窗口吞吐中位数（Mbps；无窗口数据时回退 end 汇总）
    pub mbps_median: f32,
    /// end 汇总吞吐（Mbps）
    pub mbps_end: f32,
    /// TCP 重传数（客户端为发送端时才有；反向时由服务端结果补）
    pub retransmits: Option<u32>,
}

/// 解析 iperf3 客户端 `-J` JSON。
///
/// 兼容新旧格式：老版本无 `end.sum_sent` 只有 `end.sum`；
/// 输出含 `"error"` 字段（连接失败等）返回 `None`，消息取不到了就
/// 让调用方拿原始输出片段。
pub fn parse_iperf3_json(output: &str) -> Option<IperfSummary> {
    // adb 透传时 stdout 前可能混有固件杂散行，取首个 '{' 起的 JSON
    let start = output.find('{')?;
    let v: serde_json::Value = serde_json::from_str(&output[start..]).ok()?;
    if v.get("error").is_some() {
        return None;
    }
    let end = v.get("end")?;
    let pick = |key: &str| -> Option<f64> {
        end.get(key)
            .and_then(|s| s.get("bits_per_second"))
            .and_then(|b| b.as_f64())
    };
    // 吞吐：发送/接收两端取小（同一 TCP 连接，两端差即途中损失）
    let mbps_end = {
        let sent = pick("sum_sent");
        let recv = pick("sum_received");
        let one = sent.or(recv).or_else(|| {
            end.get("sum")
                .and_then(|s| s.get("bits_per_second"))
                .and_then(|b| b.as_f64())
        })?;
        let both = match (sent, recv) {
            (Some(a), Some(b)) => Some(a.min(b)),
            _ => None,
        };
        (both.unwrap_or(one) / 1e6) as f32
    };
    // 每秒窗口中位数（iperf3 固定 1s 窗口；-R 时窗口在 streams/sum 结构同）
    let mut wins: Vec<f32> = v
        .get("intervals")
        .and_then(|i| i.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|w| {
                    w.get("sum")
                        .or_else(|| w.get("sums").and_then(|s| s.as_array()?.first()))
                        .and_then(|s| s.get("bits_per_second"))
                        .and_then(|b| b.as_f64())
                        .map(|b| (b / 1e6) as f32)
                })
                .collect()
        })
        .unwrap_or_default();
    wins.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mbps_median = match wins.len() {
        0 => mbps_end,
        n if n % 2 == 1 => wins[n / 2],
        n => (wins[n / 2 - 1] + wins[n / 2]) / 2.0,
    };
    let retransmits = end
        .get("sum_sent")
        .and_then(|s| s.get("retransmits"))
        .and_then(|x| x.as_u64())
        .map(|x| x as u32);
    Some(IperfSummary {
        mbps_median,
        mbps_end,
        retransmits,
    })
}

/// 从 iperf3 **服务端** `-J` JSON 取 TCP 重传数（反向测试时发送端在 PC）。
pub fn parse_iperf3_server_retr(output: &str) -> Option<u32> {
    let start = output.find('{')?;
    let v: serde_json::Value = serde_json::from_str(&output[start..]).ok()?;
    v.get("end")?
        .get("sum_sent")?
        .get("retransmits")?
        .as_u64()
        .map(|x| x as u32)
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
        assert_eq!(
            CameraFactory::MicSecondary.device_cmd(),
            "camera-factory 14"
        );
        assert_eq!(CameraFactory::Gyro.device_cmd(), "camera-factory 17");
        assert_eq!(CameraFactory::TimeSync.device_cmd(), "camera-factory 19");
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
    fn parse_wlan_ipv4() {
        let out = "10: wlan0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500\n    inet 192.168.3.42/24 brd 192.168.3.255 scope global wlan0\n    inet6 fe80::1/64 scope link";
        assert_eq!(parse_wlan_ip(out), Some("192.168.3.42".into()));
        assert_eq!(parse_wlan_ip("no inet here"), None);
        // busybox ifconfig 格式 + 双命令拼接输出
        let bb = "wlan0     Link encap:Ethernet  HWaddr AA:BB:CC:DD:EE:FF\n          inet addr:10.82.62.192  Bcast:10.82.62.255  Mask:255.255.255.0";
        assert_eq!(parse_wlan_ifconfig(bb), Some("10.82.62.192".into()));
        assert_eq!(parse_wlan_ifconfig("lo ... inet addr:127.0.0.1"), None);
        assert_eq!(parse_wlan_ip_any(bb), Some("10.82.62.192".into()));
        // 固件 camera-factory 5 回读（真机 2026-10-28 实样）
        let fw = "Successfully initialized wpa_supplicant\nFAIL-BUSY\nudhcpc (v1.22.1) started\nLease of 10.82.62.192 obtained, lease time 3599\nwlan0-ip: 10.82.62.192\nwifi-rssi:-39";
        assert_eq!(parse_fw_wlan_ip(fw), Some("10.82.62.192".into()));
        assert_eq!(parse_fw_wlan_ip("wlan0-ip: \n"), None);
        assert_eq!(parse_fw_wlan_ip("no tag"), None);
        // 固件 RSSI 回读
        assert_eq!(parse_fw_wifi_rssi(fw), Some(-39));
        assert_eq!(parse_fw_wifi_rssi("wifi-rssi:-30"), Some(-30));
        assert_eq!(parse_fw_wifi_rssi("no tag"), None);
    }

    #[test]
    fn parse_wifi_rssi_sources() {
        let status = "Wifi is enabled\nWi-Fi is connected to X\nmWifiInfo SSID: X, BSSID: aa:bb, MAC: cc, Supplicant state: COMPLETED, RSSI: -52, Link speed: 72Mbps, Frequency: 2437MHz";
        assert_eq!(parse_cmd_wifi_status(status), Some((-52, 72)));
        assert_eq!(parse_cmd_wifi_status("no rssi"), None);
        // /proc/net/wireless：level 列带尾点
        let proc = "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE\n face | tus | link level noise |  nwid  ...\nwlan0: 0000  70.0.  -52.  -108.  0  0  0  0  0  0  0  0  0  0  0  0  0";
        assert_eq!(parse_wireless_level(proc), Some(-52));
        assert_eq!(
            parse_wireless_level("wlan0: 0000  70.0.  -71.  -108."),
            Some(-71)
        );
    }

    #[test]
    fn parse_iperf3_client_forward() {
        // 正向（设备发送）：end.sum_sent 带 retransmits，3 窗口 → 中位
        let j = r#"{"start":{},"intervals":[
            {"sum":{"bits_per_second":20e6,"retransmits":2}},
            {"sum":{"bits_per_second":24e6,"retransmits":1}},
            {"sum":{"bits_per_second":22e6,"retransmits":0}}],
          "end":{"sum_sent":{"bits_per_second":22e6,"retransmits":36},
                 "sum_received":{"bits_per_second":21.9e6}}}"#;
        let r = parse_iperf3_json(j).unwrap();
        assert_eq!(r.mbps_median, 22.0);
        assert_eq!(r.mbps_end, 21.9); // 两端取小
        assert_eq!(r.retransmits, Some(36));
    }

    #[test]
    fn parse_iperf3_client_reverse_old_shape() {
        // 反向（设备接收）：客户端只有接收侧；老版本 end 只有 sum
        let j = r#"{"intervals":[{"sum":{"bits_per_second":30e6}}],
          "end":{"sum":{"bits_per_second":29e6}}}"#;
        let r = parse_iperf3_json(j).unwrap();
        assert_eq!(r.mbps_median, 30.0);
        assert_eq!(r.mbps_end, 29.0);
        assert_eq!(r.retransmits, None);
    }

    #[test]
    fn parse_iperf3_error_and_noise() {
        // 固件杂散行混在 JSON 前 + error 字段 → None
        let noisy = r#"HC0garbage {"error":"unable to connect to server"}"#;
        assert_eq!(parse_iperf3_json(noisy), None);
        assert_eq!(parse_iperf3_json("not json at all"), None);
    }

    #[test]
    fn parse_iperf3_server_retransmits() {
        let j = r#"{"start":{},"end":{"sum_sent":{"bits_per_second":28e6,"retransmits":87},"sum_received":{"bits_per_second":27.8e6}}}"#;
        assert_eq!(parse_iperf3_server_retr(j), Some(87));
        assert_eq!(parse_iperf3_server_retr("junk"), None);
    }

    #[test]
    fn battery_wifi_sdcard_keys() {
        // 电池：电压/电流行收集展示（真机 V3.6 实测格式 + 旧前缀写法）
        assert_eq!(parse_battery("Bat vol:4.390"), "Bat vol:4.390");
        assert_eq!(parse_battery("Vol:4.326,cur:0.235"), "Vol:4.326,cur:0.235");
        assert_eq!(
            parse_battery("Battery voltage: 88%"),
            "Battery voltage: 88%"
        );
        assert_eq!(
            parse_battery("voltage: 4.12V\ncurrent: 356mA"),
            "voltage: 4.12V | current: 356mA"
        );
        assert_eq!(parse_battery("nope"), "");
        assert!(output_ok("ok"));
        assert!(!output_ok("...FAIL..."));
        // FAIL-BUSY 是 wpa_ctrl 瞬态噪声（真机：带着它照样拿到 DHCP），放行
        assert!(output_ok(
            "Successfully initialized wpa_supplicant\nFAIL-BUSY\nudhcpc started"
        ));
        // 陀螺仪：严格等 true（false 不含 FAIL，通用判定会误过）
        assert!(gyro_ok("qmi8658 slave = 0x6a\ngyroscope-test:true"));
        assert!(!gyro_ok("qmi8658 slave = 0x6a\ngyroscope-test:false"));
        assert!(!gyro_ok("no result line"));
        assert!(sdcard_ok("mount true"));
        assert!(!sdcard_ok("false"));
        // 按键：固件回读 `key N`（V3.5），模糊匹配兼容粘包（V3.6.2）
        assert_eq!(parse_keys_codes("key 4\nkey 5\nkey 6"), vec![4, 5, 6]);
        assert_eq!(parse_keys_codes("Key 5"), vec![5]);
        assert_eq!(parse_keys_codes("key 5 key 6"), vec![5, 6]);
        // 真机粘包实测：HC0 垃圾糊在数字前后都命中
        assert_eq!(parse_keys_codes("Key 4HC0"), vec![4]);
        assert_eq!(parse_keys_codes("HC0\\Key test enable\nKey 4HC0"), vec![4]);
        assert_eq!(
            parse_keys_codes("d#\nKey 4HC0\nd\"\nKey 6\nKey 5"),
            vec![4, 5, 6]
        );
        // 模糊语义：数字后跟任意字符都算（key 45 → 按了 4）
        assert_eq!(parse_keys_codes("key 45"), vec![4]);
        // 仍不误判：key 后是字母（keyboard / key test enable）
        assert!(parse_keys_codes("keyboard 4").is_empty());
        assert!(parse_keys_codes("Key test enable").is_empty());
        assert!(parse_keys_codes("Key test disable").is_empty());
        assert!(parse_keys_codes("soft-ware:E02-1.0.5").is_empty());
        assert_eq!(key_name(5), "开机键/音量-");
        assert_eq!(key_name(6), "音量+");
        assert_eq!(key_name(4), "多功能键");
    }
}
