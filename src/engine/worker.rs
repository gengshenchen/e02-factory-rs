//! 测试序列 worker：在独立线程阻塞执行 adb/ffmpeg 流程，
//! 经 [`UiEvent`] 回传状态；人工判定阻塞等待 [`UiResponse`]。

use crate::adb;
use crate::engine::events::{UiEvent, UiResponse};
use crate::engine::runner::{self, RunOutcome};
use crate::engine::{TestItem, TestResult, Verdict};
use crate::player;
use crate::protocol::{self, CameraFactory, DeviceInfo};
use crate::report;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::Duration;

/// 一键全测顺序，对应原程序 OnBnClickedButton14 的调用顺序。
pub const BATCH_ORDER: [TestItem; 11] = [
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
];

/// 同步执行测试序列（worker 线程与测试共用）。
/// 返回报告文件路径；写入失败为 None。
pub fn run_sequence(
    items: &[TestItem],
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    media_dir: &Path,
    reports_dir: &Path,
) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(media_dir);
    let _ = std::fs::create_dir_all(reports_dir);
    let _ = tx.send(UiEvent::Started);

    let mut device = DeviceInfo::default();
    let mut results: Vec<TestResult> = Vec::new();
    for &item in items {
        let _ = tx.send(UiEvent::Current(item));
        let RunOutcome {
            result,
            device: dev,
        } = run_item(item, tx, manual, media_dir);
        if let Some(d) = &dev {
            device.merge(d.clone());
        }
        let _ = tx.send(UiEvent::Result(result.clone(), dev));
        results.push(result);
    }
    report::write_report(reports_dir, &device, &results)
}

fn run_item(
    item: TestItem,
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    media_dir: &Path,
) -> RunOutcome {
    match item {
        TestItem::Photo => photo_flow(tx, media_dir),
        TestItem::Video => video_flow(tx, media_dir),
        TestItem::Led => manual_flow(item, "指示灯闪烁是否正常？", tx, manual, Some(3)),
        TestItem::MicSpeaker => manual_flow(item, "MIC 与喇叭声音是否正常？", tx, manual, None),
        _ => runner::run_auto(item),
    }
}

/// 拍照流程：拍摄 → 拉回 → UI 显示（对应原 OnBnClickedButtonCamera）。
fn photo_flow(tx: &Sender<UiEvent>, media_dir: &Path) -> RunOutcome {
    let mut r = TestResult::new(TestItem::Photo);
    let local = media_dir.join("testjpeg.jpg");
    // 先删旧图，防止上一台设备的图片冒充本轮结果（原程序同逻辑）
    let _ = std::fs::remove_file(&local);
    let outcome = adb::shell(&CameraFactory::Photo.device_cmd())
        .map_err(|e| format!("{e:#}"))
        .and_then(|_| {
            thread::sleep(Duration::from_millis(1000)); // 原程序 Sleep(1000)
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
    let outcome = adb::shell(&CameraFactory::Video.device_cmd())
        .map_err(|e| format!("{e:#}"))
        .and_then(|_| {
            thread::sleep(Duration::from_millis(900)); // 原程序 Sleep(900)
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
) -> RunOutcome {
    let mut r = TestResult::new(item);
    let cmd = item.factory_cmd().device_cmd();
    match repeat {
        // 指示灯：原程序连发 3 次命令
        Some(n) => {
            for _ in 0..n {
                let _ = adb::shell(&cmd);
            }
        }
        None => {
            let _ = adb::shell(&cmd);
        }
    }
    let _ = tx.send(UiEvent::Manual {
        item,
        question: question.to_string(),
    });
    match manual.recv() {
        Ok(UiResponse::Manual(true)) => r.verdict = Verdict::ManualYes,
        Ok(UiResponse::Manual(false)) => r.verdict = Verdict::ManualNo,
        Err(_) => {
            r.verdict = Verdict::Skipped;
            r.detail = "界面已退出，判定中断".into();
        }
    }
    RunOutcome {
        result: r,
        device: None,
    }
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
        let handle = thread::spawn(move || run_sequence(&items, &tx, &mrx, &media_c, &reports_c));

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

        std::env::remove_var("ADB");
        std::env::remove_var("PULL_SRC");
        let _ = std::fs::remove_dir_all(&base);
    }
}
