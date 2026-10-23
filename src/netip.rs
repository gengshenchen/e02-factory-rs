//! 本机网卡枚举（V3.12 WiFi 吞吐测试）：找与设备同网段的 PC 地址。
//!
//! 产线 PC 可能多网卡（有线 + 多网段），iperf3 服务必须绑在能与设备
//! 互通的那个地址上。环境变量 `E02_LOCAL_IP` 可强制指定（测试/特殊
//! 组网），与 `ADB`/`FFMPEG` 的覆盖惯例一致。

use std::net::Ipv4Addr;

/// 同网段判断（纯函数，可测）。
pub fn same_network(a: Ipv4Addr, b: Ipv4Addr, mask: Ipv4Addr) -> bool {
    (u32::from(a) & u32::from(mask)) == (u32::from(b) & u32::from(mask))
}

/// 找本机与 `peer` 同网段的 IPv4 地址。
///
/// if-addrs 在个别 Windows 版本取不到掩码时按 /24 兜底（产线局域网
/// 绝大多数是 /24；掩码更细的组网用 `E02_LOCAL_IP` 显式指定）。
pub fn local_ip_reaching(peer: Ipv4Addr) -> Option<Ipv4Addr> {
    if let Ok(forced) = std::env::var("E02_LOCAL_IP") {
        return forced.trim().parse().ok();
    }
    let ifs = if_addrs::get_if_addrs().ok()?;
    ifs.iter()
        .filter(|i| !i.is_loopback())
        .find_map(|i| match i.addr {
            if_addrs::IfAddr::V4(ref v4) => {
                let mask = if v4.netmask.is_unspecified() {
                    Ipv4Addr::new(255, 255, 255, 0)
                } else {
                    v4.netmask
                };
                same_network(v4.ip, peer, mask).then_some(v4.ip)
            }
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_network_basics() {
        let a = "192.168.3.10".parse().unwrap();
        let b = "192.168.3.42".parse().unwrap();
        let c = "192.168.4.42".parse().unwrap();
        let m24 = "255.255.255.0".parse().unwrap();
        let m16 = "255.255.0.0".parse().unwrap();
        assert!(same_network(a, b, m24));
        assert!(!same_network(a, c, m24));
        assert!(same_network(a, c, m16));
    }
}
