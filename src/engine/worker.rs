//! 测试序列 worker：在独立线程阻塞执行 adb/ffmpeg 流程，
//! 经 [`UiEvent`] 回传状态；人工判定阻塞等待 [`UiResponse`]。

use crate::adb;
use crate::config::{Config, KeysCfg, WifiCfg};
use crate::engine::events::{UiEvent, UiResponse};
use crate::engine::runner::{self, RunOutcome};
use crate::engine::{TestItem, TestResult, Verdict};
use crate::exec;
use crate::player;
use crate::protocol::{self, CameraFactory, DeviceInfo};
use crate::report;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// 一键全测执行顺序（V3.6 重排，按产线操作流）：
/// 自动项——设备信息（核 SN）→ 蓝牙 MAC → WiFi → SD 卡（录像前先确认存储）
/// → 电池 → 陀螺仪；人工项——录像（长动作先启动、回放判定）→ 拍照 →
/// 指示灯 → 麦克风 → 按键（操作员收尾）。run_sequence 内仍会按
/// is_manual 稳定排序：自动项先行，人工项按此相对顺序殿后。
pub const BATCH_ORDER: [TestItem; 11] = [
    TestItem::DeviceInfo,
    TestItem::BtMac,
    TestItem::Wifi,
    TestItem::SdCard,
    TestItem::Battery,
    TestItem::Gyro,
    TestItem::Video,
    TestItem::Photo,
    TestItem::Led,
    TestItem::MicSpeaker,
    TestItem::Keys,
];

/// 同步执行测试序列（worker 线程与测试共用）。
/// 自动项先行、人工项殿后（V3 产线反馈：操作员可在自动项期间离手）。
/// 测试项在批序中的位置（报告行序基准；不在批序的排最后）。
fn batch_pos(item: TestItem) -> usize {
    BATCH_ORDER
        .iter()
        .position(|&i| i == item)
        .unwrap_or(usize::MAX)
}

/// 结果入列（V3.9）：按测试项替换既有槽位（单项补测合并整机报告），
/// 无则按批序插入。
fn upsert(results: &mut Vec<TestResult>, r: TestResult) {
    if let Some(slot) = results.iter_mut().find(|s| s.item == r.item) {
        *slot = r;
    } else {
        let at = results
            .iter()
            .position(|s| batch_pos(s.item) > batch_pos(r.item))
            .unwrap_or(results.len());
        results.insert(at, r);
    }
}

/// 整机基线（V3.9 单项补测）：上一轮设备信息 + 各项结果。单项补测以
/// 此为底稿，新结果按项替换后重写整机报告（一机一日一份不散页）；
/// 全测传默认值（空白，全新开始）。
#[derive(Debug, Default)]
pub struct RunBase {
    pub device: DeviceInfo,
    pub results: Vec<TestResult>,
}

/// 返回报告文件路径；写入失败为 None。
pub fn run_sequence(
    items: &[TestItem],
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    media_dir: &Path,
    reports_dir: &Path,
    cfg: &Config,
    base: RunBase,
) -> Option<PathBuf> {
    exec::clear_abort(); // 新序列不残留上一轮的停止请求
    let _ = std::fs::create_dir_all(media_dir);
    let _ = std::fs::create_dir_all(reports_dir);
    let _ = tx.send(UiEvent::Started);
    let (started_wall, started) = (SystemTime::now(), Instant::now());
    // 单项超时（V2.3）：序列期间子进程统一看门狗，防止固件命令阻塞卡死序列
    let prev_timeout = exec::default_timeout();
    exec::set_default_timeout(timeout_opt(cfg.timeouts.item_secs));
    // 序列前置（V3.5 #8）：宿主机时间写入设备再执行固件校时指令
    sync_time(tx);

    // 自动项先行、人工项殿后（稳定排序保持组内原顺序）
    let mut ordered: Vec<TestItem> = items.to_vec();
    ordered.sort_by_key(|i| i.is_manual());
    let first_manual = ordered.iter().position(|i| i.is_manual());

    let mut device = base.device;
    let mut results = base.results;
    results.sort_by_key(|r| batch_pos(r.item)); // 底稿按批序归位，报告行序稳定
    let _ = tx.send(UiEvent::Log(format!(
        "序列开始，工位 {}，共 {} 项（自动 {} + 人工 {}），单项超时 {}",
        cfg.station,
        ordered.len(),
        ordered.iter().filter(|i| !i.is_manual()).count(),
        ordered.iter().filter(|i| i.is_manual()).count(),
        timeout_desc(cfg.timeouts.item_secs)
    )));
    let mut written: Option<PathBuf> = None;
    for (idx, &item) in ordered.iter().enumerate() {
        if Some(idx) == first_manual {
            let _ = tx.send(UiEvent::Log(
                "自动项完成，进入人工判定项（请到工位操作）".into(),
            ));
        }
        let _ = tx.send(UiEvent::Current(item));
        let _ = tx.send(UiEvent::Log(format!("【{}】开始", item.label())));
        let item_started = Instant::now();
        let RunOutcome {
            mut result,
            device: dev,
        } = run_item(item, tx, manual, media_dir, cfg);
        result.secs = item_started.elapsed().as_secs();
        // 停止请求落在当前项：命令被强杀记为 FAIL 的改记跳过，不计失败
        if exec::abort_requested() && result.verdict == Verdict::Fail {
            result.verdict = Verdict::Skipped;
            result.detail = exec::abort_reason().text().into();
        }
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
        upsert(&mut results, result);
        // 报告边测边写：中途卡死/退出也留有报告；文件名同机同日固定，
        // 重测覆盖更新，SN 中途解析到后改名并删旧文件（V3.2）
        let _ = write_update(
            reports_dir,
            &meta(cfg, started_wall, started),
            &device,
            &results,
            false,
            &mut written,
        );
        if exec::abort_requested() {
            // 剩余项全部记跳过，报告仍完整
            let rest = &ordered[idx + 1..];
            for &skip in rest {
                let mut r = TestResult::new(skip);
                r.verdict = Verdict::Skipped;
                r.detail = exec::abort_reason().text().into();
                upsert(&mut results, r.clone());
                let _ = tx.send(UiEvent::Result(r, None));
            }
            let _ = tx.send(UiEvent::Log(format!(
                "{}，剩余 {} 项跳过",
                exec::abort_reason().text(),
                rest.len()
            )));
            break;
        }
    }
    exec::set_default_timeout(prev_timeout);
    write_update(
        reports_dir,
        &meta(cfg, started_wall, started),
        &device,
        &results,
        true,
        &mut written,
    )
}

/// 写报告并在文件名变化（SN 解析到）时删除旧文件，返回最终路径。
fn write_update(
    reports_dir: &Path,
    meta: &report::ReportMeta,
    device: &DeviceInfo,
    results: &[TestResult],
    finished: bool,
    written: &mut Option<PathBuf>,
) -> Option<PathBuf> {
    let path = report::write_report(reports_dir, meta, device, results, finished);
    if let (Some(new), Some(old)) = (&path, &*written) {
        if new != old {
            let _ = std::fs::remove_file(old);
        }
    }
    *written = path.clone();
    path
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
        started: report::time_str(started_wall),
        elapsed_secs: started.elapsed().as_secs(),
    }
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
        TestItem::Photo => photo_flow(tx, manual, media_dir, cfg.timeouts.manual_secs),
        TestItem::Video => video_flow(tx, manual, media_dir, cfg.timeouts.manual_secs),
        TestItem::Keys => keys_flow(tx, manual, &cfg.keys, cfg.timeouts.manual_secs),
        TestItem::Wifi => wifi_flow(tx, &cfg.wifi),
        TestItem::Led => dialog_flow(
            item,
            "指示灯显示是否正常？",
            tx,
            manual,
            cfg.timeouts.manual_secs,
        ),
        TestItem::MicSpeaker => mic_flow(tx, manual, cfg.timeouts.manual_secs),
        _ => runner::run_auto(item),
    }
}

/// 序列前置：宿主机当前时间写入设备并执行固件校时（V3.5 #8）。
/// `adb shell date -s "YYYY-MM-DD HH:MM"` 后执行 `camera-factory 19`；
/// 失败只记日志不阻断（时间同步为辅助步骤，测试照常进行）。
fn sync_time(tx: &Sender<UiEvent>) {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let _ = tx.send(UiEvent::Log(format!("同步设备时间: date -s \"{now}\"")));
    for (cmd, what) in [
        (format!("date -s \"{now}\""), "时间写入"),
        (CameraFactory::TimeSync.device_cmd(), "固件校时"),
    ] {
        match adb::shell(&cmd) {
            Ok(out) => {
                let _ = tx.send(UiEvent::Log(format!(
                    "{what}: {}",
                    clip(&runner::one_line(&out), 60)
                )));
            }
            Err(e) => {
                let _ = tx.send(UiEvent::Log(format!("{what}失败（继续测试）: {e:#}")));
            }
        }
    }
}

/// 拍照流程（V3.5）：设备返回值判定 → 拉回显示 → 人工确认图像质量。
fn photo_flow(
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    media_dir: &Path,
    manual_secs: u64,
) -> RunOutcome {
    let mut r = TestResult::new(TestItem::Photo);
    let local = media_dir.join("testjpeg.jpg");
    // 先删旧图，防止上一台设备的图片冒充本轮结果（原程序同逻辑）
    let _ = std::fs::remove_file(&local);
    // 1) 设备返回值判定：输出含 FAIL 即失败，不拉回
    let cmd = CameraFactory::Photo.device_cmd();
    let _ = tx.send(UiEvent::Log(format!("执行 adb shell {cmd}")));
    let judged = adb::shell(&cmd)
        .map_err(|e| format!("{e:#}"))
        .and_then(|out| {
            if protocol::output_ok(&out) {
                Ok(out)
            } else {
                Err(format!("设备返回失败: {}", runner::one_line(&out)))
            }
        });
    if let Err(e) = judged {
        r.verdict = Verdict::Fail;
        r.detail = e;
        return RunOutcome {
            result: r,
            device: None,
        };
    }
    // 2) 拉回并显示
    thread::sleep(Duration::from_millis(1000)); // 原程序 Sleep(1000)
    let _ = tx.send(UiEvent::Log(format!(
        "adb pull {} -> {}",
        protocol::REMOTE_PHOTO,
        local.display()
    )));
    let pulled = adb::pull(protocol::REMOTE_PHOTO, &local)
        .map_err(|e| format!("{e:#}"))
        .and_then(|_| {
            if local.exists() {
                Ok(())
            } else {
                Err("adb pull 成功但本地文件不存在".to_string())
            }
        });
    if let Err(e) = pulled {
        r.verdict = Verdict::Fail;
        r.detail = e;
        return RunOutcome {
            result: r,
            device: None,
        };
    }
    let _ = tx.send(UiEvent::PhotoReady(local));
    // 3) 人工确认图像质量（清晰/色彩/条纹）
    send_manual(tx, "拍照测试", "拍照图像是否清晰、色彩正常？");
    match wait_answer(manual, manual_secs) {
        Ok(true) => {
            r.verdict = Verdict::ManualYes;
            r.detail = "图像已拉回，人工确认合格".into();
        }
        Ok(false) => {
            r.verdict = Verdict::ManualNo;
            r.detail = "图像已拉回，人工判定不合格".into();
        }
        Err((v, d)) => {
            r.verdict = v;
            r.detail = d;
        }
    }
    RunOutcome {
        result: r,
        device: None,
    }
}

/// 录像流程（V3.5）：设备返回值判定 → 拉回解码抽帧回放 → 人工确认
/// 画面清晰/条纹/声音（对应原 OnBnClickedButtonVedio + PlayVideo）。
fn video_flow(
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    media_dir: &Path,
    manual_secs: u64,
) -> RunOutcome {
    let mut r = TestResult::new(TestItem::Video);
    let local = media_dir.join("testvideo.mp4");
    let _ = std::fs::remove_file(&local);
    // 1) 设备返回值判定：输出含 FAIL 即失败，不拉回
    let cmd = CameraFactory::Video.device_cmd();
    let _ = tx.send(UiEvent::Log(format!("执行 adb shell {cmd}")));
    let judged = adb::shell(&cmd)
        .map_err(|e| format!("{e:#}"))
        .and_then(|out| {
            if protocol::output_ok(&out) {
                Ok(out)
            } else {
                Err(format!("设备返回失败: {}", runner::one_line(&out)))
            }
        });
    if let Err(e) = judged {
        r.verdict = Verdict::Fail;
        r.detail = e;
        return RunOutcome {
            result: r,
            device: None,
        };
    }
    // 2) 拉回 + 解码抽帧（原程序仅"能播即过"，这里加严为全程解码成功）
    thread::sleep(Duration::from_millis(900)); // 原程序 Sleep(900)
    let _ = tx.send(UiEvent::Log(format!(
        "adb pull {} -> {}",
        protocol::REMOTE_VIDEO,
        local.display()
    )));
    if let Err(e) = adb::pull(protocol::REMOTE_VIDEO, &local).map_err(|e| format!("{e:#}")) {
        r.verdict = Verdict::Fail;
        r.detail = e;
        return RunOutcome {
            result: r,
            device: None,
        };
    }
    let _ = tx.send(UiEvent::Log(format!("ffmpeg 解码 {}", local.display())));
    let frames_dir = media_dir.join("frames");
    let count = match player::extract_frames(&local, &frames_dir, 5, 640) {
        Ok(c) => c,
        Err(e) => {
            r.verdict = Verdict::Fail;
            r.detail = format!("视频解码失败: {e:#}");
            return RunOutcome {
                result: r,
                device: None,
            };
        }
    };
    let has_audio = player::probe_has_audio(&local).unwrap_or(false);
    // 抽音轨供本地播放（V3.6：操作员要听到录像声音才能判定）
    let wav = media_dir.join("audio.wav");
    let audio = if has_audio {
        match player::extract_audio(&local, &wav) {
            Ok(true) => Some(wav),
            Ok(false) => None,
            Err(e) => {
                let _ = tx.send(UiEvent::Log(format!("抽音轨失败（继续）: {e:#}")));
                None
            }
        }
    } else {
        None
    };
    let _ = tx.send(UiEvent::VideoReady {
        frames_dir,
        count,
        has_audio,
        audio,
    });
    // 3) 人工确认：媒体区回放（对应原程序 Sleep(2500) 观察窗），
    //    弹窗落在右上面板区，不遮挡画面
    thread::sleep(Duration::from_millis(2500));
    send_manual(tx, "录像测试", "录像画面是否清晰？有无条纹？声音是否正常？");
    let audio_note = if has_audio {
        "含音频流"
    } else {
        "无音频流"
    };
    match wait_answer(manual, manual_secs) {
        Ok(true) => {
            r.verdict = Verdict::ManualYes;
            r.detail = format!("解码 {count} 帧，{audio_note}，人工确认合格");
        }
        Ok(false) => {
            r.verdict = Verdict::ManualNo;
            r.detail = format!("解码 {count} 帧，{audio_note}，人工判定不合格");
        }
        Err((v, d)) => {
            r.verdict = v;
            r.detail = d;
        }
    }
    RunOutcome {
        result: r,
        device: None,
    }
}

/// 发人工判定弹窗（先弹窗再执行设备动作，操作员不等空界面——V3.2 教训）。
fn send_manual(tx: &Sender<UiEvent>, title: &str, question: &str) {
    let _ = tx.send(UiEvent::Manual {
        title: title.to_string(),
        question: question.to_string(),
    });
}

/// 等待操作员应答。Ok = 操作员结论；Err = 超时/断开/停止的终态（判定+原因）。
fn wait_answer(manual: &Receiver<UiResponse>, manual_secs: u64) -> Result<bool, (Verdict, String)> {
    match recv_manual(manual, manual_secs) {
        Ok(UiResponse::Manual(yes)) => Ok(yes),
        // Keys* 应答不会出现在此类弹窗，视为应答异常
        Ok(_) => Err((Verdict::Skipped, "应答类型不匹配".into())),
        Err(WaitEnd::Timeout) => Err((
            Verdict::ManualNo,
            format!("超时未人工判定（{manual_secs}s），自动判失败"),
        )),
        Err(WaitEnd::Disconnected) => Err((Verdict::Skipped, "界面已退出，判定中断".into())),
        Err(WaitEnd::Aborted) => Err((Verdict::Skipped, exec::abort_reason().text().into())),
    }
}

/// 纯人工判定项（V3.5 指示灯：不再发设备指令，仅弹窗确认）。
fn dialog_flow(
    item: TestItem,
    question: &str,
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    manual_secs: u64,
) -> RunOutcome {
    let mut r = TestResult::new(item);
    send_manual(tx, item.label(), question);
    match wait_answer(manual, manual_secs) {
        Ok(true) => r.verdict = Verdict::ManualYes,
        Ok(false) => r.verdict = Verdict::ManualNo,
        Err((v, d)) => {
            r.verdict = v;
            r.detail = d;
        }
    }
    RunOutcome {
        result: r,
        device: None,
    }
}

/// 麦克风流程（V3.5）：主麦 camera-factory 13、副麦 camera-factory 14，
/// 各自弹窗先行，操作员听设备端实时回环判定，两段全部合格才通过。
///
/// V3.6 关键改动：不再等 13/14 命令退出。T33 旧固件约 2s 自退
/// （`Loopback m/b on` → `1` → 回提示符）；当前固件（v2.4.2 实测）回环
/// 进程存活期间才有声音、**不会自己退出**——阻塞等待会把整条流程卡死，
/// 操作员点"合格"也无人消费（真机反馈"点击合格后无响应"的根因）。
/// 现在启动即转入人工等待，应答后 kill 本地 adb 会话结束回环，
/// 两种固件行为都兼容。
fn mic_flow(tx: &Sender<UiEvent>, manual: &Receiver<UiResponse>, manual_secs: u64) -> RunOutcome {
    let mut r = TestResult::new(TestItem::MicSpeaker);
    let phases: [(&str, CameraFactory, &str); 2] = [
        (
            "主麦",
            CameraFactory::MicSpeaker,
            "请对主麦说话，录音回放声音是否正常？",
        ),
        (
            "副麦",
            CameraFactory::MicSecondary,
            "请对副麦说话，录音回放声音是否正常？",
        ),
    ];
    let mut marks: Vec<(&str, bool)> = Vec::new();
    for (name, factory, question) in phases {
        send_manual(tx, &format!("{name}判定"), question);
        let mut loopback = start_loopback(tx, &factory.device_cmd());
        // 输出不作自动判定，人工听感为准
        match wait_answer(manual, manual_secs) {
            Ok(ok) => {
                stop_loopback(&mut loopback);
                marks.push((name, ok));
            }
            Err((v, d)) => {
                stop_loopback(&mut loopback);
                r.verdict = v;
                r.detail = format!("{name}: {d}");
                return RunOutcome {
                    result: r,
                    device: None,
                };
            }
        }
    }
    let all_ok = marks.iter().all(|&(_, ok)| ok);
    r.verdict = if all_ok {
        Verdict::ManualYes
    } else {
        Verdict::ManualNo
    };
    r.detail = marks
        .iter()
        .map(|&(name, ok)| {
            if ok {
                format!("{name}合格")
            } else {
                format!("{name}不合格")
            }
        })
        .collect::<Vec<_>>()
        .join("、");
    RunOutcome {
        result: r,
        device: None,
    }
}

/// 等待结果的终止原因（人工应答等待用）。
enum WaitEnd {
    Timeout,
    Disconnected,
    /// 操作员按了停止测试
    Aborted,
}

/// 启动回环命令并转发输出行到日志（读线程随管道 EOF 自然退出；
/// T33 旧固件 2s 自退时线程即结束，新固件常驻到应答后 kill）。
fn start_loopback(tx: &Sender<UiEvent>, cmd: &str) -> Option<std::process::Child> {
    let _ = tx.send(UiEvent::Log(format!(
        "执行 adb shell {cmd}（设备实时回环，请对麦克风说话）"
    )));
    match adb::spawn_shell(cmd) {
        Ok(mut child) => {
            if let Some(stdout) = child.stdout.take() {
                let tx_live = tx.clone();
                thread::spawn(move || {
                    use std::io::{BufRead, BufReader};
                    for line in BufReader::new(stdout).lines() {
                        match line {
                            Ok(l) => {
                                let l = crate::exec::decode_out(l.as_bytes());
                                let l = l.trim_end_matches(['\n', '\r']).to_string();
                                let _ = tx_live.send(UiEvent::Log(format!("回环: {l}")));
                            }
                            // 非法 UTF-8 行：BufRead::lines 报错即断流，
                            // 与主流程无关（听感判定），静默结束
                            Err(_) => break,
                        }
                    }
                });
            }
            Some(child)
        }
        Err(e) => {
            let _ = tx.send(UiEvent::Log(format!(
                "{cmd} 启动失败（继续人工判定）: {e:#}"
            )));
            None
        }
    }
}

/// 结束回环：杀本地 adb 会话（设备侧随会话断开停止回环）。
fn stop_loopback(child: &mut Option<std::process::Child>) {
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// 等待 UI 应答；`secs` 0 = 无限等。等待期间以 250ms 切片轮询停止请求，
/// 保证"停止测试"能在人工等待阶段立即生效。
fn recv_manual(manual: &Receiver<UiResponse>, secs: u64) -> Result<UiResponse, WaitEnd> {
    let deadline = timeout_opt(secs).map(|d| Instant::now() + d);
    loop {
        let slice = deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(250))
            .min(Duration::from_millis(250));
        if slice.is_zero() {
            return Err(WaitEnd::Timeout);
        }
        match manual.recv_timeout(slice) {
            Ok(r) => return Ok(r),
            Err(RecvTimeoutError::Timeout) => {
                if exec::abort_requested() {
                    return Err(WaitEnd::Aborted);
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    return Err(WaitEnd::Timeout);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Err(WaitEnd::Disconnected),
        }
    }
}

/// 按键监听阶段结果：读线程发 `Update`/`Collected`/`Done`，
/// `Timeout`/`Aborted` 是主流程本地终态。
enum KeysListen {
    /// 流中增量检测到新键（主循环转发 UI 点亮弹窗）
    Update(Vec<u8>),
    /// 流中收齐全部必测键（V3.6.3：不等固件退出即结束）
    Collected(Vec<u8>),
    /// 监听命令自行退出（旧固件行为），携带完整输出
    Done(String),
    /// 单项超时耗尽仍未收齐
    Timeout,
    /// 手动停止
    Aborted,
}

/// 按键引导流程（V2.1）：弹窗提示按键 → 固件监听窗口内操作员按键 →
/// 回显已检测到的键，收齐自动 PASS；未收齐由操作员重测/改判。
/// 必测键集合由工位配置裁剪（V2.2）。
///
/// V3.5：固件回读格式为 `key 4/5/6`（多功能键/开机键·音量-/音量+）；
/// 弹窗在命令发出前先行打开（此前首个事件要等检测到键或超时才发，
/// 真机监听窗口无输出时操作员全程看不到弹窗）。
///
/// V3.6.3：固件 v2.4.2 的 15 监听进程收齐按键后并不退出（同 13/14 回环
/// 的行为漂移），原实现把判定押在命令返回上，真机复现"弹窗三键全亮但
/// 流程不动"。改为 spawn + 读线程：读线程增量解析实时点亮（V3.1 行为
/// 保留），发现收齐立即发信号，主流程杀本地会话直接判 PASS；命令自行
/// 退出（旧固件）或单项超时才进入"未收齐"人工弹窗。
fn keys_flow(
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    keys: &KeysCfg,
    manual_secs: u64,
) -> RunOutcome {
    let mut r = TestResult::new(TestItem::Keys);
    let cmd = CameraFactory::Keys.device_cmd();
    loop {
        let _ = tx.send(UiEvent::KeysUpdate {
            detected: Vec::new(),
            awaiting: false,
        });
        let _ = tx.send(UiEvent::Log(format!("执行 adb shell {cmd}，请依次按键...")));
        // V3.6.3：spawn + 读线程（同 mic 回环模式）。读线程沿用 V3.1 的
        // 增量解析——弹窗实时点亮"已按下"（原 MFC 版要等命令退出才显示
        // 按下什么键）；发现收齐立即发信号，不等固件退出。
        let mut child = match adb::spawn_shell(&cmd) {
            Ok(c) => Some(c),
            Err(e) => {
                r.verdict = Verdict::Fail;
                r.detail = format!("启动 {cmd} 失败: {e:#}");
                break;
            }
        };
        let (sig_tx, sig_rx) = channel::<KeysListen>();
        if let Some(stdout) = child.as_mut().and_then(|c| c.stdout.take()) {
            let required = keys.required.clone();
            thread::spawn(move || {
                use std::io::{BufRead, BufReader};
                let mut reader = BufReader::new(stdout);
                let mut acc = String::new();
                let mut seen = Vec::new();
                let mut buf = Vec::new();
                // 读线程必以一个信号收尾（收齐或 EOF），主流程据此判定。
                // 增量点亮也走本通道由主循环转发——读线程绝不持有 UI Sender：
                // 杀掉 adb 后孤儿孙进程可能仍握着管道，读线程会吊到它退出，
                // 若持有 Sender 克隆，UI 事件通道迟迟不关，界面侧等不到收尾
                let sig = loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf) {
                        // EOF：监听命令退出（旧固件自退行为），按完整输出判定
                        Ok(0) => break KeysListen::Done(acc),
                        Ok(_) => {
                            // 按字节读行再解码：GBK 输出不被 BufRead::lines 拦腰截断
                            let line = exec::decode_out(&buf);
                            let line = line.trim_end_matches(['\n', '\r']);
                            acc.push_str(line);
                            acc.push('\n');
                            let got = protocol::parse_keys_codes(&acc);
                            if got != seen {
                                seen = got.clone();
                                if sig_tx.send(KeysListen::Update(got.clone())).is_err() {
                                    // 主流程已放弃（超时/停止），继续读无意义
                                    break KeysListen::Done(acc);
                                }
                            }
                            if required.iter().all(|c| got.contains(c)) {
                                // 收齐即收尾（V3.6.3 核心修复）
                                break KeysListen::Collected(got);
                            }
                        }
                        Err(_) => break KeysListen::Done(acc),
                    }
                };
                let _ = sig_tx.send(sig);
            });
        } else {
            let _ = sig_tx.send(KeysListen::Done(String::new()));
        }

        // 主流程等待信号：250ms 切片响应停止请求，并遵守单项超时
        // （结构同 recv_manual）
        let deadline = exec::default_timeout().map(|d| Instant::now() + d);
        let listen = loop {
            let slice = deadline
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_millis(250))
                .min(Duration::from_millis(250));
            if slice.is_zero() {
                break KeysListen::Timeout;
            }
            match sig_rx.recv_timeout(slice) {
                // 增量点亮：转发 UI 后继续等终态（不 break）
                Ok(KeysListen::Update(got)) => {
                    let _ = tx.send(UiEvent::KeysUpdate {
                        detected: got,
                        awaiting: false,
                    });
                }
                Ok(sig) => break sig,
                Err(RecvTimeoutError::Timeout) => {
                    if exec::abort_requested() {
                        break KeysListen::Aborted;
                    }
                }
                // 读线程必先发信号再退出；Disconnected 视作空输出退出
                Err(RecvTimeoutError::Disconnected) => {
                    break KeysListen::Done(String::new());
                }
            }
        };
        // 去向无论如何都先结束本地会话（设备侧随会话断开停止监听）
        stop_loopback(&mut child);

        match listen {
            KeysListen::Collected(got) => {
                let _ = tx.send(UiEvent::KeysUpdate {
                    detected: got,
                    awaiting: false,
                });
                let _ = tx.send(UiEvent::Log("收齐全部必测按键，结束监听".to_string()));
                r.verdict = Verdict::Pass;
                r.detail = "收齐全部按键".into();
                break;
            }
            KeysListen::Aborted => {
                r.verdict = Verdict::Skipped;
                r.detail = exec::abort_reason().text().into();
                break;
            }
            // 监听等待循环只在终态 break，Update 已在循环内转发
            KeysListen::Update(_) => unreachable!("按键监听等待循环只在终态 break"),
            KeysListen::Done(out) => {
                let got = protocol::parse_keys_codes(&out);
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
                        r.detail = format!("强制判定合格（缺 {}）", missing.join("、"));
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
                    Err(WaitEnd::Timeout) => {
                        r.verdict = Verdict::ManualNo;
                        r.detail = format!("超时未应答（缺 {}）", missing.join("、"));
                        break;
                    }
                    Err(WaitEnd::Disconnected) => {
                        r.verdict = Verdict::Skipped;
                        r.detail = "界面已退出，按键判定中断".into();
                        break;
                    }
                    Err(WaitEnd::Aborted) => {
                        r.verdict = Verdict::Skipped;
                        r.detail = exec::abort_reason().text().into();
                        break;
                    }
                }
            }
            KeysListen::Timeout => {
                // 监听窗口耗尽仍没收齐：按"未收齐"处理，交给操作员
                let _ = tx.send(UiEvent::KeysUpdate {
                    detected: Vec::new(),
                    awaiting: true,
                });
                let _ = tx.send(UiEvent::Log("按键监听超时，未收齐".to_string()));
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
                        r.detail = "强制判定合格（监听超时未收齐）".into();
                        break;
                    }
                    Ok(_) => {
                        r.verdict = Verdict::ManualNo;
                        r.detail = "监听超时，未收齐按键".into();
                        break;
                    }
                    Err(WaitEnd::Timeout) => {
                        r.verdict = Verdict::ManualNo;
                        r.detail = format!("监听超时且无人应答（{manual_secs}s）");
                        break;
                    }
                    Err(WaitEnd::Disconnected) => {
                        r.verdict = Verdict::Skipped;
                        r.detail = "界面已退出，按键判定中断".into();
                        break;
                    }
                    Err(WaitEnd::Aborted) => {
                        r.verdict = Verdict::Skipped;
                        r.detail = exec::abort_reason().text().into();
                        break;
                    }
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

#[cfg(all(unix, test))] // 假 adb 桩 + ffmpeg 仅在 unix 测试构建可用
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// 端到端：设备信息 + 人工判定（阻塞应答）+ 拍照 + 录像（真 ffmpeg 解码）。
    /// 单个测试函数串行执行，避免并发测试竞争 ADB/PULL_SRC 环境变量。
    #[test]
    fn worker_sequence_flows() {
        // 与 exec 测试互斥：停止标志为全局静态，并行会互相干扰
        let _lock = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        exec::clear_abort();
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
        // 传入顺序人工项（Led）在前：执行时应自动项先行、人工殿后（V3）
        let items = vec![
            TestItem::DeviceInfo,
            TestItem::Led,
            TestItem::Photo,
            TestItem::Video,
        ];
        let media_c = media.clone();
        let reports_c = reports.clone();
        let cfg = Config::default();
        let handle = thread::spawn(move || {
            run_sequence(
                &items,
                &tx,
                &mrx,
                &media_c,
                &reports_c,
                &cfg,
                RunBase::default(),
            )
        });

        let (mut manual_asked, mut photo_ready, mut video_ready) = (0, 0, 0);
        let mut verdicts: Vec<(TestItem, Verdict)> = Vec::new();
        let mut order: Vec<TestItem> = Vec::new(); // 执行顺序（V3：自动先行）
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
                    assert!(count >= 8, "2 秒 5fps 应至少 8 帧，实际 {count}");
                    assert!(has_audio);
                }
                UiEvent::Result(r, _) => {
                    order.push(r.item);
                    verdicts.push((r.item, r.verdict));
                }
                _ => {}
            }
        }
        let report = handle.join().unwrap().expect("应产出报告路径");

        // 指示灯 + 拍照 + 录像 三次人工判定（V3.5：媒体项也人工确认）
        assert_eq!(manual_asked, 3);
        assert_eq!(photo_ready, 1);
        assert_eq!(video_ready, 1);
        // 自动项先行、人工项殿后（稳定排序保持传入相对顺序；
        // 生产入口 batch_items 走 BATCH_ORDER，见下方顺序断言）
        assert_eq!(
            order,
            vec![
                TestItem::DeviceInfo,
                TestItem::Led,
                TestItem::Photo,
                TestItem::Video
            ]
        );
        // BATCH_ORDER（V3.6 生产顺序）：自动 信息→蓝牙→WiFi→SD→电池→陀螺仪，
        // 人工 录像→拍照→指示灯→麦克风→按键
        let batch: Vec<TestItem> = BATCH_ORDER.to_vec();
        let manual: Vec<TestItem> = batch.iter().copied().filter(|i| i.is_manual()).collect();
        assert_eq!(
            manual,
            vec![
                TestItem::Video,
                TestItem::Photo,
                TestItem::Led,
                TestItem::MicSpeaker,
                TestItem::Keys
            ]
        );
        let auto: Vec<TestItem> = batch.iter().copied().filter(|i| !i.is_manual()).collect();
        assert_eq!(
            auto,
            vec![
                TestItem::DeviceInfo,
                TestItem::BtMac,
                TestItem::Wifi,
                TestItem::SdCard,
                TestItem::Battery,
                TestItem::Gyro
            ]
        );
        let get = |item: TestItem| {
            verdicts
                .iter()
                .find(|(i, _)| *i == item)
                .map(|(_, v)| *v)
                .unwrap()
        };
        assert_eq!(get(TestItem::DeviceInfo), Verdict::Pass);
        assert_eq!(get(TestItem::Led), Verdict::ManualYes);
        assert_eq!(get(TestItem::Photo), Verdict::ManualYes);
        assert_eq!(get(TestItem::Video), Verdict::ManualYes);

        let text = std::fs::read_to_string(&report).unwrap();
        assert!(text.contains("结果: PASS"));
        assert!(text.contains("sn-address:002"));
        assert!(text.contains("指示灯测试:PASS"));
        assert!(text.contains("工位: complete"));
        assert!(text.contains("汇总: 4 项，PASS 4，FAIL 0，跳过 0"));
        // SN 进文件名（V3）；SN 未知期间的旧文件应已删除
        assert!(report
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-002.txt"));
        let txts = std::fs::read_dir(&reports)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("-002"))
            .count();
        assert_eq!(txts, 1, "同一序列 SN 变化只留最终文件");

        // —— V2.1 按键引导：三场景（顺序执行，防 ADB 环境变量竞争）——
        let kscript = base.join("adb-keys");
        let keys_case = |body: &str| {
            format!(
                "#!/bin/sh\ncase \"$*\" in\n*\"camera-factory 15\"*) {body};;\n\
                 *) printf 'ok\\n';;\nesac\n"
            )
        };
        let items = |i: TestItem| vec![i];
        // 1) 输出全部键码（V3.5 固件格式 key N）：自动收齐 PASS，全程无人工介入
        let (vs, n) = scenario(
            &kscript,
            items(TestItem::Keys),
            &media,
            &reports,
            Config::default(),
            &keys_case("printf 'key 4\\nkey 5\\nkey 6\\n'"),
            || unreachable!("全键码不应触发人工应答"),
        );
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));
        // 2) 只回 key 5：未收齐 → 强制改判失败
        let (vs, n) = scenario(
            &kscript,
            items(TestItem::Keys),
            &media,
            &reports,
            Config::default(),
            &keys_case("printf 'key 5\\n'"),
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
                 echo $n > \"$KEYS_CNT\"; [ \"$n\" -ge 2 ] && printf 'key 4\\nkey 5\\nkey 6\\n' \
                 || printf 'key 5\\n'",
            ),
            || Some(UiResponse::KeysRetry),
        );
        assert_eq!((vs[0].1, n), (Verdict::Pass, 1));
        std::env::remove_var("KEYS_CNT");
        // 4) V3.6.3 收齐即判 PASS：固件收齐后不退出（真机 v2.4.2 行为，
        //    日志复现"弹窗三键全亮但流程不动"），也不能把判定押在命令返回上
        let t0 = Instant::now();
        let (vs, n) = scenario(
            &kscript,
            items(TestItem::Keys),
            &media,
            &reports,
            Config::default(),
            &keys_case("printf 'key 4\\nkey 5\\nkey 6\\n'; sleep 30"),
            || unreachable!("收齐即判，不应触发人工应答"),
        );
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));
        assert!(
            t0.elapsed() < Duration::from_secs(15),
            "收齐后应杀会话立即结束，不等固件退出，实测 {:?}",
            t0.elapsed()
        );

        // —— V3.1 按键流式回显：键码分两波输出（间隔 1s），
        //     弹窗应在监听窗口内逐波点亮，而非命令退出后一次性更新 ——
        {
            let lscript = base.join("adb-keys-live");
            std::fs::write(
                &lscript,
                keys_case("printf 'key 5\\n'; sleep 1; printf 'key 6\\nkey 4\\n'"),
            )
            .unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&lscript, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::env::set_var("ADB", &lscript);
            let (tx, rx) = mpsc::channel();
            let (_mtx, mrx) = mpsc::channel();
            let (m_c, r_c) = (media.clone(), reports.clone());
            let live_items = vec![TestItem::Keys];
            let handle = thread::spawn(move || {
                run_sequence(
                    &live_items,
                    &tx,
                    &mrx,
                    &m_c,
                    &r_c,
                    &Config::default(),
                    RunBase::default(),
                )
            });
            let (mut waves, mut verdict) = (0usize, None);
            while let Ok(ev) = rx.recv_timeout(Duration::from_secs(60)) {
                match ev {
                    UiEvent::KeysUpdate {
                        detected,
                        awaiting: false,
                        ..
                    } => {
                        if !detected.is_empty() {
                            waves += 1; // 每检测到新键一波
                        }
                    }
                    UiEvent::Result(r, _) => verdict = Some(r.verdict),
                    _ => {}
                }
            }
            handle.join().unwrap();
            assert_eq!(verdict, Some(Verdict::Pass));
            assert!(
                waves >= 2,
                "应至少两波回显（5 先到，6/4 后到），实测 {waves}"
            );
            std::env::remove_var("ADB");
        }

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

        // —— V3.5 麦克风主/副两段判定 ——
        let (vs, n) = scenario(
            &base.join("adb-mic"),
            items(TestItem::MicSpeaker),
            &media,
            &reports,
            Config::default(),
            "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 13\"*) printf 'mic main\\n';;\n\
             *\"camera-factory 14\"*) printf 'mic sub\\n';;\n\
             *) printf 'ok\\n';;\nesac\n",
            || Some(UiResponse::Manual(true)),
        );
        assert_eq!((vs[0].1, n), (Verdict::ManualYes, 0));

        // —— V3.6 麦克风回环不退出（当前固件实测：13/14 常驻不回提示符）——
        // 真机曾因此卡死：worker 阻塞等命令退出，"合格"应答无人消费。
        // 现在：启动即转人工等待，应答即杀本地会话，全程不等 sleep 30。
        {
            let mscript = base.join("adb-mic-live");
            std::fs::write(
                &mscript,
                "#!/bin/sh\ncase \"$*\" in\n\
                 *\"camera-factory 13\"*) printf 'Loopback m on\\n'; sleep 30;;\n\
                 *\"camera-factory 14\"*) printf 'Loopback b on\\n'; sleep 30;;\n\
                 *) printf 'ok\\n';;\nesac\n",
            )
            .unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&mscript, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::env::set_var("ADB", &mscript);
            let (tx, rx) = mpsc::channel();
            let (mtx, mrx) = mpsc::channel();
            let (m_c, r_c) = (media.clone(), reports.clone());
            let mic_items = vec![TestItem::MicSpeaker];
            let t0 = Instant::now();
            let handle = thread::spawn(move || {
                run_sequence(
                    &mic_items,
                    &tx,
                    &mrx,
                    &m_c,
                    &r_c,
                    &Config::default(),
                    RunBase::default(),
                )
            });
            let (mut asked, mut verdict) = (0usize, None);
            while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
                match ev {
                    UiEvent::Manual { .. } => {
                        asked += 1;
                        mtx.send(UiResponse::Manual(true)).unwrap();
                    }
                    UiEvent::Result(r, _) => verdict = Some(r.verdict),
                    _ => {}
                }
            }
            handle.join().unwrap();
            assert_eq!(asked, 2, "主麦/副麦两段判定都应弹出");
            assert_eq!(verdict, Some(Verdict::ManualYes));
            assert!(
                t0.elapsed() < Duration::from_secs(15),
                "回环命令挂住不应阻塞流程，实测 {:?}",
                t0.elapsed()
            );
        }

        // —— V2.2 陀螺仪（V3.5 改 camera-factory 17，返回值判定）——
        let gscript = base.join("adb-gyro");
        let gitems = items(TestItem::Gyro);
        // 返回值无 FAIL → PASS
        let (vs, _) = scenario(
            &gscript,
            gitems.clone(),
            &media,
            &reports,
            Config::default(),
            "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 17\"*) printf 'gyroscope-test:true\\n';;\n\
             *) printf 'ok\\n';;\nesac\n",
            || unreachable!(),
        );
        assert_eq!(vs[0].1, Verdict::Pass);
        // 返回值含 FAIL → FAIL
        let (vs, _) = scenario(
            &gscript,
            gitems,
            &media,
            &reports,
            Config::default(),
            "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 17\"*) printf 'gyroscope-test:false\\n';;\n\
             *) printf 'ok\\n';;\nesac\n",
            || unreachable!(),
        );
        assert_eq!(vs[0].1, Verdict::Fail);

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
            &keys_case("printf 'key 5\\n'"),
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

        // —— V3 手动停止：慢速命令执行中途请求停止 ——
        exec::clear_abort();
        let sscript = base.join("adb-stop");
        std::fs::write(
            &sscript,
            "#!/bin/sh\ncase \"$*\" in\n*\"camera-factory 5\"*) sleep 30; printf 'wifi ok\\n';;\n\
             *\"camera-factory 8\"*) sleep 30;;\n\
             *) printf 'ok\\n';;\nesac\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sscript, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", &sscript);
        let stop_items = vec![TestItem::Wifi, TestItem::SdCard];
        let (tx, rx) = mpsc::channel();
        let (_mtx, mrx) = mpsc::channel();
        let (m_c, r_c) = (media.clone(), reports.clone());
        let t0 = Instant::now();
        let handle = thread::spawn(move || {
            run_sequence(
                &stop_items,
                &tx,
                &mrx,
                &m_c,
                &r_c,
                &Config::default(),
                RunBase::default(),
            )
        });
        let mut stop_verdicts = Vec::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            if matches!(ev, UiEvent::Current(_)) {
                exec::request_abort(exec::StopReason::User); // 收到第一项 Current 即模拟操作员按停止
            }
            if let UiEvent::Result(r, _) = ev {
                stop_verdicts.push((r.item, r.verdict));
            }
        }
        let stop_report = handle.join().unwrap().expect("停止后仍应产出报告");
        assert!(
            t0.elapsed() < Duration::from_secs(15),
            "停止应立即生效，不应等满 30s 命令"
        );
        // 当前项命令被强杀（FAIL → 跳过），剩余项记跳过
        assert_eq!(
            stop_verdicts,
            vec![
                (TestItem::Wifi, Verdict::Skipped),
                (TestItem::SdCard, Verdict::Skipped)
            ]
        );
        let text = std::fs::read_to_string(&stop_report).unwrap();
        assert!(text.contains("手动结束测试"));
        assert!(text.contains("结果: 跳过"));
        exec::clear_abort();

        // —— V3.8 设备拔出中止：同一停止路径，文案按原因 ——
        exec::clear_abort();
        std::env::set_var("ADB", &sscript); // 复用上面的慢速桩
        let gone_items = vec![TestItem::Wifi, TestItem::SdCard];
        let (tx, rx) = mpsc::channel();
        let (_mtx, mrx) = mpsc::channel();
        let (m_g, r_g) = (media.clone(), reports.clone());
        let handle = thread::spawn(move || {
            run_sequence(
                &gone_items,
                &tx,
                &mrx,
                &m_g,
                &r_g,
                &Config::default(),
                RunBase::default(),
            )
        });
        let mut gone_verdicts = Vec::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            if matches!(ev, UiEvent::Current(_)) {
                // 模拟监测线程发现设备拔出
                exec::request_abort(exec::StopReason::DeviceGone);
            }
            if let UiEvent::Result(r, _) = ev {
                gone_verdicts.push((r.item, r.verdict));
            }
        }
        let gone_report = handle.join().unwrap().expect("拔出后仍应产出报告");
        assert_eq!(
            gone_verdicts,
            vec![
                (TestItem::Wifi, Verdict::Skipped),
                (TestItem::SdCard, Verdict::Skipped)
            ]
        );
        let text = std::fs::read_to_string(&gone_report).unwrap();
        assert!(text.contains("设备已拔出"));
        assert!(text.contains("结果: 跳过"));
        exec::clear_abort();

        // —— V3.6 按键停止：监听阶段（adb 阻塞中）与等待应答阶段 ——
        // 1) 监听中停止：固件监听窗口阻塞 30s，中途按停止应立即退出
        exec::clear_abort();
        let kstop = base.join("adb-keys-stop");
        std::fs::write(
            &kstop,
            "#!/bin/sh\ncase \"$*\" in\n*\"camera-factory 15\"*) printf 'key 4\\n'; sleep 30;;\n\
             *) printf 'ok\\n';;\nesac\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&kstop, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", &kstop);
        let (tx, rx) = mpsc::channel();
        let (_mtx, mrx) = mpsc::channel();
        let (m_c, r_c) = (media.clone(), reports.clone());
        let t0 = Instant::now();
        let ks_items = vec![TestItem::Keys];
        let handle = thread::spawn(move || {
            run_sequence(
                &ks_items,
                &tx,
                &mrx,
                &m_c,
                &r_c,
                &Config::default(),
                RunBase::default(),
            )
        });
        let mut ks_verdicts = Vec::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            if matches!(
                ev,
                UiEvent::KeysUpdate {
                    awaiting: false,
                    ..
                }
            ) {
                exec::request_abort(exec::StopReason::User); // 弹窗已开、监听进行中：模拟按停止
            }
            if let UiEvent::Result(r, _) = ev {
                ks_verdicts.push((r.item, r.verdict));
            }
        }
        handle.join().unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(15),
            "监听中停止应立即生效，实测 {:?}",
            t0.elapsed()
        );
        assert_eq!(ks_verdicts, vec![(TestItem::Keys, Verdict::Skipped)]);
        exec::clear_abort();

        // 2) 等待应答阶段停止：未收齐弹窗悬着，操作员不点改判而直接按停止
        std::fs::write(
            &kstop,
            "#!/bin/sh\ncase \"$*\" in\n*\"camera-factory 15\"*) printf 'key 4\\n';;\n\
             *) printf 'ok\\n';;\nesac\n",
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        let (_mtx, mrx) = mpsc::channel();
        let (m_c, r_c) = (media.clone(), reports.clone());
        let t0 = Instant::now();
        let ks_items = vec![TestItem::Keys];
        let handle = thread::spawn(move || {
            run_sequence(
                &ks_items,
                &tx,
                &mrx,
                &m_c,
                &r_c,
                &Config::default(),
                RunBase::default(),
            )
        });
        let mut ks_verdicts = Vec::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            if matches!(ev, UiEvent::KeysUpdate { awaiting: true, .. }) {
                exec::request_abort(exec::StopReason::User); // 不应答，直接停止
            }
            if let UiEvent::Result(r, _) = ev {
                ks_verdicts.push((r.item, r.verdict));
            }
        }
        handle.join().unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(15),
            "等待应答时停止应立即生效，实测 {:?}",
            t0.elapsed()
        );
        assert_eq!(ks_verdicts, vec![(TestItem::Keys, Verdict::Skipped)]);
        exec::clear_abort();

        std::env::remove_var("ADB");
        std::env::remove_var("PULL_SRC");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 场景驱动：写入完整假 adb 脚本后跑 `items` 序列；
    /// 收到人工应答请求（Manual / 按键 awaiting）时调 `respond`
    /// （返回 None = 不应答，模拟无人操作）。返回 (逐项判定, awaiting 次数)。
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
        let handle = thread::spawn(move || {
            run_sequence(&items, &tx, &mrx, &m, &r, &cfg, RunBase::default())
        });

        let (mut verdicts, mut awaiting) = (Vec::new(), 0usize);
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(60)) {
            match ev {
                UiEvent::KeysUpdate { awaiting: true, .. } => {
                    awaiting += 1;
                    if let Some(resp) = respond() {
                        mtx.send(resp).unwrap();
                    }
                }
                UiEvent::Manual { .. } => {
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

    #[test]
    fn single_item_retest_merges_into_unit_report() {
        let _g = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::exec::clear_abort();
        let base_dir = std::env::temp_dir().join(format!("e02-merge-{}", std::process::id()));
        let media = base_dir.join("media");
        let reports = base_dir.join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        let script = base_dir.join("adb-merge");
        std::fs::write(&script, "#!/bin/sh\nprintf 'wifi ok\n'\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", &script);
        // 整机底稿：设备信息合格、WiFi 不合格，SN 已知（上一轮全测产物）
        let mut r_info = TestResult::new(TestItem::DeviceInfo);
        r_info.verdict = Verdict::Pass;
        let mut r_wifi = TestResult::new(TestItem::Wifi);
        r_wifi.verdict = Verdict::Fail;
        let base = RunBase {
            device: DeviceInfo {
                sn: "SNM".into(),
                ..Default::default()
            },
            results: vec![r_info, r_wifi],
        };
        // 单项补测 WiFi（假桩输出 ok → 合格），应替换底稿槽位并重写整机报告
        let items = vec![TestItem::Wifi];
        let (tx, rx) = mpsc::channel();
        let (_mtx, mrx) = mpsc::channel();
        let (m, r) = (media.clone(), reports.clone());
        let handle = thread::spawn(move || {
            run_sequence(&items, &tx, &mrx, &m, &r, &Config::default(), base)
        });
        let mut verdicts = Vec::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            if let UiEvent::Result(res, _) = ev {
                verdicts.push((res.item, res.verdict));
            }
        }
        let report = handle.join().unwrap().expect("补测应产出报告");
        assert_eq!(verdicts, vec![(TestItem::Wifi, Verdict::Pass)]);
        // 同 SN 同日文件：整机报告被重写，不是另散一份单项报告
        assert!(report.to_string_lossy().contains("SNM"), "{report:?}");
        let text = std::fs::read_to_string(&report).unwrap();
        assert!(text.contains("sn-address:SNM"), "{text}");
        assert!(text.contains("设备信息:PASS"), "{text}"); // 底稿项保留
        assert!(text.contains("WiFi 测试:PASS"), "{text}"); // 补测项替换
        assert!(!text.contains("WiFi 测试:FAIL"), "{text}");
        assert!(text.contains("汇总: 2 项，PASS 2"), "{text}");
        assert!(text.contains("结果: PASS"), "{text}"); // 补测通过整机翻 PASS
        std::env::remove_var("ADB");
        crate::exec::clear_abort();
        let _ = std::fs::remove_dir_all(&base_dir);
    }
}
