//! 测试序列 worker：在独立线程阻塞执行 adb/ffmpeg 流程，
//! 经 [`UiEvent`] 回传状态；人工判定阻塞等待 [`UiResponse`]。

use crate::adb;
use crate::config::{Config, IperfDir, KeysCfg, ModelKind, ThroughputCfg, WifiCfg};
use crate::engine::events::{UiEvent, UiResponse};
use crate::engine::runner::{self, RunOutcome};
use crate::engine::{TestItem, TestResult, Verdict};
use crate::exec;
use crate::ledger::{self, BindCheck, Ledger};
use crate::netip;
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
pub const BATCH_ORDER: [TestItem; 13] = [
    TestItem::DeviceInfo,
    TestItem::BtMac,
    // 虚拟项（紧随蓝牙 MAC：地址两半齐备即校验）
    TestItem::Binding,
    TestItem::Wifi,
    TestItem::SdCard,
    TestItem::Battery,
    TestItem::Gyro,
    TestItem::Esim,
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
    /// 当前连接的这台设备已校时过（true 时跳过前置校时，避免单点测试重复下发）
    pub skip_time_sync: bool,
}

/// 返回报告文件路径；写入失败为 None。
/// `serial`（V3.15）= 开始时在线的被测设备序列号：序列期间所有 adb
/// 命令定向到它，中途插入其他设备不打断测试（None = 不定向）。
// 参数是执行一条序列的全部上下文，聚成结构体反而不易读，豁免条数限制
#[allow(clippy::too_many_arguments)]
pub fn run_sequence(
    items: &[TestItem],
    tx: &Sender<UiEvent>,
    manual: &Receiver<UiResponse>,
    media_dir: &Path,
    reports_dir: &Path,
    cfg: &Config,
    base: RunBase,
    serial: Option<String>,
) -> Option<PathBuf> {
    exec::clear_abort(); // 新序列不残留上一轮的停止请求
    let _target = crate::adb::TargetGuard::new(serial);
    let _ = std::fs::create_dir_all(media_dir);
    let _ = std::fs::create_dir_all(reports_dir);
    let _ = tx.send(UiEvent::Started);
    let (started_wall, started) = (SystemTime::now(), Instant::now());
    // 单项超时（V2.3）：序列期间子进程统一看门狗，防止固件命令阻塞卡死序列
    let prev_timeout = exec::default_timeout();
    exec::set_default_timeout(timeout_opt(cfg.timeouts.item_secs));
    // 序列前置（V3.5 #8）：宿主机时间写入设备再执行固件校时指令；
    // 同一接入周期内的同一台设备仅校时一次，单点补测不再重复下发
    if !base.skip_time_sync {
        sync_time(tx);
    }

    // 自动项先行、人工项殿后（稳定排序保持组内原顺序）
    let mut ordered: Vec<TestItem> = items.to_vec();
    ordered.sort_by_key(|i| i.is_manual());
    let first_manual = ordered.iter().position(|i| i.is_manual());

    let mut device = base.device;
    let mut results = base.results;
    // 底稿按批序归位，报告行序稳定
    // 绑定记录（V3.13）：蓝牙与绑定对另一半齐备后校验一次（含单项补测——
    // 底稿 device 已带两地址时开测即校验）。冲突判 FAIL 但不中止：剩余项
    // 测完留完整报告（产线返修定位需要），冲突机不动记录、报告文件名加
    // "-冲突"后缀避免覆盖原机报告。记录每次变更前从磁盘重读：设置窗可
    // 在测试中开合，避免收尾回填覆盖中途的删/清操作。
    // 型号（V3.14；V3.14.1 改手动锁定）：设置窗选定为准，不随固件名
    // 自动切换；读到与所选不符的固件名只提醒核对（E02E 固件名规律
    // 未证实，不作判定依据）
    results.sort_by_key(|r| batch_pos(r.item));
    let model = current_model(cfg);
    let mut model_warned = false;
    let ledger_path = ledger::path_of(cfg);
    let mut binding_done = false;
    maybe_bind(
        cfg.binding.enable,
        &mut binding_done,
        &ledger_path,
        &device,
        model,
        &cfg.station,
        &mut results,
        tx,
    );
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
            // 240 容得下 WiFi 全文（IP+AP+双向速率+重传 ~190 字符，V3.15.2：
            // 曾截 100 导致"打印不全"；原始命令输出在 detail 拼装时另有窄截）
            clip(&result.detail, 240)
        )));
        if let Some(d) = &dev {
            device.merge(d.clone());
            // 固件名不符提醒（只提醒一次，不影响判定与流程）
            if !model_warned {
                if let Some(detected) = ModelKind::detect(&device.t32_ver) {
                    if detected != model {
                        model_warned = true;
                        let _ = tx.send(UiEvent::Log(format!(
                            "提示：固件名 {} 疑似 {} 型，与所选型号 {} 不符，请核对设置",
                            clip(&device.t32_ver, 40),
                            detected.label(),
                            model.label()
                        )));
                    }
                }
            }
            maybe_bind(
                cfg.binding.enable,
                &mut binding_done,
                &ledger_path,
                &device,
                model,
                &cfg.station,
                &mut results,
                tx,
            );
        }
        let _ = tx.send(UiEvent::Result(result.clone(), dev));
        upsert(&mut results, result);
        // 报告边测边写：中途卡死/退出也留有报告；文件名同机同日固定，
        // 重测覆盖更新，SN 中途解析到后改名并删旧文件（V3.2）
        let _ = write_update(
            reports_dir,
            &meta(cfg, model, started_wall, started),
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
    // 测后回填整机判定（台账"最近判定"列；bind 入过账的机器才会命中）。
    // FQC §3 ① 闸门防绕过：仅当整机工位（complete）且当前型号整机全部
    // 启用项均已执行且全部 PASS（排除单项补测与中途停止），才置整机 PASS。
    if cfg.binding.enable && !device.bt_mac.is_empty() {
        let verdict = report::overall(&results, true);
        let complete_all_pass = is_complete_full_pass(cfg, model, &results, verdict);
        let mut ledger = Ledger::load(&ledger_path);
        ledger.set_verdict(
            &device.bt_mac,
            verdict,
            &cfg.station,
            complete_all_pass,
            &report::time_str(SystemTime::now()),
        );
        if let Err(e) = ledger.save(&ledger_path) {
            let _ = tx.send(UiEvent::Log(format!("台账保存失败: {e}")));
        }
    }
    write_update(
        reports_dir,
        &meta(cfg, model, started_wall, started),
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

/// 判断本次测试结果是否满足"整机工位全测 PASS"（FQC 闸门严口径）：
/// 当前工位为 `complete`、总判定为 `PASS`、且该型号在整机档案下的全部
/// 启用项（含必测 info/btmac）均已测且为 PASS（防单项补测或中途停止绕过）。
fn is_complete_full_pass(
    cfg: &Config,
    model: ModelKind,
    results: &[TestResult],
    verdict: &str,
) -> bool {
    if cfg.station != "complete" || verdict != "PASS" {
        return false;
    }
    let required: Vec<TestItem> = cfg
        .active_items()
        .into_iter()
        .filter(|i| i.available_for(model))
        .collect();
    !required.is_empty()
        && required.iter().all(|req| {
            results
                .iter()
                .any(|r| r.item == *req && r.verdict.is_pass())
        })
}

/// 绑定校验执行一次（V3.13；V3.14 型号化绑定对）：蓝牙 MAC 与绑定对
/// 另一半（E02=WiFi MAC，E02E=IMEI）都齐备且本轮未校验过。
/// 结果行进序列/报告；台账即时落盘（应用随时可能被关），变更前重读
/// 磁盘（设置窗可能在测试中改台账）。
#[allow(clippy::too_many_arguments)]
fn maybe_bind(
    enable: bool,
    done: &mut bool,
    ledger_path: &Path,
    device: &DeviceInfo,
    model: ModelKind,
    cfg_station: &str,
    results: &mut Vec<TestResult>,
    tx: &Sender<UiEvent>,
) {
    let partner_kind = if model.has_wifi() { "WiFi" } else { "IMEI" };
    let partner = if model.has_wifi() {
        device.wifi_mac.as_str()
    } else {
        device.imei.as_str()
    };
    if *done || !enable || device.bt_mac.is_empty() || partner.is_empty() {
        return;
    }
    *done = true;
    let now = report::time_str(SystemTime::now());
    let mut ledger = Ledger::load(ledger_path);
    let mut r = TestResult::new(TestItem::Binding);
    match ledger.bind(
        &device.bt_mac,
        partner,
        model.label(),
        &device.sn,
        cfg_station,
        &now,
    ) {
        BindCheck::New => {
            r.verdict = Verdict::Pass;
            r.detail = "新设备入账".into();
        }
        BindCheck::Retest { tests } => {
            r.verdict = Verdict::Pass;
            r.detail = format!("同一设备第 {tests} 次测试");
        }
        BindCheck::BtConflict { other } => {
            r.verdict = Verdict::Fail;
            r.detail = format!(
                "该蓝牙地址已绑定{} {}（首见 {}，已测 {} 次），不同设备烧了同一蓝牙地址",
                partner_kind, other.partner, other.first_seen, other.tests
            );
        }
        BindCheck::PartnerConflict { other } => {
            r.verdict = Verdict::Fail;
            r.detail = format!(
                "该{}已绑定蓝牙 {}（首见 {}，已测 {} 次），不同设备烧了同一地址",
                partner_kind, other.bt_mac, other.first_seen, other.tests
            );
        }
    }
    if let Err(e) = ledger.save(ledger_path) {
        let _ = tx.send(UiEvent::Log(format!("绑定记录保存失败: {e}")));
    }
    let _ = tx.send(UiEvent::Log(format!(
        "【{}】{} {}",
        r.item.label(),
        r.verdict.label(),
        clip(&r.detail, 240) // 与结果行同宽（V3.15.2）：冲突详情不再截半句
    )));
    let _ = tx.send(UiEvent::Result(r.clone(), None));
    upsert(results, r);
}

/// 当前型号（V3.14；V3.14.1 改手动锁定）：设置窗选定，缺省 E02。
fn current_model(cfg: &Config) -> ModelKind {
    cfg.model.unwrap_or_default()
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

fn meta(
    cfg: &Config,
    model: ModelKind,
    started_wall: SystemTime,
    started: Instant,
) -> report::ReportMeta {
    report::ReportMeta {
        station: cfg.station.clone(),
        model,
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
        TestItem::Wifi => {
            let mut wifi = cfg.wifi.clone();
            wifi.depth = cfg.active_wifi_depth();
            wifi_flow(tx, &wifi)
        }
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
    let mut ok = true;
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
                ok = false;
                let _ = tx.send(UiEvent::Log(format!("{what}失败（继续测试）: {e:#}")));
            }
        }
    }
    if ok && !exec::abort_requested() {
        let _ = tx.send(UiEvent::TimeSynced);
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
    send_manual(tx, "拍照测试", "拍照图像是否清晰、色彩正常？", true);
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
    send_manual(
        tx,
        "录像测试",
        "录像画面是否清晰？有无条纹？声音是否正常？",
        true,
    );
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
fn send_manual(tx: &Sender<UiEvent>, title: &str, question: &str, in_media: bool) {
    let _ = tx.send(UiEvent::Manual {
        title: title.to_string(),
        question: question.to_string(),
        in_media,
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
    send_manual(tx, item.label(), question, false);
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
        send_manual(tx, &format!("{name}判定"), question, false);
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

/// 结束回环（V3.15.3：先礼后兵）。13/14/15 固件端是 `ret=getchar();`
/// ——等任意输入才退出；此前直接杀本地 adb 会话，设备侧回环/监听进程
/// 收不到字符、一直挂着。改为先往 stdin 写一个空符号（换行）让固件
/// 自己走完退出，最多等 2s；超时（固件没在等输入/管道异常）兜底强杀。
fn stop_loopback(child: &mut Option<std::process::Child>) {
    if let Some(mut c) = child.take() {
        if let Some(mut stdin) = c.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(b"\n");
            let _ = stdin.flush();
            drop(stdin);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match c.try_wait() {
                Ok(Some(_)) => return, // 固件收到字符，自然退出
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
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

/// 单方向 iperf3 测试结果（判定与拼 detail 用）。
struct DirOutcome {
    label: &'static str, // 上行 / 下行（UI 内嵌 CJK 字体无箭头字形，禁用符号）
    mbps: f32,           // 窗口中位数
    need: f32,           // 阈值（0=不判）
    retr: Option<u32>,
    retr_max: u32, // 0=不判
    retested: bool,
}

impl DirOutcome {
    fn pass(&self) -> bool {
        let thr_ok = self.need <= 0.0 || self.mbps >= self.need;
        let retr_ok = self.retr_max == 0 || self.retr.is_none_or(|v| v <= self.retr_max);
        thr_ok && retr_ok
    }
    fn detail(&self) -> String {
        // 文案自解释（真机反馈"上限500什么意思/是不是每秒"）：
        // Mbps 本就是每秒兆比特，仍显式标注"每秒速率中位数"；
        // 重传计"次"、上限标"不超过"。（内嵌 CJK 字体无 ≥≤ 字形，
        // 只能用 ASCII+CJK+全角标点）
        let mut s = format!("{}{:.1}Mbps", self.label, self.mbps);
        if self.need > 0.0 {
            s.push_str(&format!(
                "(每秒速率中位数，阈值{:.0}，低于判不合格)",
                self.need
            ));
        } else if self.retr.is_some() {
            s.push_str("(每秒速率中位数)");
        }
        if let Some(v) = self.retr {
            if self.retr_max > 0 {
                s.push_str(&format!(" 重传{v}次(不超过{}合格)", self.retr_max));
            } else {
                s.push_str(&format!(" 重传{v}次"));
            }
        }
        if self.retested {
            s.push_str("[复测]");
        }
        s
    }
}

/// 起一次服务端（-1 单测自退）+ 设备端客户端，返回结果摘要。
///
/// 服务端在 PC 线程里跑（exec::output 带超时与全局停止），反向测试的
/// 重传数发生在 PC 发送端，从服务端 JSON 补齐。端口每次独立取空闲
/// （server_port=0 时）：真机 18:44 案例里上行无线瞬断卡死、服务端
/// 20s 超时被杀后端口残留，下行复用同端口 bind 失败被秒拒——
/// 每方向各拿各的端口后这类残留互不传染。
fn iperf_once(
    tx: &Sender<UiEvent>,
    host: &str,
    t: &ThroughputCfg,
    local: std::net::Ipv4Addr,
    reverse: bool,
) -> Result<protocol::IperfSummary, String> {
    let label = if reverse { "下行" } else { "上行" };
    let port = if t.server_port == 0 {
        free_port().unwrap_or(19703)
    } else {
        t.server_port
    };
    let _ = tx.send(UiEvent::Log(format!(
        "iperf3 {label}：{local}:{port} {}s",
        t.duration_secs
    )));

    let server_timeout = Duration::from_secs(t.duration_secs as u64 + 15);
    let server = {
        let mut cmd = std::process::Command::new(host);
        cmd.args([
            "-s",
            "-1",
            "-B",
            &local.to_string(),
            "-p",
            &port.to_string(),
            "--json",
        ]);
        std::thread::spawn(move || exec::output(&mut cmd, Some(server_timeout)).ok())
    };

    let mut client = format!("-c {local} -p {port} -t {} -J", t.duration_secs);
    if reverse {
        client.push_str(" -R");
    }
    // 客户端专用超时 = 时长 + 缓冲：不再走 60s 单项超时白等
    // （瞬断/被拦时 adb 挂满一分钟才被杀，整项直接超时）
    let client_secs = t.duration_secs as u64 + 12;
    // 固件把 iperf3 放在系统目录（PATH 可达；固件侧无 which 指令），裸名执行
    let out = adb::shell_timeout(
        &format!("iperf3 {client}"),
        Duration::from_secs(client_secs),
    )
    .map_err(|e| {
        let es = format!("{e:#}");
        if es.contains("超时") {
            format!("命令 {client_secs}s 未返回（疑无线瞬断或 PC 防火墙拦 {local}:{port} 入站）")
        } else {
            format!("adb: {es}")
        }
    })?;
    let srv = server.join().ok().flatten();

    let mut sum = protocol::parse_iperf3_json(&out).ok_or_else(|| {
        if out.contains("not found") {
            "固件未集成 iperf3（PATH 直接执行失败）".to_string()
        } else if out.contains("connected\":[]") || out.contains("unable to connect") {
            format!(
                "设备连不上 PC 服务器 {local}:{port}（疑端口未监听/防火墙）: {}",
                clip(&runner::one_line(&out), 60)
            )
        } else {
            format!("iperf3 输出无法解析: {}", clip(&runner::one_line(&out), 80))
        }
    })?;
    if reverse && sum.retransmits.is_none() {
        // 反向时发送端在 PC：客户端 JSON 无重传字段，从服务端补
        sum.retransmits = srv
            .as_ref()
            .and_then(|o| protocol::parse_iperf3_server_retr(&exec::decode_out(&o.stdout)));
    }
    Ok(sum)
}

/// 推送产线 WiFi 配置并触发重连（V3.12.3）：本地按设备原格式生成
/// wpa_supplicant.conf（密码空 = key_mgmt NONE 开放网络）→ adb push 到
/// /config/IConfig/ → camera-factory 5 走固件既有逻辑按新配置重连。
/// 返回 5 的输出（关联 + DHCP 成功时固件回读打印 wlan0-ip）。
fn join_wifi(ssid: &str, psk: &str) -> Result<String, String> {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let network = if psk.is_empty() {
        format!(
            "network={{\n        ssid=\"{}\"\n        key_mgmt=NONE\n}}",
            esc(ssid)
        )
    } else {
        format!(
            "network={{\n        ssid=\"{}\"\n        psk=\"{}\"\n        key_mgmt=WPA-PSK\n}}",
            esc(ssid),
            esc(psk)
        )
    };
    let conf = format!(
        "ctrl_interface=/var/run/wpa_supplicant\nap_scan=1\nupdate_config=1\n\n{network}\n"
    );
    let local = std::env::temp_dir().join(format!("e02-wpa-{}.conf", std::process::id()));
    std::fs::write(&local, conf).map_err(|e| format!("写临时配置失败: {e}"))?;
    let pushed = adb::push(&local, "/config/IConfig/wpa_supplicant.conf");
    let _ = std::fs::remove_file(&local); // 推完即删，不留本机痕迹
    pushed.map_err(|e| format!("adb push 配置失败: {e:#}"))?;
    let out = adb::shell("camera-factory 5").map_err(|e| format!("camera-factory 5: {e:#}"))?;
    // FAIL-BUSY 是 wpa_ctrl 瞬态噪声（真机证明非致命），剔除后才判
    if out.replace("FAIL-BUSY", "").contains("FAIL") {
        return Err(format!(
            "固件重连失败: {}",
            clip(&runner::one_line(&out), 60)
        ));
    }
    Ok(out)
}

/// 读设备当前关联的 SSID（V3.12.13）：wpa_cli 优先、iwconfig 兜底；
/// MIPS 固件两者都可能没有，读不到返回 None（命令缺失时尾部 `; true`
/// 兜住退出码，adb::shell 才不会当失败丢输出）。
fn assoc_ssid() -> Option<String> {
    adb::shell("wpa_cli -i wlan0 status 2>/dev/null; iwconfig wlan0 2>/dev/null; true")
        .ok()
        .and_then(|o| protocol::parse_assoc_ssid(&o))
}

/// WiFi 流程（深度可配）：
/// - driver：只跑 camera-factory 5 驱动检查；
/// - throughput：驱动过后跑 iperf3 上/下行吞吐与 TCP 重传（V3.12；
///   无屏蔽箱共享 AP 环境：每秒窗口中位数判阈值 + 边缘复测兜底）
fn wifi_flow(tx: &Sender<UiEvent>, wifi: &WifiCfg) -> RunOutcome {
    // 先跑驱动检查（camera-factory 5），失败直接返回
    let base = runner::run_auto(TestItem::Wifi);
    if base.result.verdict != Verdict::Pass || wifi.depth == crate::config::WifiDepth::Driver {
        return base;
    }

    let mut r = base.result;
    let t = &wifi.throughput;

    // 设备 IP：join + DHCP 成功即连通性证明（ping 已退役为失败归因用）。
    // MIPS/uClibc 固件没有 Android 的 cmd wifi——配了产线 SSID 就推送
    // wpa_supplicant.conf 再跑 camera-factory 5 走固件既有连接逻辑重连
    // （原 camera-factory 20 直连指令方案已弃，固件侧不动）
    let _ = tx.send(UiEvent::Log("WiFi 吞吐：读取 wlan0 地址...".to_string()));
    // ip/ifconfig 双探测：busybox 未必编入 ip applet（真机 2026-10-28
    // 轮询 10 次全空但固件实际已拿到 DHCP 租约），ifconfig 基本必在
    const PROBE: &str = "ip -4 addr show wlan0 2>/dev/null; ifconfig wlan0 2>/dev/null";
    let probe_ip = || {
        adb::shell(PROBE)
            .ok()
            .and_then(|o| protocol::parse_wlan_ip_any(&o))
    };
    let mut ip = probe_ip();
    // 重连前的旧地址：5 撞 wpa BUSY 提前返回时固件回读打印的是重连前
    // 旧态（真机 2026-10-30 17:01：回读出 HEC 旧址 192.168.1.27 被采信，
    // 拿去对 PC 网段误报"PC 无同网段"）——强制重连后首轮只接受**变化后**
    // 的地址；等满一轮再重跑 5 的回读才视为新态（届时固件重连已落地）
    let old_ip = ip.clone();
    let mut last5 = String::new(); // 最近一次 camera-factory 5 输出（失败归因用）
                                   // V3.12.12：配了产线 SSID 就每次吞吐前强制推送重连，不再只在未入网
                                   // 时才接管——设备可能还挂在别的 AP 上（真机 2026-10-30：HEC 同 SSID
                                   // 三台 AP 跨信道，测试中漫游正是下行首跑卡死 42s 的来源）；留空才
                                   // 维持"用当前网络直接测"。V3.12.13：能读到关联 SSID 且已是配置的
                                   // AP 时跳过重连（产线常态省 ~10s），重连完再验证真的换了网
    let mut rejoined = false;
    // 关联 SSID 恒探测（不只配了 SSID 才探）：换网判定要用，结果行
    // 也要带"连的哪台 AP"（真机反馈报告看不出连的哪个 AP）
    let mut assoc_seen = assoc_ssid();
    let _ = tx.send(UiEvent::Log(match &assoc_seen {
        Some(s) => format!("耳机当前关联 SSID: {s}"),
        None => "关联 SSID 未读到（wpa_cli/iwconfig 不可用）".to_string(),
    }));
    if !wifi.ssid.is_empty() {
        if assoc_seen.as_deref() == Some(wifi.ssid.as_str()) && ip.is_some() {
            let _ = tx.send(UiEvent::Log(format!(
                "已连在配置的 {} 上，跳过重连",
                wifi.ssid
            )));
        } else {
            rejoined = true;
            let _ = tx.send(UiEvent::Log(match &old_ip {
                Some(old) => {
                    format!("已入网({old})，推送 {} 配置并重连...", wifi.ssid)
                }
                None => format!("wlan0 未入网，推送产线 SSID {} 并重连...", wifi.ssid),
            }));
            match join_wifi(&wifi.ssid, &wifi.psk) {
                // 关联 + DHCP 成功时固件自己回读打印 wlan0-ip——但强制
                // 重连场景首轮回读可能仍是旧态（见 old_ip 注释）：与旧
                // 地址相同先不采信，置空交给下面的轮询/重跑 5 分辨
                Ok(out) => {
                    last5 = out.clone();
                    let _ = tx.send(UiEvent::Log(format!(
                        "camera-factory 5 返回: {}",
                        clip(&runner::one_line(&out), 70)
                    )));
                    ip = match protocol::parse_fw_wlan_ip(&out) {
                        Some(v) if old_ip.as_deref() == Some(v.as_str()) => {
                            // 同路由器双频/同网段（如 HEC → HEC 5G）DHCP 常分回原 IP：
                            // 顺手核对关联 SSID，已切到目标热点则直接采信，免白等 20s
                            if assoc_ssid().as_deref() == Some(wifi.ssid.as_str()) {
                                let _ = tx.send(UiEvent::Log(format!(
                                    "已切到 {}，地址保持 {v}（同网段正常）",
                                    wifi.ssid
                                )));
                                Some(v)
                            } else {
                                let _ = tx.send(UiEvent::Log(format!(
                                    "回读 {v} 与重连前相同，先不采信（等地址变化或 SSID 切换）"
                                )));
                                None
                            }
                        }
                        rb => rb,
                    };
                }
                Err(e) => {
                    r.verdict = Verdict::Fail;
                    r.detail = format!("连接 {} 失败: {e}", wifi.ssid);
                    return RunOutcome {
                        result: r,
                        device: base.device,
                    };
                }
            }
            // 等 DHCP；一轮没拿到就重跑一次 5 再等一轮（等价产线手动
            // 重试，真机验证有效）。重跑后的回读视为新态：等满 20s 固件
            // 重连已实际完成，此时同址 = 同网重连同址，正常
            for round in 0..2 {
                for i in 0..10 {
                    if ip.is_some() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    let p = probe_ip();
                    let switched_same_ip = p.is_some()
                        && p == old_ip
                        && assoc_ssid().as_deref() == Some(wifi.ssid.as_str());
                    let _ = tx.send(UiEvent::Log(match (&p, &old_ip) {
                        (Some(p), Some(old)) if p == old && switched_same_ip => {
                            format!("等地址({}/10): {p}（已切到 {}）", i + 1, wifi.ssid)
                        }
                        (Some(p), Some(old)) if p == old => {
                            format!("等地址({}/10): {p}（未变化，继续等）", i + 1)
                        }
                        (Some(p), _) => format!("等地址({}/10): {p}", i + 1),
                        (None, _) => format!("等地址({}/10): 无", i + 1),
                    }));
                    if p != old_ip || switched_same_ip {
                        ip = p; // 地址已变化，或同网段已切到目标 SSID
                    }
                }
                if ip.is_some() || round == 1 {
                    break;
                }
                let _ = tx.send(UiEvent::Log(
                    "仍未入网，重跑 camera-factory 5...".to_string(),
                ));
                match adb::shell("camera-factory 5") {
                    Ok(out) => {
                        last5 = out.clone();
                        ip = protocol::parse_fw_wlan_ip(&out);
                    }
                    Err(e) => {
                        last5 = format!("执行失败: {e:#}");
                    }
                }
            }
            // 能读到关联 SSID 就验证换网结果（读不到只能靠地址变化侧面判）
            if ip.is_some() {
                if let Some(s) = assoc_ssid() {
                    assoc_seen = Some(s.clone());
                    let _ = tx.send(UiEvent::Log(format!("重连后关联 SSID: {s}")));
                    if s != wifi.ssid {
                        r.verdict = Verdict::Fail;
                        r.detail = format!(
                            "重连后仍关联在 {s}，未连上配置的 {}（检查该热点是否 2.4G、密码是否正确）",
                            wifi.ssid
                        );
                        return RunOutcome {
                            result: r,
                            device: base.device,
                        };
                    }
                }
            }
        }
    }
    let Some(ip) = ip else {
        r.verdict = Verdict::Fail;
        r.detail = if wifi.ssid.is_empty() {
            "驱动通过，但 wlan0 无 IPv4（未入网/未拿到 DHCP？设置页配产线 SSID 可自动连接）".into()
        } else if rejoined {
            format!(
                "驱动通过，已推送 {} 配置重连，wlan0 仍未拿到 IPv4（5 输出: {}）",
                wifi.ssid,
                clip(&runner::one_line(&last5), 60)
            )
        } else {
            format!(
                "驱动通过，已关联 {} 但 wlan0 无 IPv4（DHCP 未完成？）",
                wifi.ssid
            )
        };
        return RunOutcome {
            result: r,
            device: base.device,
        };
    };
    // 强制重连后地址没变：目标网与旧网同网段（正常）或压根没换网（异常）
    // ——留痕日志，配合下面 PC 网段核对一起定位
    if rejoined && old_ip.as_deref() == Some(ip.as_str()) {
        let _ = tx.send(UiEvent::Log(format!(
            "注意：地址仍为 {ip}（新旧网段相同属正常，否则疑未换网成功）"
        )));
    }

    // RSSI/链路速率：只记录不设门（环境不控），极宽松兜底抓天线级故障
    let rssi = wifi_rssi().or_else(|| {
        // MIPS 无 cmd wifi、/proc/net/wireless 也可能空——退回固件 5 的
        // 回读（join 时输出 wifi-rssi:，真机 2026-10-28 实证）
        protocol::parse_fw_wifi_rssi(&last5).map(|v| (v, 0))
    });

    // PC 与设备同网段地址
    let Some(peer) = ip.parse::<std::net::Ipv4Addr>().ok() else {
        r.verdict = Verdict::Fail;
        r.detail = format!("驱动 ok，IP {ip}；地址无法解析");
        return RunOutcome {
            result: r,
            device: base.device,
        };
    };
    let Some(local) = netip::local_ip_reaching(peer) else {
        // 诊断：把 PC 各网卡网段摆出来（真机 17:01 案例：耳机其实还挂
        // 在 HEC(192.168.1.27)、PC 已切到手机热点网段，两边对不上才报
        // "PC 无同网段"——清单让人一眼看出各在哪边）
        let ifs: Vec<String> = netip::ipv4_list()
            .into_iter()
            .map(|(n, a, m)| format!("{n}={a}/{m}"))
            .collect();
        let listed = if ifs.is_empty() {
            "无 IPv4 网卡".to_string()
        } else {
            ifs.join(", ")
        };
        let _ = tx.send(UiEvent::Log(format!("PC 网卡: {listed}")));
        let stale = if rejoined && old_ip.as_deref() == Some(ip.as_str()) {
            format!(
                "；地址与重连前相同，疑耳机未连上 {}（热点是否 2.4G？密码对吗？）",
                wifi.ssid
            )
        } else {
            String::new()
        };
        r.verdict = Verdict::Fail;
        r.detail = format!("驱动 ok，IP {ip}；PC 无同网段地址（PC 网卡: {listed}）{stale}");
        return RunOutcome {
            result: r,
            device: base.device,
        };
    };
    let _ = tx.send(UiEvent::Log(format!(
        "PC 本地地址 {local}（与设备 {ip} 同网段）"
    )));
    // PC 侧 iperf3 预检：随包分发缺失时明确报错，别让客户端玄学解析失败
    let host = player::resolve_tool(player::exe_dir().as_deref(), "IPERF3", "iperf3");
    let mut probe = std::process::Command::new(&host);
    exec::hide_window(&mut probe);
    match probe.arg("--version").output() {
        Ok(o) if o.status.success() => {}
        _ => {
            r.verdict = Verdict::Fail;
            r.detail =
                format!("驱动 ok，IP {ip}；PC 缺 iperf3（应随包分发；环境变量 IPERF3 可指定路径）");
            return RunOutcome {
                result: r,
                device: base.device,
            };
        }
    }

    // 方向序列：(反向?, 标签, 阈值)；标签与 UI 字体约束一致用中文
    let dirs: &[(bool, &'static str, f32)] = match t.direction {
        IperfDir::Up => &[(false, "上行", t.min_mbps_up)],
        IperfDir::Down => &[(true, "下行", t.min_mbps_down)],
        IperfDir::Both => &[
            (false, "上行", t.min_mbps_up),
            (true, "下行", t.min_mbps_down),
        ],
    };

    let mut parts = vec![match &assoc_seen {
        Some(ap) => format!("IP {ip}（AP {ap}）"),
        None => format!("IP {ip}（AP 未读到）"),
    }];
    let mut all_ok = true;
    for &(rev, label, need) in dirs {
        let run = || iperf_once(tx, &host, t, local, rev);
        let outcome =
            |sum: protocol::IperfSummary, retr_fallback: Option<u32>, retested: bool| DirOutcome {
                label,
                mbps: sum.mbps_median,
                need,
                retr: sum.retransmits.or(retr_fallback),
                retr_max: t.max_retr,
                retested,
            };
        // 命令级失败（超时/连不上）开了复测就换端口再试一次：
        // 共享 2.4G 无线瞬断、端口残留这类一次性的坑不该直接判死
        let mut o = match run() {
            Ok(sum) => outcome(sum, None, false),
            Err(msg) => {
                if !t.retest {
                    all_ok = false;
                    parts.push(format!("{label} 测试失败：{msg}"));
                    continue;
                }
                let _ = tx.send(UiEvent::Log(format!("{label} 失败，自动复测一次...")));
                match run() {
                    Ok(sum) => outcome(sum, None, true),
                    Err(msg2) => {
                        all_ok = false;
                        parts.push(format!("{label} 测试失败：{msg2}"));
                        continue;
                    }
                }
            }
        };
        // 边缘失败或偶发 0Mbps 断流自动复测一次（共享无线环境下瞬时抢道/
        // 反向流建连空跑不冤枉良品；命令失败已复测过的方向不再重复复测）
        if !o.pass() && t.retest && !o.retested {
            let zero_stall = o.need > 0.0 && o.mbps <= 0.01;
            let marginal = (o.need > 0.0 && o.mbps >= o.need * 0.5)
                || o.retr.is_some_and(|v| v <= o.retr_max * 2);
            if zero_stall || marginal {
                let why = if zero_stall {
                    "0Mbps 异常"
                } else {
                    "边缘失败"
                };
                let _ = tx.send(UiEvent::Log(format!("{label} {why}，复测一次...")));
                if let Ok(sum) = run() {
                    o = outcome(sum, None, true);
                }
            }
        }
        if !o.pass() {
            all_ok = false;
        }
        parts.push(o.detail());
    }

    // RSSI 兜底与记录
    if let Some((v, speed)) = rssi {
        if v < -80 {
            all_ok = false;
            parts.push(format!("RSSI {v}dBm 过低（疑天线/馈线故障）"));
        } else if speed > 0 {
            parts.push(format!("RSSI {v}dBm/{speed}Mbps"));
        } else {
            parts.push(format!("RSSI {v}dBm"));
        }
    }

    // 失败归因：ping 网关区分"断连"与"连通但慢"
    if !all_ok {
        if let Some(gw) = adb::shell("ip route")
            .ok()
            .and_then(|o| protocol::parse_default_gateway(&o))
        {
            if let Ok(p) = adb::shell(&format!("ping -c 8 -W 2 {gw}")) {
                if let Some((loss, avg)) = protocol::parse_ping_stats(&p) {
                    let why = if loss >= 50.0 {
                        format!("ping 网关 {loss:.0}% 丢包 → 疑断连/严重不良")
                    } else {
                        format!("ping 网关丢包 {loss:.0}% avg {avg:.0}ms → 连通但速率低")
                    };
                    parts.push(why);
                }
            }
        }
    }

    r.verdict = if all_ok { Verdict::Pass } else { Verdict::Fail };
    r.detail = parts.join("；");
    RunOutcome {
        result: r,
        device: base.device,
    }
}

/// RSSI/链路速率探测链：`cmd wifi status`（Android 11+）→ /proc/net/wireless。
fn wifi_rssi() -> Option<(i32, u32)> {
    if let Ok(out) = adb::shell("cmd wifi status") {
        if let Some(v) = protocol::parse_cmd_wifi_status(&out) {
            return Some(v);
        }
    }
    adb::shell("cat /proc/net/wireless")
        .ok()
        .and_then(|o| protocol::parse_wireless_level(&o))
        .map(|v| (v, 0))
}

/// 找一个空闲 TCP 端口（0.0.0.0 绑定即释放，产线 PC 独占、竞态可忽略）。
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("0.0.0.0:0")
        .ok()?
        .local_addr()
        .ok()
        .map(|a| a.port())
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
                None,
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
                // 虚拟项：蓝牙 MAC 解析到即查绑定记录（V3.13）
                TestItem::Binding,
                TestItem::Wifi,
                TestItem::SdCard,
                TestItem::Battery,
                TestItem::Gyro,
                // E02E 专属（V3.14.1 单项：camera-factory 20 一条指令）
                TestItem::Esim
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
        // 蓝牙 MAC 未解析到 → unknown 兜底文件名（V3.13 键改蓝牙 MAC，
        // SN 仅正文留证）；同序列重写只留一份
        assert!(report
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-unknown.txt"));
        let txts = report::list_reports(&reports, 10)
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains("-unknown"))
            })
            .count();
        assert_eq!(txts, 1, "同一序列只留最终文件");

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
                    None,
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
        // V3.12：iperf3 客户端桩按 -R 与否吐不同 JSON（正向带重传、反向无）；
        // V3.12.3：wlan0 有 IP 由 wlan-up 标记文件控制——adb push 推送
        // wpa_supplicant.conf（产线 SSID 配网）时拷贝留证并创建标记，
        // 模拟"按新配置重连后拿到 DHCP"
        let wifi_case = "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 5\"*) printf 'Successfully initialized wpa_supplicant\\nFAIL-BUSY\\nwlan0-ip: 192.168.3.42\\nwifi-rssi:-39\\n';;\n\
             push*) cp \"$2\" \"$(dirname \"$0\")/pushed.conf\" 2>/dev/null; : > \"$(dirname \"$0\")/wlan-up\"; printf 'ok\\n';;\n\
             *\"ip -4 addr show wlan0\"*) if [ -f \"$(dirname \"$0\")/wlan-up\" ]; then printf 'inet 192.168.3.42/24 brd 192.168.3.255 scope global wlan0\\n'; fi;;\n\
             *\"iperf3 -c\"*) case \"$*\" in\n\
               *\"-R\"*) printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":30e6}}],\"end\":{\"sum\":{\"bits_per_second\":29e6}}}';;\n\
               *) if [ -f \"$(dirname \"$0\")/drop-first\" ]; then rm -f \"$(dirname \"$0\")/drop-first\"; printf '%s' '{\"start\":{\"connected\":[]},\"error\":\"unable to connect to server\"}'; else printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":20e6}},{\"sum\":{\"bits_per_second\":24e6}},{\"sum\":{\"bits_per_second\":22e6}}],\"end\":{\"sum_sent\":{\"bits_per_second\":22e6,\"retransmits\":36},\"sum_received\":{\"bits_per_second\":21.9e6}}} '; fi;;\n\
             esac;;\n\
             *\"ip route\"*) printf 'default via 192.168.1.1 dev wlan0\\n';;\n\
             *\"ping\"*) printf '%s\\n%s\\n' \
             '20 packets transmitted, 20 received, 0% packet loss' \
             'rtt min/avg/max/mdev = 1.2/23.4/99.0/5.0 ms';;\n\
             *) printf 'ok\\n';;\nesac\n";
        // driver 深度（模组工位默认）：不测吞吐，驱动过即过（wlan-up 标记使 IP 恒在）
        std::fs::write(base.join("wlan-up"), b"").unwrap();
        let drv_cfg = Config {
            station: "module".into(),
            ..Config::default()
        };
        let (vs, n) = scenario(
            &wscript,
            items(TestItem::Wifi),
            &media,
            &reports,
            drv_cfg,
            wifi_case,
            || unreachable!(),
        );
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));
        // throughput 深度：假 iperf3 服务端（PC 侧）+ 本机地址注入，
        // 客户端桩上行中位 22Mbps ≥ 10、下行 30 ≥ 15、重传 36/87 ≤ 500 → PASS
        let iscript = base.join("iperf3-host");
        std::fs::write(
            &iscript,
            "#!/bin/sh\nprintf '%s' '{\"start\":{},\"end\":{\"sum_sent\":{\"bits_per_second\":28e6,\"retransmits\":87}}}'\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&iscript, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("IPERF3", &iscript);
        std::env::set_var("E02_LOCAL_IP", "192.168.3.10");
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
        std::env::remove_var("IPERF3");
        std::env::remove_var("E02_LOCAL_IP");
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));

        // V3.12.11：上行首跑连不上（connected:[] 命令级失败）→ 开了复测
        // 换端口再试成功 → 整项仍 PASS（真机 18:44 无线瞬断案例的回归；
        // 无复测逻辑时首跑 Err 会直接 FAIL，PASS 即证明失败复测生效）
        std::fs::write(base.join("wlan-up"), b"").unwrap();
        std::fs::write(base.join("drop-first"), b"").unwrap();
        std::env::set_var("IPERF3", &iscript);
        std::env::set_var("E02_LOCAL_IP", "192.168.3.10");
        let mut flaky_cfg = Config::default();
        flaky_cfg.wifi.depth = crate::config::WifiDepth::Throughput;
        let (vs, n) = scenario(
            &wscript,
            items(TestItem::Wifi),
            &media,
            &reports,
            flaky_cfg,
            wifi_case,
            || unreachable!(),
        );
        std::env::remove_var("IPERF3");
        std::env::remove_var("E02_LOCAL_IP");
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));

        // V3.12.3：配了产线 SSID 且未入网 → adb push 推送 wpa_supplicant.conf
        // （桩拷到 pushed.conf 留证并置 wlan-up 标记模拟重连成功）→
        // camera-factory 5（输出带 FAIL-BUSY 噪声 + 回读 wlan0-ip）→
        // 直接采信回读 IP → 吞吐照常 PASS
        std::fs::remove_file(base.join("wlan-up")).unwrap();
        std::env::set_var("IPERF3", &iscript);
        std::env::set_var("E02_LOCAL_IP", "192.168.3.10");
        let mut cfg = Config::default();
        cfg.wifi.depth = crate::config::WifiDepth::Throughput;
        cfg.wifi.ssid = "line-ap".into();
        cfg.wifi.psk = "12345678".into();
        let (vs, n) = scenario(
            &wscript,
            items(TestItem::Wifi),
            &media,
            &reports,
            cfg,
            wifi_case,
            || unreachable!(),
        );
        std::env::remove_var("IPERF3");
        std::env::remove_var("E02_LOCAL_IP");
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));
        // 推送内容核对：设备原格式 + 产线 SSID/密码的 WPA-PSK 网络块
        let pushed = std::fs::read_to_string(base.join("pushed.conf")).unwrap();
        assert!(pushed.starts_with("ctrl_interface=/var/run/wpa_supplicant"));
        assert!(pushed.contains("ssid=\"line-ap\""));
        assert!(pushed.contains("psk=\"12345678\""));
        assert!(pushed.contains("key_mgmt=WPA-PSK"));

        // V3.12.12：已入网（probe 有旧网 IP）但配了产线 SSID → 仍强制
        // 推送重连，不再是"已入网就直接测"（真机 2026-10-30：设备挂在
        // 别的 AP 上不切走，测的还是旧网）。swap 桩：重连（push）后 5
        // 回读新网地址 192.168.43.9（≠ 旧 192.168.3.42，直接采信）→
        // PASS + pushed.conf 换成了新 SSID 证明重连确实发生
        std::fs::remove_file(base.join("pushed.conf")).unwrap();
        let swap_case = "#!/bin/sh\nd=\"$(dirname \"$0\")\"\ncase \"$*\" in\n\
             *\"camera-factory 5\"*) if [ -f \"$d/phone\" ]; then printf 'Successfully initialized wpa_supplicant\\nwlan0-ip: 192.168.43.9\\nwifi-rssi:-45\\n'; else printf 'ok\\n'; fi;;\n\
             push*) cp \"$2\" \"$d/pushed.conf\" 2>/dev/null; : > \"$d/phone\"; : > \"$d/wlan-up\"; printf 'ok\\n';;\n\
             *\"ip -4 addr show wlan0\"*) if [ -f \"$d/phone\" ]; then printf 'inet 192.168.43.9/24 brd 192.168.43.255 scope global wlan0\\n'; elif [ -f \"$d/wlan-up\" ]; then printf 'inet 192.168.3.42/24 brd 192.168.3.255 scope global wlan0\\n'; fi;;\n\
             *\"iperf3 -c\"*) case \"$*\" in\n\
               *\"-R\"*) printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":30e6}}],\"end\":{\"sum\":{\"bits_per_second\":29e6}}}';;\n\
               *) printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":22e6}}],\"end\":{\"sum_sent\":{\"bits_per_second\":22e6,\"retransmits\":36}}} ';;\n\
             esac;;\n\
             *) printf 'ok\\n';;\nesac\n";
        std::env::set_var("IPERF3", &iscript);
        std::env::set_var("E02_LOCAL_IP", "192.168.43.10");
        let mut cfg = Config::default();
        cfg.wifi.depth = crate::config::WifiDepth::Throughput;
        cfg.wifi.ssid = "phone-ap".into();
        cfg.wifi.psk = "12345678".into();
        let (vs, n) = scenario(
            &wscript,
            items(TestItem::Wifi),
            &media,
            &reports,
            cfg,
            swap_case,
            || unreachable!(),
        );
        std::env::remove_var("IPERF3");
        std::env::remove_var("E02_LOCAL_IP");
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));
        assert!(std::fs::read_to_string(base.join("pushed.conf"))
            .unwrap()
            .contains("ssid=\"phone-ap\""));

        // V3.12.12 陈旧窗口：5 撞 BUSY 提前返回（无 wlan0-ip 回读），
        // 前两轮探测仍吐旧网地址（设备还没断开），第三轮才见新网地址
        // ——与旧地址相同的探测不得采信。probe.n 恰为 4（首查 1 + 轮询
        // 3：陈旧×2 后新址×1）即证明等到了地址变化；若采信陈旧，第 2
        // 次探测就停了
        std::fs::remove_file(base.join("pushed.conf")).unwrap();
        let stale_case = "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 5\"*) if [ -f \"$(dirname \"$0\")/pushed.conf\" ]; then printf 'FAIL-BUSY\\nwifi-rssi:-41\\n'; else printf 'ok\\n'; fi;;\n\
             push*) cp \"$2\" \"$(dirname \"$0\")/pushed.conf\" 2>/dev/null; printf 'ok\\n';;\n\
             *\"ip -4 addr show wlan0\"*) d=\"$(dirname \"$0\")\"; n=$(cat \"$d/probe.n\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$d/probe.n\"; if [ \"$n\" -ge 4 ]; then printf 'inet 192.168.43.7/24 brd 192.168.43.255 scope global wlan0\\n'; else printf 'inet 192.168.3.42/24 brd 192.168.3.255 scope global wlan0\\n'; fi;;\n\
             *\"iperf3 -c\"*) case \"$*\" in\n\
               *\"-R\"*) printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":30e6}}],\"end\":{\"sum\":{\"bits_per_second\":29e6}}}';;\n\
               *) printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":22e6}}],\"end\":{\"sum_sent\":{\"bits_per_second\":22e6,\"retransmits\":36}}} ';;\n\
             esac;;\n\
             *) printf 'ok\\n';;\nesac\n";
        std::env::set_var("IPERF3", &iscript);
        std::env::set_var("E02_LOCAL_IP", "192.168.43.10");
        let mut stale_cfg = Config::default();
        stale_cfg.wifi.depth = crate::config::WifiDepth::Throughput;
        stale_cfg.wifi.ssid = "phone-ap".into();
        stale_cfg.wifi.psk = "12345678".into();
        let (vs, n) = scenario(
            &wscript,
            items(TestItem::Wifi),
            &media,
            &reports,
            stale_cfg,
            stale_case,
            || unreachable!(),
        );
        std::env::remove_var("IPERF3");
        std::env::remove_var("E02_LOCAL_IP");
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0));
        assert_eq!(
            std::fs::read_to_string(base.join("probe.n"))
                .unwrap()
                .trim(),
            "4"
        );

        // V3.12.13 关联 SSID 验证：assoc.ssid 文件控制 wpa_cli 回什么。
        // ① 设备 stuck 在 HEC（重连后仍 ssid=HEC ≠ 配置 phone-ap）→
        //    明确 FAIL"未连上配置的 AP"，而不是拿旧网地址误报 PC 网段错
        // ② 已关联配置的 AP（ssid=phone-ap 且有 IP）→ 跳过重连（无 push）
        let assoc_case = "#!/bin/sh\nd=\"$(dirname \"$0\")\"\ncase \"$*\" in\n\
             *wpa_cli*) s=$(cat \"$d/assoc.ssid\" 2>/dev/null || echo none); if [ \"$s\" = \"none\" ]; then printf 'wpa_state=SCANNING\\n'; else printf \"ssid=$s\\nwpa_state=COMPLETED\\n\"; fi;;\n\
             *\"camera-factory 5\"*) if [ -f \"$d/pushed.conf\" ]; then printf 'Successfully initialized wpa_supplicant\\nwlan0-ip: 192.168.43.9\\nwifi-rssi:-45\\n'; else printf 'ok\\n'; fi;;\n\
             push*) cp \"$2\" \"$d/pushed.conf\" 2>/dev/null; : > \"$d/wlan-up\"; printf 'ok\\n';;\n\
             *\"ip -4 addr show wlan0\"*) if [ -f \"$d/pushed.conf\" ]; then printf 'inet 192.168.43.9/24 brd 192.168.43.255 scope global wlan0\\n'; elif [ -f \"$d/wlan-up\" ]; then printf 'inet 192.168.3.42/24 brd 192.168.3.255 scope global wlan0\\n'; fi;;\n\
             *\"iperf3 -c\"*) printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":22e6}}],\"end\":{\"sum_sent\":{\"bits_per_second\":22e6,\"retransmits\":36}}} ';;\n\
             *) printf 'ok\\n';;\nesac\n";
        std::fs::remove_file(base.join("pushed.conf")).unwrap();
        std::fs::write(base.join("assoc.ssid"), "HEC").unwrap();
        std::env::set_var("IPERF3", &iscript);
        std::env::set_var("E02_LOCAL_IP", "192.168.43.10");
        let mut assoc_cfg = Config::default();
        assoc_cfg.wifi.depth = crate::config::WifiDepth::Throughput;
        assoc_cfg.wifi.ssid = "phone-ap".into();
        assoc_cfg.wifi.psk = "12345678".into();
        let (vs, n) = scenario(
            &wscript,
            items(TestItem::Wifi),
            &media,
            &reports,
            assoc_cfg.clone(),
            assoc_case,
            || unreachable!(),
        );
        assert_eq!((vs[0].1, n), (Verdict::Fail, 0)); // 未换网成功要 FAIL

        std::fs::remove_file(base.join("pushed.conf")).unwrap();
        std::fs::write(base.join("assoc.ssid"), "phone-ap").unwrap();
        let (vs, n) = scenario(
            &wscript,
            items(TestItem::Wifi),
            &media,
            &reports,
            assoc_cfg,
            assoc_case,
            || unreachable!(),
        );
        std::env::remove_var("IPERF3");
        std::env::remove_var("E02_LOCAL_IP");
        assert_eq!((vs[0].1, n), (Verdict::Pass, 0)); // 已在配置 AP 上
        assert!(!base.join("pushed.conf").exists()); // 跳过重连 = 没 push

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
                    None,
                )
            });
            let (mut asked, mut verdict) = (0usize, None);
            while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
                match ev {
                    UiEvent::Manual { .. } => asked += 1,
                    // 回环行到达 = 脚本已在 sleep 中：此刻作答才真正考验"应答即杀"
                    UiEvent::Log(ref l) if l.contains("回环:") => {
                        mtx.send(UiResponse::Manual(true)).unwrap();
                    }
                    UiEvent::Result(r, _) => verdict = Some(r.verdict),
                    _ => {}
                }
                if verdict.is_some() && asked == 2 {
                    break;
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
                None,
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
                None,
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
                None,
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
                None,
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
            run_sequence(&items, &tx, &mrx, &m, &r, &cfg, RunBase::default(), None)
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
            skip_time_sync: false,
        };
        // 单项补测 WiFi（假桩输出 ok → 合格），应替换底稿槽位并重写整机报告
        let items = vec![TestItem::Wifi];
        let mut cfg = Config::default();
        cfg.set_active_wifi_depth(crate::config::WifiDepth::Driver);
        let (tx, rx) = mpsc::channel();
        let (_mtx, mrx) = mpsc::channel();
        let (m, r) = (media.clone(), reports.clone());
        let handle =
            thread::spawn(move || run_sequence(&items, &tx, &mrx, &m, &r, &cfg, base, None));
        let mut verdicts = Vec::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            if let UiEvent::Result(res, _) = ev {
                verdicts.push((res.item, res.verdict));
            }
        }
        let report = handle.join().unwrap().expect("补测应产出报告");
        assert_eq!(verdicts, vec![(TestItem::Wifi, Verdict::Pass)]);
        // 蓝牙 MAC 未解析到 → unknown 兜底文件名；整机报告被重写，不散页
        assert!(report.to_string_lossy().contains("unknown"), "{report:?}");
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

    /// 绑定台账三段（V3.13）：新机入账 → 同蓝牙不同 WiFi 判冲突（文件名加
    /// "-冲突"后缀、不动台账）→ 原对复测放行计数。台账指到临时目录，
    /// 不污染真实数据目录。
    #[test]
    fn binding_ledger_new_conflict_retest() {
        let _g = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::exec::clear_abort();
        let base_dir = std::env::temp_dir().join(format!("e02-bind-{}", std::process::id()));
        let reports = base_dir.join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        let ledger_path = base_dir.join("ledger.json");
        // 假 adb：3 回 SN+WiFi（WiFi 由环境变量切换机型），10 回蓝牙 MAC
        let script = base_dir.join("adb-bind");
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 3\"*) printf 'sn-address:00001\\nwifi-mac:%s\\n' \"$WIFI_MAC\";;\n\
             *\"camera-factory 10\"*) printf 'MAC:0011A3B4C5D6\\n';;\n\
             *) printf 'ok\\n';;\nesac\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", &script);

        let run = |wifi: &str| -> (Option<PathBuf>, Vec<(TestItem, Verdict)>) {
            std::env::set_var("WIFI_MAC", wifi);
            let mut cfg = Config::default();
            cfg.binding.ledger_path = Some(ledger_path.to_string_lossy().into_owned());
            let items = vec![TestItem::DeviceInfo, TestItem::BtMac];
            let (tx, rx) = mpsc::channel();
            let (_mtx, mrx) = mpsc::channel();
            let (m, r) = (base_dir.join("media"), reports.clone());
            let handle = thread::spawn(move || {
                run_sequence(&items, &tx, &mrx, &m, &r, &cfg, RunBase::default(), None)
            });
            let mut verdicts = Vec::new();
            while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
                if let UiEvent::Result(res, _) = ev {
                    verdicts.push((res.item, res.verdict));
                }
            }
            (handle.join().unwrap(), verdicts)
        };

        // 1) 新机：绑定 PASS 入账，报告以蓝牙 MAC 为键
        let (p1, v1) = run("00:60:A7:0B:9C:F2");
        let p1 = p1.expect("新机应产出报告");
        assert!(
            p1.to_string_lossy().ends_with("-0011A3B4C5D6.txt"),
            "{p1:?}"
        );
        assert!(v1.contains(&(TestItem::Binding, Verdict::Pass)), "{v1:?}");
        let text1 = std::fs::read_to_string(&p1).unwrap();
        assert!(text1.contains("MAC 绑定:PASS 新设备入账"), "{text1}");
        assert!(text1.contains("结果: PASS"), "{text1}");
        let l = Ledger::load(&ledger_path);
        assert_eq!(l.len(), 1);

        // 2) 同蓝牙不同 WiFi：判 FAIL 告警，文件名加 -冲突，台账不动
        let (p2, v2) = run("00:60:A7:0B:9C:99");
        let p2 = p2.expect("冲突机也应产出报告（其余项测完留证）");
        let name2 = p2.to_string_lossy();
        assert!(name2.contains("0011A3B4C5D6-冲突"), "{name2}");
        assert_ne!(p1, p2, "冲突机不得覆盖原机报告");
        assert!(v2.contains(&(TestItem::Binding, Verdict::Fail)), "{v2:?}");
        let text2 = std::fs::read_to_string(&p2).unwrap();
        assert!(text2.contains("MAC 绑定:FAIL"), "{text2}");
        assert!(
            text2.contains("该蓝牙地址已绑定WiFi 00:60:A7:0B:9C:F2"),
            "{text2}"
        );
        assert!(text2.contains("结果: FAIL"), "{text2}");
        assert_eq!(Ledger::load(&ledger_path).len(), 1, "冲突不入账");

        // 3) 原对复测：不报错，计数累加，整机翻回 PASS
        let (p3, v3) = run("00:60:A7:0B:9C:F2");
        assert_eq!(p3.as_ref(), Some(&p1), "同一设备复测覆盖同一份报告");
        assert!(v3.contains(&(TestItem::Binding, Verdict::Pass)), "{v3:?}");
        let text3 = std::fs::read_to_string(&p1).unwrap();
        assert!(
            text3.contains("MAC 绑定:PASS 同一设备第 2 次测试"),
            "{text3}"
        );
        assert!(text3.contains("结果: PASS"), "{text3}");
        let l = Ledger::load(&ledger_path);
        assert_eq!(l.recent(5)[0].tests, 2);
        assert_eq!(l.recent(5)[0].last_verdict, "PASS");

        std::env::remove_var("ADB");
        std::env::remove_var("WIFI_MAC");
        crate::exec::clear_abort();
        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// E02E（eSIM 型，V3.14）全链：型号手动锁定 E02E → 蓝牙↔IMEI 绑定
    /// （不同 IMEI 判冲突）→ IMEI/CCID 单项走 camera-factory 20 → 报告
    /// e02e- 前缀 + E02E 标题。假 adb 同前，20 的值由环境变量切换。
    #[test]
    fn e02e_model_flow_and_imei_binding() {
        let _g = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::exec::clear_abort();
        let base_dir = std::env::temp_dir().join(format!("e02-e02e-{}", std::process::id()));
        let reports = base_dir.join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        let ledger_path = base_dir.join("ledger.json");
        let script = base_dir.join("adb-e02e");
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 3\"*) printf 'sn-address:00001\\nsoft-ware:E02E-1.0.1\\n';;\n\
             *\"camera-factory 10\"*) printf 'MAC:0011A3B4C5D6\\n';;\n\
             *\"camera-factory 17\"*) printf 'qmi8658 slave = 0x6a\\ngyroscope-test:true\\n';;\n\
             *\"camera-factory 20\"*) printf 'imei:%s\\nccid:%s\\n' \"$E_IMEI\" \"$E_CCID\";;\n\
             *) printf 'ok\\n';;\nesac\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", &script);

        let run = |imei: &str| -> (Option<PathBuf>, Vec<(TestItem, Verdict)>) {
            std::env::set_var("E_IMEI", imei);
            std::env::set_var("E_CCID", "89860115123456789012");
            let mut cfg = Config::default();
            cfg.binding.ledger_path = Some(ledger_path.to_string_lossy().into_owned());
            // 型号手动锁定 E02E（V3.14.1：不随固件名自动切换）
            cfg.model = Some(ModelKind::E02e);
            // e02e 产线模板的项集（陀螺仪 + IMEI/CCID 单项 + 身份来源）
            let items = vec![
                TestItem::DeviceInfo,
                TestItem::BtMac,
                TestItem::Gyro,
                TestItem::Esim,
            ];
            let (tx, rx) = mpsc::channel();
            let (_mtx, mrx) = mpsc::channel();
            let (m, r) = (base_dir.join("media"), reports.clone());
            let handle = thread::spawn(move || {
                run_sequence(&items, &tx, &mrx, &m, &r, &cfg, RunBase::default(), None)
            });
            let mut verdicts = Vec::new();
            while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
                if let UiEvent::Result(res, _) = ev {
                    verdicts.push((res.item, res.verdict));
                }
            }
            (handle.join().unwrap(), verdicts)
        };

        // 1) 新机：型号锁定 E02E，蓝牙↔IMEI 入账，报告 e02e- 前缀
        let (p1, v1) = run("864192051234567");
        let p1 = p1.expect("E02E 新机应产出报告");
        let name1 = p1.to_string_lossy();
        assert!(name1.contains("e02e-"), "{name1}");
        assert!(name1.ends_with("-0011A3B4C5D6.txt"), "{name1}");
        assert!(v1.contains(&(TestItem::Binding, Verdict::Pass)), "{v1:?}");
        assert!(v1.contains(&(TestItem::Esim, Verdict::Pass)), "{v1:?}");
        assert!(v1.contains(&(TestItem::Gyro, Verdict::Pass)), "{v1:?}");
        let text1 = std::fs::read_to_string(&p1).unwrap();
        assert!(text1.contains("E02E 工厂测试报告"), "{text1}");
        assert!(text1.contains("MAC 绑定:PASS 新设备入账"), "{text1}");
        assert!(text1.contains("imei:864192051234567"), "{text1}");
        assert!(text1.contains("ccid:89860115123456789012"), "{text1}");
        assert!(text1.contains("IMEI/CCID:PASS"), "{text1}");
        assert!(text1.contains("陀螺仪测试:PASS"), "{text1}");
        assert!(text1.contains("结果: PASS"), "{text1}");
        let rec = Ledger::load(&ledger_path).recent(5)[0].clone();
        assert_eq!(rec.partner, "864192051234567", "E02E 绑定对 = 蓝牙↔IMEI");
        assert_eq!(rec.model, "E02E");

        // 2) 同蓝牙不同 IMEI：判冲突（绑定对是 IMEI 不是 WiFi），台账不动
        let (p2, v2) = run("864192051234999");
        let p2 = p2.expect("冲突机也应产出报告");
        let name2 = p2.to_string_lossy();
        assert!(name2.contains("0011A3B4C5D6-冲突"), "{name2}");
        assert_ne!(p1, p2, "冲突机不得覆盖原机报告");
        assert!(v2.contains(&(TestItem::Binding, Verdict::Fail)), "{v2:?}");
        let text2 = std::fs::read_to_string(&p2).unwrap();
        assert!(
            text2.contains("该蓝牙地址已绑定IMEI 864192051234567"),
            "{text2}"
        );
        assert!(text2.contains("结果: FAIL"), "{text2}");
        assert_eq!(Ledger::load(&ledger_path).len(), 1, "冲突不入账");

        // 3) 原对复测：放行计数，报告覆盖同份
        let (p3, v3) = run("864192051234567");
        assert_eq!(p3.as_ref(), Some(&p1), "同机复测覆盖同一份报告");
        assert!(v3.contains(&(TestItem::Binding, Verdict::Pass)), "{v3:?}");
        let text3 = std::fs::read_to_string(&p1).unwrap();
        assert!(
            text3.contains("MAC 绑定:PASS 同一设备第 2 次测试"),
            "{text3}"
        );
        let l = Ledger::load(&ledger_path);
        assert_eq!(l.recent(5)[0].tests, 2);

        std::env::remove_var("ADB");
        std::env::remove_var("E_IMEI");
        std::env::remove_var("E_CCID");
        crate::exec::clear_abort();
        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// 设备定向（V3.15）：run_sequence 带 serial 时所有 adb 子命令注入
    /// ANDROID_SERIAL。假桩把该环境变量回读成 SN，端到端验证注入生效；
    /// 序列结束后定向恢复（Guard Drop）。
    #[test]
    fn adb_serial_target_directs_commands() {
        let _g = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::exec::clear_abort();
        let base_dir = std::env::temp_dir().join(format!("e02-serial-{}", std::process::id()));
        let reports = base_dir.join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        let script = base_dir.join("adb-serial");
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$*\" in\n\
             *\"camera-factory 3\"*) printf 'sn-address:%s\\n' \"$ANDROID_SERIAL\";;\n\
             *) printf 'ok\\n';;\nesac\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", &script);
        let items = vec![TestItem::DeviceInfo];
        let (tx, rx) = mpsc::channel();
        let (_mtx, mrx) = mpsc::channel();
        let (m, r) = (base_dir.join("media"), reports.clone());
        let handle = thread::spawn(move || {
            run_sequence(
                &items,
                &tx,
                &mrx,
                &m,
                &r,
                &Config::default(),
                RunBase::default(),
                Some("SERIAL123".into()),
            )
        });
        let mut pass = false;
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            if let UiEvent::Result(res, _) = ev {
                pass = res.verdict == Verdict::Pass;
            }
        }
        let report = handle.join().unwrap().expect("应产出报告");
        assert!(pass, "设备信息应 PASS（桩回读 ANDROID_SERIAL 作 SN）");
        let text = std::fs::read_to_string(&report).unwrap();
        assert!(text.contains("sn-address:SERIAL123"), "{text}");
        assert_eq!(crate::adb::target(), None, "序列结束应恢复定向");
        std::env::remove_var("ADB");
        crate::exec::clear_abort();
        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// 验证：1) 同一设备首次执行发校时并上报 TimeSynced，后续单点测试传 skip_time_sync=true 不再重复发校时；
    /// 2) 蓝牙 MAC 项顺带采集 cf7 蓝牙版本，并在报告落盘 `bt-ver:`。
    #[test]
    fn skip_time_sync_and_bt_version_collection() {
        let _g = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::exec::clear_abort();
        let base_dir = std::env::temp_dir().join(format!("e02-sync-btv-{}", std::process::id()));
        let reports = base_dir.join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        let sync_log = base_dir.join("sync.log");
        let script = base_dir.join("adb-sync");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n\
                 *\"date -s\"*|*\"camera-factory 19\"*) echo \"$*\" >> '{}'; printf 'ok\\n';;\n\
                 *\"camera-factory 10\"*) printf 'MAC:0011A3B4C5D6\\n';;\n\
                 *\"camera-factory 7\"*) printf 'bt-ver:V3_9_2\\n';;\n\
                 *) printf 'ok\\n';;\nesac\n",
                sync_log.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("ADB", &script);

        // 第 1 次：未校时 → 执行校时并收到 TimeSynced，同时收集到 bt_ver
        let (tx, rx) = mpsc::channel();
        let (_mtx, mrx) = mpsc::channel();
        let (m, r) = (base_dir.join("media"), reports.clone());
        let h1 = thread::spawn(move || {
            run_sequence(
                &[TestItem::BtMac],
                &tx,
                &mrx,
                &m,
                &r,
                &Config::default(),
                RunBase::default(),
                None,
            )
        });
        let mut saw_synced = false;
        let mut bt_ver = String::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(30)) {
            match ev {
                UiEvent::TimeSynced => saw_synced = true,
                UiEvent::Result(_, Some(dev)) if !dev.bt_ver.is_empty() => bt_ver = dev.bt_ver,
                _ => {}
            }
        }
        let rep = h1.join().unwrap().expect("应产出报告");
        assert!(saw_synced, "首次执行应上报 TimeSynced");
        assert_eq!(bt_ver, "V3_9_2");
        let rep_text = std::fs::read_to_string(&rep).unwrap();
        assert!(rep_text.contains("bt-ver:V3_9_2"), "{rep_text}");
        let lines_first = std::fs::read_to_string(&sync_log).unwrap().lines().count();
        assert_eq!(lines_first, 2, "首次应执行 date -s 与 camera-factory 19");

        // 第 2 次：同机单点重测（skip_time_sync = true）→ 不再调用校时命令
        let (tx2, rx2) = mpsc::channel();
        let (_mtx2, mrx2) = mpsc::channel();
        let (m2, r2) = (base_dir.join("media"), reports.clone());
        let h2 = thread::spawn(move || {
            run_sequence(
                &[TestItem::BtMac],
                &tx2,
                &mrx2,
                &m2,
                &r2,
                &Config::default(),
                RunBase {
                    skip_time_sync: true,
                    ..RunBase::default()
                },
                None,
            )
        });
        let mut saw_synced2 = false;
        while let Ok(ev) = rx2.recv_timeout(Duration::from_secs(30)) {
            if matches!(ev, UiEvent::TimeSynced) {
                saw_synced2 = true;
            }
        }
        h2.join().unwrap();
        assert!(!saw_synced2, "skip_time_sync 时不应再发校时");
        let lines_second = std::fs::read_to_string(&sync_log).unwrap().lines().count();
        assert_eq!(lines_second, 2, "二次单点测试不应再执行 date -s / cf19");

        std::env::remove_var("ADB");
        crate::exec::clear_abort();
        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// 回归（2026-11-28 真机日志）：
    /// 1) 同路由器从 HEC 切 HEC 5G 且 DHCP 返回同一 IP（192.168.1.161）时，
    ///    核对 assoc_ssid 已切换即直接采信，不白等 10 轮；
    /// 2) 下行 iperf3 偶发 0.0Mbps 时自动复测一次并通过。
    #[test]
    fn wifi_same_subnet_ssid_switch_fast_and_zero_mbps_retest() {
        let _g = crate::exec::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::exec::clear_abort();
        let base_dir = std::env::temp_dir().join(format!("e02-wifi-reg-{}", std::process::id()));
        let media = base_dir.join("media");
        let reports = base_dir.join("reports");
        std::fs::create_dir_all(&reports).unwrap();

        let iscript = base_dir.join("iperf3-host");
        std::fs::write(
            &iscript,
            "#!/bin/sh\nprintf '%s' '{\"start\":{},\"end\":{\"sum_sent\":{\"bits_per_second\":30e6,\"retransmits\":0}}}'\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&iscript, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let wscript = base_dir.join("adb-wifi-reg");
        let script = "\
#!/bin/sh
d=\"$(dirname \"$0\")\"
case \"$*\" in
  *wpa_cli*)
    if [ -f \"$d/pushed.conf\" ]; then
      printf 'ssid=HEC 5G\\nwpa_state=COMPLETED\\n'
    else
      printf 'ssid=HEC\\nwpa_state=COMPLETED\\n'
    fi
    ;;
  push*)
    cp \"$2\" \"$d/pushed.conf\" 2>/dev/null
    printf 'ok\\n'
    ;;
  *'camera-factory 5'*)
    printf 'Successfully initialized wpa_supplicant\\nwlan0-ip: 192.168.1.161\\nwifi-rssi:-28\\n'
    ;;
  *'ip -4 addr show wlan0'*)
    printf 'inet 192.168.1.161/24 brd 192.168.1.255 scope global wlan0\\n'
    ;;
  *'iperf3 -c'*)
    case \"$*\" in
      *'-R'*)
        n=$(cat \"$d/dl.n\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$d/dl.n\"
        if [ \"$n\" -eq 1 ]; then
          printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":0}}],\"end\":{\"sum_received\":{\"bits_per_second\":0}}}'
        else
          printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":39e6}}],\"end\":{\"sum_received\":{\"bits_per_second\":39e6}}}'
        fi
        ;;
      *)
        printf '%s' '{\"intervals\":[{\"sum\":{\"bits_per_second\":22e6}}],\"end\":{\"sum_sent\":{\"bits_per_second\":22e6,\"retransmits\":0}}}'
        ;;
    esac
    ;;
  *) printf 'ok\\n';;
esac
";
        std::env::set_var("IPERF3", &iscript);
        std::env::set_var("E02_LOCAL_IP", "192.168.1.183");
        let mut cfg = Config::default();
        cfg.wifi.ssid = "HEC 5G".into();
        cfg.wifi.psk = "12345678".into();
        let t0 = Instant::now();
        let (vs, _) = scenario(
            &wscript,
            vec![TestItem::Wifi],
            &media,
            &reports,
            cfg,
            script,
            || unreachable!(),
        );
        let elapsed = t0.elapsed();
        std::env::remove_var("IPERF3");
        std::env::remove_var("E02_LOCAL_IP");
        std::env::remove_var("ADB");

        assert_eq!(vs, vec![(TestItem::Wifi, Verdict::Pass)]);
        assert!(
            elapsed < Duration::from_secs(10),
            "同网段 SSID 已切换不应空等 20s，实测 {elapsed:?}"
        );
        let dl_runs: u32 = std::fs::read_to_string(base_dir.join("dl.n"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(dl_runs, 2, "下行 0.0Mbps 应自动复测一次");

        crate::exec::clear_abort();
        let _ = std::fs::remove_dir_all(&base_dir);
    }
}
