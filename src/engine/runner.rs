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
        TestItem::DeviceInfo => {
            match exec(&CameraFactory::DeviceInfo.device_cmd()).and_then(|out| {
                protocol::parse_device_info(&out)
                    .map_err(|e| format!("{e:#}（原始输出: {}）", one_line(&out)))
            }) {
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
            }
        }
        TestItem::BtMac => match exec(&CameraFactory::BtMac.device_cmd()).and_then(|out| {
            protocol::parse_device_info(&out)
                .map_err(|e| format!("{e:#}（原始输出: {}）", one_line(&out)))
        }) {
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
                r.verdict = bool_verdict(protocol::output_ok(&out));
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
        TestItem::Battery => match exec(&CameraFactory::Battery.device_cmd()) {
            Ok(out) => {
                // V3.5：收集电压/电流行展示（充电项已并入）；无相关字段判失败
                let detail = protocol::parse_battery(&out);
                if detail.is_empty() {
                    r.verdict = Verdict::Fail;
                    r.detail = format!("未解析到电压/电流数据，输出: {}", one_line(&out));
                    None
                } else {
                    r.verdict = Verdict::Pass;
                    r.detail = detail.clone();
                    Some(DeviceInfo {
                        battery: detail,
                        ..DeviceInfo::default()
                    })
                }
            }
            Err(e) => fail(&mut r, e),
        },
        TestItem::Keys => match exec(&CameraFactory::Keys.device_cmd()) {
            Ok(out) => {
                let keys = protocol::parse_keys_codes(&out);
                if keys.is_empty() {
                    r.verdict = Verdict::Fail;
                    r.detail = format!("未检测到按键，输出: {}", one_line(&out));
                } else {
                    r.verdict = Verdict::Pass;
                    r.detail = keys
                        .iter()
                        .map(|&c| protocol::key_name(c))
                        .collect::<Vec<_>>()
                        .join("、");
                }
                None
            }
            Err(e) => fail(&mut r, e),
        },
        TestItem::Led | TestItem::MicSpeaker | TestItem::Video | TestItem::Photo => {
            r.verdict = Verdict::Skipped;
            r.detail = "需人工判定（GUI 接入）".into();
            None
        }
        TestItem::Gyro => match exec(&CameraFactory::Gyro.device_cmd()) {
            Ok(out) => {
                // camera-factory 17（V3.5）：固件回读 gyroscope-test:true/false，
                // 严格等 true——false 输出不含 FAIL 字样，通用判定会误过
                r.verdict = bool_verdict(protocol::gyro_ok(&out));
                r.detail = one_line(&out);
                None
            }
            Err(e) => fail(&mut r, e),
        },
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
/// 输出压成单行（日志/detail 展示用）。
pub fn one_line(s: &str) -> String {
    s.lines().map(str::trim).collect::<Vec<_>>().join(" | ")
}

#[cfg(all(unix, test))] // 假 adb 桩仅在 unix 测试构建下存在
mod tests {
    use super::*;

    /// 用假 adb 脚本桩做端到端验证（Windows CI 跳过，真机行为由产线回归覆盖）。
    /// ADB 是进程级环境变量、exec 停止位是全局静态：与其他用到假 adb 的
    /// 测试（worker 序列）互斥，否则并行时互相覆盖桩/误置停止位。
    #[test]
    fn run_auto_info_with_fake_adb() {
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::exec::clear_abort();
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
