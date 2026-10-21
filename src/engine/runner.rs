//! 测试执行器：调用 adb 执行测试项并产出判定结果。
//!
//! M1 为同步阻塞实现（CLI 直接调用）；M2 由 GUI 的 worker 线程驱动。

use crate::adb;
use crate::engine::{TestItem, TestResult, Verdict};
use crate::protocol::{self, CameraFactory, DeviceInfo};

/// 单项执行产出：结果 + 可能携带的设备信息（info/btmac 项）。
pub struct RunOutcome {
    pub result: TestResult,
    pub device: Option<DeviceInfo>,
}

/// 批量执行产出：合并后的设备信息 + 各项结果。
pub struct BatchOutcome {
    pub device: DeviceInfo,
    pub results: Vec<TestResult>,
}

/// 执行一个测试项。人工判定项（指示灯/MIC·喇叭）与媒体项（录像/拍照）
/// 在 headless 下自动跳过，留给 GUI 阶段处理。
pub fn run_auto(item: TestItem) -> RunOutcome {
    let mut r = TestResult::new(item);
    let device = match item {
        TestItem::DeviceInfo => match exec(&CameraFactory::DeviceInfo.device_cmd())
            .and_then(|out| protocol::parse_device_info(&out).map_err(|e| format!("{e:#}")))
        {
            Ok(info) => {
                r.verdict = Verdict::Pass;
                r.detail = format!("sn={} wifi={} sw={}", info.sn, info.wifi_mac, info.t32_ver);
                Some(info)
            }
            Err(e) => {
                r.verdict = Verdict::Fail;
                r.detail = e;
                None
            }
        },
        TestItem::BtMac => match exec(&CameraFactory::BtMac.device_cmd())
            .and_then(|out| protocol::parse_device_info(&out).map_err(|e| format!("{e:#}")))
        {
            Ok(info) if !info.bt_mac.is_empty() => {
                r.verdict = Verdict::Pass;
                r.detail = info.bt_mac.clone();
                Some(info)
            }
            Ok(_) => {
                r.verdict = Verdict::Fail;
                r.detail = "输出中未解析到蓝牙 MAC".into();
                None
            }
            Err(e) => {
                r.verdict = Verdict::Fail;
                r.detail = e;
                None
            }
        },
        TestItem::Wifi => match exec(&CameraFactory::Wifi.device_cmd()) {
            Ok(out) => {
                r.verdict = bool_verdict(protocol::wifi_ok(&out));
                r.detail = one_line(&out);
                None
            }
            Err(e) => fail(&mut r, e),
        },
        TestItem::SdCard => match exec(&CameraFactory::SdCard.device_cmd()) {
            Ok(out) => {
                r.verdict = bool_verdict(protocol::sdcard_ok(&out));
                r.detail = one_line(&out);
                None
            }
            Err(e) => fail(&mut r, e),
        },
        TestItem::Charger => match exec(&CameraFactory::Charger.device_cmd()) {
            Ok(out) => {
                match protocol::parse_charger(&out) {
                    Ok(line) => {
                        r.verdict = Verdict::Pass;
                        r.detail = one_line(&line);
                    }
                    Err(_) => {
                        r.verdict = Verdict::Fail;
                        r.detail = one_line(&out);
                    }
                }
                None
            }
            Err(e) => fail(&mut r, e),
        },
        TestItem::Battery => match exec(&CameraFactory::Battery.device_cmd()) {
            Ok(out) => {
                let pct = protocol::parse_battery(&out);
                if pct.is_empty() {
                    r.verdict = Verdict::Fail;
                    r.detail = one_line(&out);
                } else {
                    r.verdict = Verdict::Pass;
                    r.detail = format!("电池电压 {pct}");
                }
                None
            }
            Err(e) => fail(&mut r, e),
        },
        TestItem::Keys => match exec(&CameraFactory::Keys.device_cmd()) {
            Ok(out) => {
                let keys = protocol::parse_keys(&out);
                if keys.is_empty() {
                    r.verdict = Verdict::Fail;
                    r.detail = format!("未检测到按键，输出: {}", one_line(&out));
                } else {
                    r.verdict = Verdict::Pass;
                    r.detail = keys.join("、");
                }
                None
            }
            Err(e) => fail(&mut r, e),
        },
        TestItem::Led | TestItem::MicSpeaker | TestItem::Video | TestItem::Photo => {
            r.verdict = Verdict::Skipped;
            r.detail = match item {
                TestItem::Led | TestItem::MicSpeaker => "需人工判定（GUI M5 接入）".into(),
                _ => "媒体展示项（M3/M4 接入）".into(),
            };
            None
        }
    };
    RunOutcome { result: r, device }
}

/// 批量执行并合并设备信息。
pub fn run_batch(items: &[TestItem]) -> BatchOutcome {
    let mut device = DeviceInfo::default();
    let mut results = Vec::with_capacity(items.len());
    for &item in items {
        let out = run_auto(item);
        if let Some(d) = out.device {
            device.merge(d);
        }
        results.push(out.result);
    }
    BatchOutcome { device, results }
}

/// 执行 shell 命令，错误折叠为单行字符串。
fn exec(cmd: &str) -> Result<String, String> {
    adb::shell(cmd).map_err(|e| format!("{e:#}"))
}

fn fail(r: &mut TestResult, e: String) -> Option<DeviceInfo> {
    r.verdict = Verdict::Fail;
    r.detail = e;
    None
}

fn bool_verdict(ok: bool) -> Verdict {
    if ok {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// 多行输出压成单行，便于日志/界面展示。
fn one_line(s: &str) -> String {
    s.lines().map(str::trim).collect::<Vec<_>>().join(" | ")
}

#[cfg(all(unix, test))] // 假 adb 桩仅在 unix 测试构建下存在
mod tests {
    use super::*;

    /// 用假 adb 脚本桩做端到端验证（Windows CI 跳过，真机行为由产线回归覆盖）。
    #[test]
    fn run_auto_info_with_fake_adb() {
        let dir = std::env::temp_dir().join(format!("e02-fake-adb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("adb");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'sn-address:002\\nwifi-mac:00:60:a7:0b:9c:f2\\nsoft-ware:E02-1.0.1\\n'\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", &script);

        let out = run_auto(TestItem::DeviceInfo);
        assert_eq!(out.result.verdict, Verdict::Pass);
        let info = out.device.expect("info 测试应带回设备信息");
        assert_eq!(info.sn, "002");
        assert_eq!(info.wifi_mac, "00:60:A7:0B:9C:F2");

        let skipped = run_auto(TestItem::Led);
        assert_eq!(skipped.result.verdict, Verdict::Skipped);

        let batch = run_batch(&TestItem::ALL);
        assert_eq!(batch.results.len(), TestItem::ALL.len());
        assert_eq!(batch.device.sn, "002");

        std::env::remove_var("ADB");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
