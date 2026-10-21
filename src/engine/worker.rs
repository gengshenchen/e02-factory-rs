//! 测试序列 worker：在独立线程阻塞执行 adb/ffmpeg 流程，
//! 经 [`UiEvent`] 回传状态；人工判定阻塞等待 [`UiResponse`]。

use crate::adb;
use crate::config::{Config, GyroCfg, KeysCfg, WifiCfg};
use crate::engine::events::{UiEvent, UiResponse};
use crate::engine::runner::{self, RunOutcome};
use crate::engine::{TestItem, TestResult, Verdict};
use crate::exec;
use crate::player;
use crate::protocol::{self, CameraFactory, DeviceInfo};
use crate::report;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// 一键全测顺序，对应原程序 OnBnClickedButton14 的调用顺序（陀螺仪追加在末尾）。
pub const BATCH_ORDER: [TestItem; 12] = [
    TestItem::Photo,
    TestItem::DeviceInfo,
    TestItem::BtMac,
    TestItem::Wifi,
    TestItem::Charger,
    TestItem::Battery,
    TestItem::SdCard,
    TestItem::Video,
    TestItem::Keys,
    TestItem::Led,
    TestItem::MicSpeaker,
    TestItem::Gyro,
];

/// 同步执行测试序列（worker 线程与测试共用）。
/// 返回报告文件路径；写入失败为 None。
pub fn run_sequence(
    items: &[TestItem],
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    media_dir: &Path,
    reports_dir: &Path,
    cfg: &Config,
) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(media_dir);
    let _ = std::fs::create_dir_all(reports_dir);
    let _ = tx.send(UiEvent::Started);
    let (started_wall, started) = (SystemTime::now(), Instant::now());
    // 单项超时（V2.3）：序列期间子进程统一看门狗，防止固件命令阻塞卡死序列
    let prev_timeout = exec::default_timeout();
    exec::set_default_timeout(timeout_opt(cfg.timeouts.item_secs));

    let mut device = DeviceInfo::default();
    let mut results: Vec<TestResult> = Vec::new();
    let stem = report::time_str(started_wall);
    let _ = tx.send(UiEvent::Log(format!(
        "序列开始，工位 {}，共 {} 项，单项超时 {}",
        cfg.station,
        items.len(),
        timeout_desc(cfg.timeouts.item_secs)
    )));
    for &item in items {
        let _ = tx.send(UiEvent::Current(item));
        let _ = tx.send(UiEvent::Log(format!("【{}】开始", item.label())));
        let RunOutcome {
            result,
            device: dev,
        } = run_item(item, tx, manual, media_dir, cfg);
        let _ = tx.send(UiEvent::Log(format!(
            "【{}】{} {}",
            item.label(),
            result.verdict.label(),
            clip(&result.detail, 100)
        )));
        if let Some(d) = &dev {
            device.merge(d.clone());
        }
        let _ = tx.send(UiEvent::Result(result.clone(), dev));
        results.push(result);
        // 报告边测边写（V2.3）：文件名固定、逐项覆盖，中途卡死/退出也留有报告
        let _ = report::write_report_stem(
            reports_dir,
            sn_dir(cfg, &device).as_deref(),
            &meta(cfg, started_wall, started),
            &device,
            &results,
            &stem,
        );
    }
    exec::set_default_timeout(prev_timeout);
    report::write_report_stem(
        reports_dir,
        sn_dir(cfg, &device).as_deref(),
        &meta(cfg, started_wall, started),
        &device,
        &results,
        &stem,
    )
}

/// 秒数 → 超时选项（0 = 不限制）。
fn timeout_opt(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

fn timeout_desc(secs: u64) -> String {
    if secs == 0 {
        "关".into()
    } else {
        format!("{secs}s")
    }
}

fn meta(cfg: &Config, started_wall: SystemTime, started: Instant) -> report::ReportMeta {
    report::ReportMeta {
        station: cfg.station.clone(),
        started_utc: report::time_str(started_wall),
        elapsed_secs: started.elapsed().as_secs(),
    }
}

/// SN 归档子目录（V2.3）：开启后 reports/<SN>/，空 SN 归入 unknown/
fn sn_dir(cfg: &Config, device: &DeviceInfo) -> Option<String> {
    cfg.report_by_sn.then(|| {
        if device.sn.is_empty() {
            "unknown".to_string()
        } else {
            device.sn.clone()
        }
    })
}

/// 日志/detail 截断（尾部加省略号）。
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}...")
    }
}

fn run_item(
    item: TestItem,
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    media_dir: &Path,
    cfg: &Config,
) -> RunOutcome {
    match item {
        TestItem::Photo => photo_flow(tx, media_dir),
        TestItem::Video => video_flow(tx, media_dir),
        TestItem::Keys => keys_flow(tx, manual, &cfg.keys, cfg.timeouts.manual_secs),
        TestItem::Wifi => wifi_flow(tx, &cfg.wifi),
        TestItem::Gyro => gyro_flow(tx, &cfg.gyro),
        TestItem::Led => manual_flow(
            item,
            "指示灯闪烁是否正常？",
            tx,
            manual,
            Some(3),
            cfg.timeouts.manual_secs,
        ),
        TestItem::MicSpeaker => manual_flow(
            item,
            "MIC 与喇叭声音是否正常？",
            tx,
            manual,
            None,
            cfg.timeouts.manual_secs,
        ),
        _ => runner::run_auto(item),
    }
}

/// 拍照流程：拍摄 → 拉回 → UI 显示（对应原 OnBnClickedButtonCamera）。
fn photo_flow(tx: &Sender<UiEvent>, media_dir: &Path) -> RunOutcome {
    let mut r = TestResult::new(TestItem::Photo);
    let local = media_dir.join("testjpeg.jpg");
    // 先删旧图，防止上一台设备的图片冒充本轮结果（原程序同逻辑）
    let _ = std::fs::remove_file(&local);
    let cmd = CameraFactory::Photo.device_cmd();
    let _ = tx.send(UiEvent::Log(format!("执行 adb shell {cmd}")));
    let outcome = adb::shell(&cmd)
        .map_err(|e| format!("{e:#}"))
        .and_then(|_| {
            thread::sleep(Duration::from_millis(1000)); // 原程序 Sleep(1000)
            let _ = tx.send(UiEvent::Log(format!(
                "adb pull {} -> {}",
                protocol::REMOTE_PHOTO,
                local.display()
            )));
            adb::pull(protocol::REMOTE_PHOTO, &local).map_err(|e| format!("{e:#}"))
        })
        .and_then(|_| {
            if local.exists() {
                Ok(())
            } else {
                Err("adb pull 成功但本地文件不存在".to_string())
            }
        });
    match outcome {
        Ok(()) => {
            let _ = tx.send(UiEvent::PhotoReady(local));
            r.verdict = Verdict::Pass;
            r.detail = "图片已拉回".into();
        }
        Err(e) => {
            r.verdict = Verdict::Fail;
            r.detail = e;
        }
    }
    RunOutcome {
        result: r,
        device: None,
    }
}

/// 录像流程：录制 → 拉回 → ffmpeg 解码验证 → 抽帧回放（对应原 OnBnClickedButtonVedio）。
fn video_flow(tx: &Sender<UiEvent>, media_dir: &Path) -> RunOutcome {
    let mut r = TestResult::new(TestItem::Video);
    let local = media_dir.join("testvideo.mp4");
    let _ = std::fs::remove_file(&local);
    let cmd = CameraFactory::Video.device_cmd();
    let _ = tx.send(UiEvent::Log(format!("执行 adb shell {cmd}")));
    let outcome = adb::shell(&cmd)
        .map_err(|e| format!("{e:#}"))
        .and_then(|_| {
            thread::sleep(Duration::from_millis(900)); // 原程序 Sleep(900)
            let _ = tx.send(UiEvent::Log(format!(
                "adb pull {} -> {}",
                protocol::REMOTE_VIDEO,
                local.display()
            )));
            adb::pull(protocol::REMOTE_VIDEO, &local).map_err(|e| format!("{e:#}"))
        });
    if let Err(e) = outcome {
        r.verdict = Verdict::Fail;
        r.detail = e;
        return RunOutcome {
            result: r,
            device: None,
        };
    }
    // 解码验证 + 抽帧（原程序仅"能播即过"，这里加严为全程解码成功）
    let _ = tx.send(UiEvent::Log(format!("ffmpeg 解码 {}", local.display())));
    let frames_dir = media_dir.join("frames");
    match player::extract_frames(&local, &frames_dir, 2, 640) {
        Ok(count) => {
            let has_audio = player::probe_has_audio(&local).unwrap_or(false);
            let _ = tx.send(UiEvent::VideoReady {
                frames_dir,
                count,
                has_audio,
            });
            // 回放观察窗口（对应原程序 Sleep(2500)）
            thread::sleep(Duration::from_millis(2500));
            r.verdict = Verdict::Pass;
            r.detail = format!(
                "解码 {count} 帧，{}",
                if has_audio {
                    "含音频流"
                } else {
                    "无音频流"
                }
            );
        }
        Err(e) => {
            r.verdict = Verdict::Fail;
            r.detail = format!("视频解码失败: {e:#}");
        }
    }
    RunOutcome {
        result: r,
        device: None,
    }
}

/// 人工判定流程：发命令 → 弹窗询问 → 阻塞等待应答（原 MessageBox 语义）。
fn manual_flow(
    item: TestItem,
    question: &str,
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    repeat: Option<usize>,
    manual_secs: u64,
) -> RunOutcome {
    let mut r = TestResult::new(item);
    // 人工判定项（指示灯/MIC）必有 camera-factory 命令
    let cmd = item
        .factory_cmd()
        .expect("人工判定项必有设备命令")
        .device_cmd();
    let n = repeat.unwrap_or(1);
    let _ = tx.send(UiEvent::Log(format!("执行 adb shell {cmd} 共 {n} 次")));
    for _ in 0..n {
        let _ = adb::shell(&cmd);
    }
    let _ = tx.send(UiEvent::Manual {
        item,
        question: question.to_string(),
    });
    match recv_manual(manual, manual_secs) {
        Ok(UiResponse::Manual(true)) => r.verdict = Verdict::ManualYes,
        Ok(UiResponse::Manual(false)) => r.verdict = Verdict::ManualNo,
        Ok(_) => {
            // Keys* 应答不会出现在此弹窗，视为应答异常
            r.verdict = Verdict::Skipped;
            r.detail = "应答类型不匹配".into();
        }
        Err(RecvTimeoutError::Timeout) => {
            // 保守判失败：宁可复测，不静默放过
            r.verdict = Verdict::ManualNo;
            r.detail = format!("超时未人工判定（{manual_secs}s），自动判失败");
        }
        Err(RecvTimeoutError::Disconnected) => {
            r.verdict = Verdict::Skipped;
            r.detail = "界面已退出，判定中断".into();
        }
    }
    RunOutcome {
        result: r,
        device: None,
    }
}

/// 等待 UI 应答；`secs` 0 = 无限等。超时返回 Err(Timeout)。
fn recv_manual(manual: &Receiver<UiResponse>, secs: u64) -> Result<UiResponse, RecvTimeoutError> {
    match timeout_opt(secs) {
        Some(d) => manual.recv_timeout(d),
        None => manual.recv().map_err(|e| e.into()),
    }
}

/// 按键引导流程（V2.1）：弹窗提示按键 → 固件监听窗口内操作员按键 →
/// 回显已检测到的键，收齐自动 PASS；未收齐由操作员重测/改判。
/// 必测键集合与解析模式由工位配置裁剪（V2.2）。
///
/// 原程序为无提示静默解析（没按到键时什么都不打印、也不判失败），
/// 一键全测下无人按键必失败，故改为引导式（V2_PLAN.md §1）。
fn keys_flow(
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    keys: &KeysCfg,
    manual_secs: u64,
) -> RunOutcome {
    let mut r = TestResult::new(TestItem::Keys);
    let cmd = CameraFactory::Keys.device_cmd();
    loop {
        let _ = tx.send(UiEvent::Log(format!("执行 adb shell {cmd}，请依次按键...")));
        match adb::shell(&cmd) {
            Ok(out) => {
                let got = if keys.strict {
                    protocol::parse_keys_strict(&out)
                } else {
                    protocol::parse_keys_codes(&out)
                };
                let missing: Vec<&'static str> = keys
                    .required
                    .iter()
                    .filter(|c| !got.contains(c))
                    .map(|&c| protocol::key_name(c))
                    .collect();
                let _ = tx.send(UiEvent::Log(format!(
                    "按键输出: {}",
                    clip(&runner::one_line(&out), 80)
                )));
                if missing.is_empty() {
                    let _ = tx.send(UiEvent::KeysUpdate {
                        detected: got,
                        awaiting: false,
                    });
                    r.verdict = Verdict::Pass;
                    r.detail = "收齐全部按键".into();
                    break;
                }
                let _ = tx.send(UiEvent::KeysUpdate {
                    detected: got.clone(),
                    awaiting: true,
                });
                let _ = tx.send(UiEvent::Log(format!("未收齐，缺: {}", missing.join("、"))));
                match recv_manual(manual, manual_secs) {
                    Ok(UiResponse::KeysRetry) => {
                        // 立即回发"检测中"，弹窗不闪断；再进入下一轮命令
                        let _ = tx.send(UiEvent::KeysUpdate {
                            detected: got.clone(),
                            awaiting: false,
                        });
                        continue;
                    }
                    Ok(UiResponse::KeysForce(true)) => {
                        r.verdict = Verdict::ManualYes;
                        r.detail = format!("人工强制通过（缺 {}）", missing.join("、"));
                        break;
                    }
                    Ok(UiResponse::KeysForce(false)) | Ok(UiResponse::Manual(false)) => {
                        r.verdict = Verdict::ManualNo;
                        r.detail = format!("缺 {}", missing.join("、"));
                        break;
                    }
                    Ok(UiResponse::Manual(true)) => {
                        // 视作重测（同 KeysRetry）
                        let _ = tx.send(UiEvent::KeysUpdate {
                            detected: got.clone(),
                            awaiting: false,
                        });
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        r.verdict = Verdict::ManualNo;
                        r.detail = format!("超时未应答（缺 {}）", missing.join("、"));
                        break;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        r.verdict = Verdict::Skipped;
                        r.detail = "界面已退出，按键判定中断".into();
                        break;
                    }
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("超时") {
                    // 固件监听窗口一直阻塞到超时：按"未收齐"处理，交给操作员
                    let _ = tx.send(UiEvent::KeysUpdate {
                        detected: Vec::new(),
                        awaiting: true,
                    });
                    let _ = tx.send(UiEvent::Log("按键监听超时，无输出".to_string()));
                    match recv_manual(manual, manual_secs) {
                        Ok(UiResponse::KeysRetry) | Ok(UiResponse::Manual(true)) => {
                            let _ = tx.send(UiEvent::KeysUpdate {
                                detected: Vec::new(),
                                awaiting: false,
                            });
                            continue;
                        }
                        Ok(UiResponse::KeysForce(true)) => {
                            r.verdict = Verdict::ManualYes;
                            r.detail = "人工强制通过（监听超时无输出）".into();
                            break;
                        }
                        Ok(_) => {
                            r.verdict = Verdict::ManualNo;
                            r.detail = "监听超时，未检测到按键".into();
                            break;
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            r.verdict = Verdict::ManualNo;
                            r.detail = format!("监听超时且无人应答（{manual_secs}s）");
                            break;
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            r.verdict = Verdict::Skipped;
                            r.detail = "界面已退出，按键判定中断".into();
                            break;
                        }
                    }
                } else {
                    r.verdict = Verdict::Fail;
                    r.detail = msg;
                    break;
                }
            }
        }
    }
    RunOutcome {
        result: r,
        device: None,
    }
}

/// WiFi 流程（V2.2 深度可配）：
/// - driver：`camera-factory 5` 驱动通即过（原程序行为）
/// - throughput：驱动过后追加 ping 网关统计（丢包率/avg RTT 阈值判定）
fn wifi_flow(tx: &Sender<UiEvent>, wifi: &WifiCfg) -> RunOutcome {
    // 先跑驱动检查（camera-factory 5），失败直接返回
    let base = runner::run_auto(TestItem::Wifi);
    if base.result.verdict != Verdict::Pass || wifi.depth == crate::config::WifiDepth::Driver {
        return base;
    }

    let mut r = base.result;
    let t = &wifi.throughput;
    let _ = tx.send(UiEvent::Log("WiFi 速率：探测默认网关...".to_string()));
    let gw = adb::shell("ip route")
        .ok()
        .and_then(|out| protocol::parse_default_gateway(&out));
    let Some(gw) = gw else {
        r.verdict = Verdict::Fail;
        r.detail = "驱动通过，但未解析到默认网关（WiFi 未连路由器？）".into();
        return RunOutcome {
            result: r,
            device: base.device,
        };
    };

    let _ = tx.send(UiEvent::Log(format!(
        "ping 网关 {gw} 共 {} 包",
        t.ping_count
    )));
    match adb::shell(&format!("ping -c {} -W 2 {gw}", t.ping_count)) {
        Ok(out) => match protocol::parse_ping_stats(&out) {
            Some((loss, avg)) => {
                let ok = loss <= t.loss_max_pct && avg <= t.rtt_max_ms;
                r.verdict = if ok { Verdict::Pass } else { Verdict::Fail };
                r.detail = format!(
                    "丢包 {loss}%，avg {avg}ms（阈值 {}% / {}ms）",
                    t.loss_max_pct, t.rtt_max_ms
                );
            }
            None => {
                r.verdict = Verdict::Fail;
                r.detail = format!("ping 输出无法解析: {}", clip(&runner::one_line(&out), 80));
            }
        },
        Err(e) => {
            r.verdict = Verdict::Fail;
            r.detail = format!("ping 失败: {e:#}");
        }
    }
    RunOutcome {
        result: r,
        device: base.device,
    }
}

/// 陀螺仪流程（V2.2）：按配置命令采样两次，数值有变化即 PASS
/// （验证芯片上电、注册、出数）。固件日后提供 camera-factory 命令后改 cmd 即可。
fn gyro_flow(tx: &Sender<UiEvent>, gyro: &GyroCfg) -> RunOutcome {
    let mut r = TestResult::new(TestItem::Gyro);
    let _ = tx.send(UiEvent::Log(format!("陀螺仪采样: {}", gyro.cmd)));
    let a = adb::shell(&gyro.cmd);
    thread::sleep(Duration::from_millis(gyro.interval_ms));
    let b = adb::shell(&gyro.cmd);
    match (a, b) {
        (Ok(a), Ok(b)) => {
            let (na, nb) = (number_tokens(&a), number_tokens(&b));
            if na.is_empty() {
                r.verdict = Verdict::Fail;
                r.detail = "无数据输出（无陀螺仪或命令不适用）".into();
            } else if na == nb {
                r.verdict = Verdict::Fail;
                r.detail = format!(
                    "两次采样数值一致（芯片静止或未出数）: {}",
                    clip(&runner::one_line(&a), 80)
                );
            } else {
                r.verdict = Verdict::Pass;
                r.detail = format!("两次采样数据有变化（{} 个数值）", na.len());
            }
        }
        (Err(e), _) | (_, Err(e)) => {
            r.verdict = Verdict::Fail;
            r.detail = format!("{e:#}");
        }
    }
    RunOutcome {
        result: r,
        device: None,
    }
}

/// 提取文本中全部数值（排序后比较，陀螺仪采样对比用）。
fn number_tokens(s: &str) -> Vec<f64> {
    let mut v: Vec<f64> = s
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v
}

#[cfg(all(unix, test))] // 假 adb 桩 + ffmpeg 仅在 unix 测试构建可用
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// 端到端：设备信息 + 人工判定（阻塞应答）+ 拍照 + 录像（真 ffmpeg 解码）。
    /// 单个测试函数串行执行，避免并发测试竞争 ADB/PULL_SRC 环境变量。
    #[test]
    fn worker_sequence_flows() {
        let base = std::env::temp_dir().join(format!("e02-worker-{}", std::process::id()));
        let media = base.join("media");
        let reports = base.join("reports");
        std::fs::create_dir_all(&base).unwrap();

        // 生成 2 秒带音频测试视频；ffmpeg 不可用则整体跳过
        let clip = base.join("clip.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=320x240:rate=10",
            ])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .arg(&clip)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("ffmpeg 不可用，跳过 worker 序列测试");
            return;
        }

        // 假 adb：info 返回样例；pull 拷贝生成的视频文件冒充设备文件
        let script = base.join("adb");
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 3\"*) printf 'sn-address:002\\n';;\n\
             *\"camera-factory 2\"*) printf 'shot\\n';;\n\
             *\"camera-factory 1\"*) printf 'rec\\n';;\n\
             *\"camera-factory 9\"*) printf 'led\\n';;\n\
             *\"pull\"*) shift; dest=\"\"; for a in \"$@\"; do dest=\"$a\"; done; cp \"$PULL_SRC\" \"$dest\";;\n\
             *) printf 'ok\\n';;\nesac\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", &script);
        std::env::set_var("PULL_SRC", &clip);

        let (tx, rx) = mpsc::channel();
        let (mtx, mrx) = mpsc::channel();
        let items = vec![
            TestItem::DeviceInfo,
            TestItem::Led,
            TestItem::Photo,
            TestItem::Video,
        ];
        let media_c = media.clone();
        let reports_c = reports.clone();
        let cfg = Config {
            report_by_sn: true, // V2.3：主流程端到端顺带验证 SN 归档
            ..Config::default()
        };
        let handle =
            thread::spawn(move || run_sequence(&items, &tx, &mrx, &media_c, &reports_c, &cfg));

        let (mut manual_asked, mut photo_ready, mut video_ready) = (0, 0, 0);
        let mut verdicts: Vec<(TestItem, Verdict)> = Vec::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(120)) {
            match ev {
                UiEvent::Manual { .. } => {
                    manual_asked += 1;
                    mtx.send(UiResponse::Manual(true)).unwrap();
                }
                UiEvent::PhotoReady(_) => photo_ready += 1,
                UiEvent::VideoReady {
                    count, has_audio, ..
                } => {
                    video_ready += 1;
                    assert!(count >= 3, "2 秒 2fps 应至少 3 帧");
                    assert!(has_audio);
                }
                UiEvent::Result(r, _) => verdicts.push((r.item, r.verdict)),
                _ => {}
            }
        }
        let report = handle.join().unwrap().expect("应产出报告路径");

        assert_eq!(manual_asked, 1);
        assert_eq!(photo_ready, 1);
        assert_eq!(video_ready, 1);
        let get = |item: TestItem| {
            verdicts
                .iter()
                .find(|(i, _)| *i == item)
                .map(|(_, v)| *v)
                .unwrap()
        };
        assert_eq!(get(TestItem::DeviceInfo), Verdict::Pass);
        assert_eq!(get(TestItem::Led), Verdict::ManualYes);
        assert_eq!(get(TestItem::Photo), Verdict::Pass);
        assert_eq!(get(TestItem::Video), Verdict::Pass);

        let text = std::fs::read_to_string(&report).unwrap();
        assert!(text.contains("sn-address:002"));
        assert!(text.contains("指示灯测试:PASS"));
        assert!(text.contains("工位: complete"));
        assert!(text.contains("汇总: 4 项，PASS 4，FAIL 0，跳过 0"));
        // SN 归档 + JSON 成对
        assert!(report.starts_with(reports.join("002")));
        assert!(report.with_extension("json").exists());

        // —— V2.1 按键引导：三场景（顺序执行，防 ADB 环境变量竞争）——
        let kscript = base.join("adb-keys");
        let keys_case = |body: &str| {
            format!(
                "#!/bin/sh\ncase \"$*\" in\n*\"camera-factory 15\"*) {body};;\n\
                 *) printf 'ok\\n';;\nesac\n"
            )
        };
        let items = |i: TestItem| vec![i];
        // 1) 输出全键码：自动收齐 PASS，全程无人工介入
        let (vs, n) = scenario(
            &kscript,
            items(TestItem::Keys),
            &media,
            &reports,
            Config::default(),
            &keys_case("printf '5\\n6\\n4\\n'"),
            || unreachable!("全键码不应触发人工应答"),
        );
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));
        // 2) 只回 5：未收齐 → 强制改判失败
        let (vs, n) = scenario(
            &kscript,
            items(TestItem::Keys),
            &media,
            &reports,
            Config::default(),
            &keys_case("printf '5\\n'"),
            || Some(UiResponse::KeysForce(false)),
        );
        assert_eq!((vs[0].1, n), (Verdict::ManualNo, 1));
        // 3) 首轮缺键 → 重测 → 第二轮全键码收齐 PASS
        std::env::set_var("KEYS_CNT", base.join("cnt").display().to_string());
        let (vs, n) = scenario(
            &kscript,
            items(TestItem::Keys),
            &media,
            &reports,
            Config::default(),
            &keys_case(
                "n=$(cat \"$KEYS_CNT\" 2>/dev/null || echo 0); n=$((n+1)); \
                 echo $n > \"$KEYS_CNT\"; [ \"$n\" -ge 2 ] && printf '5\\n6\\n4\\n' \
                 || printf '5\\n'",
            ),
            || Some(UiResponse::KeysRetry),
        );
        assert_eq!((vs[0].1, n), (Verdict::Pass, 1));
        std::env::remove_var("KEYS_CNT");

        // —— V2.2 WiFi 速率（ping 法）——
        let wscript = base.join("adb-wifi");
        // 注：数据里的 % 需经 %s 格式串输出（printf 参数含 % 会被 dash 当格式符）
        let wifi_case = "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 5\"*) printf 'wifi ok\\n';;\n\
             *\"ip route\"*) printf 'default via 192.168.1.1 dev wlan0\\n';;\n\
             *\"ping\"*) printf '%s\\n%s\\n' \
             '20 packets transmitted, 20 received, 0% packet loss' \
             'rtt min/avg/max/mdev = 1.2/23.4/99.0/5.0 ms';;\n\
             *) printf 'ok\\n';;\nesac\n";
        // driver 深度：不 ping，驱动过即过
        let (vs, n) = scenario(
            &wscript,
            items(TestItem::Wifi),
            &media,
            &reports,
            Config::default(),
            wifi_case,
            || unreachable!(),
        );
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));
        // throughput 深度：驱动过 + ping 统计达标
        let mut cfg = Config::default();
        cfg.wifi.depth = crate::config::WifiDepth::Throughput;
        let (vs, n) = scenario(
            &wscript,
            items(TestItem::Wifi),
            &media,
            &reports,
            cfg,
            wifi_case,
            || unreachable!(),
        );
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));

        // —— V2.2 陀螺仪 ——
        let gscript = base.join("adb-gyro");
        let gitems = items(TestItem::Gyro);
        // 两次采样数值变化 → PASS
        std::env::set_var("GYRO_CNT", base.join("gcnt").display().to_string());
        let (vs, _) = scenario(
            &gscript,
            gitems.clone(),
            &media,
            &reports,
            Config::default(),
            "#!/bin/sh\ncase \"$*\" in\n\
             *\"sensorservice\"*) n=$(cat \"$GYRO_CNT\" 2>/dev/null || echo 0); n=$((n+1)); \
             echo $n > \"$GYRO_CNT\"; echo \"gyro: $n.$n $n.$n $n.$n\";;\n\
             *) printf 'ok\\n';;\nesac\n",
            || unreachable!(),
        );
        assert_eq!(vs[0].1, Verdict::Pass);
        // 数值恒定 → FAIL
        let (vs, _) = scenario(
            &gscript,
            gitems,
            &media,
            &reports,
            Config::default(),
            "#!/bin/sh\ncase \"$*\" in\n\
             *\"sensorservice\"*) printf 'gyro: 1.0 2.0 3.0\\n';;\n\
             *) printf 'ok\\n';;\nesac\n",
            || unreachable!(),
        );
        assert_eq!(vs[0].1, Verdict::Fail);
        std::env::remove_var("GYRO_CNT");

        // —— V2.3 超时 ——
        let tcfg = Config {
            timeouts: crate::config::TimeoutsCfg {
                item_secs: 1,
                manual_secs: 1,
            },
            ..Config::default()
        };
        // 1) 命令阻塞超过 item_secs：超时强杀判 FAIL，序列不卡死
        let (vs, n) = scenario(
            &base.join("adb-slow"),
            items(TestItem::Wifi),
            &media,
            &reports,
            tcfg.clone(),
            "#!/bin/sh\ncase \"$*\" in\n*\"camera-factory 5\"*) sleep 5; printf 'wifi ok\\n';;\n\
             *) printf 'ok\\n';;\nesac\n",
            || unreachable!(),
        );
        assert_eq!((vs[0].1, n), (Verdict::Fail, 0));
        // 2) 按键未收齐且无人应答：manual_secs 到点自动判失败
        let (vs, n) = scenario(
            &base.join("adb-keys-t"),
            items(TestItem::Keys),
            &media,
            &reports,
            tcfg.clone(),
            &keys_case("printf '5\\n'"),
            || None,
        );
        assert_eq!((vs[0].1, n), (Verdict::ManualNo, 1));
        // 3) 指示灯人工判定无人应答：超时判不合格
        let (vs, n) = scenario(
            &base.join("adb-led-t"),
            items(TestItem::Led),
            &media,
            &reports,
            tcfg,
            "#!/bin/sh\nprintf 'led\\n'\n",
            || None,
        );
        assert_eq!((vs[0].1, n), (Verdict::ManualNo, 0));

        std::env::remove_var("ADB");
        std::env::remove_var("PULL_SRC");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 场景驱动：写入完整假 adb 脚本后跑 `items` 序列；
    /// 每次收到 awaiting 应答请求时调 `respond`（返回 None = 不应答，模拟无人操作）。
    /// 返回 (逐项判定, awaiting 次数)。
    fn scenario(
        script: &Path,
        items: Vec<TestItem>,
        media: &Path,
        reports: &Path,
        cfg: Config,
        script_text: &str,
        mut respond: impl FnMut() -> Option<UiResponse>,
    ) -> (Vec<(TestItem, Verdict)>, usize) {
        std::fs::write(script, script_text).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", script);

        let (tx, rx) = mpsc::channel();
        let (mtx, mrx) = mpsc::channel();
        let (m, r) = (media.to_path_buf(), reports.to_path_buf());
        let handle = thread::spawn(move || run_sequence(&items, &tx, &mrx, &m, &r, &cfg));

        let (mut verdicts, mut awaiting) = (Vec::new(), 0usize);
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(60)) {
            match ev {
                UiEvent::KeysUpdate { awaiting: true, .. } => {
                    awaiting += 1;
                    if let Some(resp) = respond() {
                        mtx.send(resp).unwrap();
                    }
                }
                UiEvent::Result(res, _) => verdicts.push((res.item, res.verdict)),
                _ => {}
            }
        }
        handle.join().unwrap();
        (verdicts, awaiting)
    }
}
