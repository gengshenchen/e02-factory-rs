//! 应用状态与窗口装配：worker 事件消费、媒体显示、人工判定应答。

use crate::config::{self, Config};
use crate::devwatch::{self, DevState};
use crate::engine::events::{UiEvent, UiResponse};
use crate::engine::worker;
use crate::engine::{TestItem, TestResult};
use crate::exec::StopReason;
use crate::protocol::DeviceInfo;
use crate::report;
use crate::ui;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// 内嵌中文字体：Droid Sans Fallback（Apache-2.0，完整 CJK 覆盖，3.9MB）。
/// M6 打包阶段如需更精致字形，可换 Noto Sans SC 子集。
const CJK_FONT: &[u8] = include_bytes!("../assets/fonts/DroidSansFallbackFull.ttf");

/// 视频回放帧率（worker 抽帧同为 2fps）。
const PLAYBACK_INTERVAL: Duration = Duration::from_millis(200);

/// 人工判定待应答请求（UI 弹窗收集应答后经 reply 传回 worker）。
pub struct ManualAsk {
    pub title: String,
    pub question: String,
    /// 应答截止时刻（None = 不限时）；到点 worker 自动判失败
    pub deadline: Option<Instant>,
    reply: Sender<UiResponse>,
}

impl ManualAsk {
    pub fn answer(&self, yes: bool) {
        let _ = self.reply.send(UiResponse::Manual(yes));
    }
}

/// 按键引导弹窗状态（worker 经 [`UiEvent::KeysUpdate`] 驱动）。
pub struct KeysPrompt {
    /// 已检测到的键码（4=多功能 5=开机 6=音量）
    pub detected: Vec<u8>,
    /// true：未收齐，等待操作员 重测/改判
    pub awaiting: bool,
    /// 应答截止时刻（None = 不限时）；到点 worker 自动判失败
    pub deadline: Option<Instant>,
    reply: Sender<UiResponse>,
}

impl KeysPrompt {
    pub fn retry(&self) {
        let _ = self.reply.send(UiResponse::KeysRetry);
    }

    pub fn force(&self, yes: bool) {
        let _ = self.reply.send(UiResponse::KeysForce(yes));
    }
}

/// 序列进度（顶栏进度条数据）。
pub struct Progress {
    pub total: usize,
    pub done: usize,
    pub started: Instant,
}

/// 媒体区状态：占位说明 / 拍照图片 / 录像抽帧回放。
pub enum MediaState {
    Note(String),
    Photo {
        texture: egui::TextureHandle,
        size: [f32; 2],
    },
    Video {
        frames: Vec<egui::TextureHandle>,
        sizes: Vec<[f32; 2]>,
        idx: usize,
        last: Instant,
        has_audio: bool,
    },
}

/// 应用全局状态。
pub struct E02App {
    /// 被测设备信息（原 Deviceinfo 结构）
    pub device: DeviceInfo,
    /// 设备在位状态（V3.8 USB 插拔监测：开测门槛与拔出中止依据）
    pub dev: DevState,
    /// 各测试项结果，顺序与左侧面板一致
    pub results: Vec<TestResult>,
    /// 测试序列进行中
    pub busy: bool,
    /// 当前正在执行的测试项
    pub current: Option<TestItem>,
    /// 待应答的人工判定弹窗
    pub manual: Option<ManualAsk>,
    /// 媒体区
    pub media: MediaState,
    /// 过程日志（尾部若干条）
    pub log: VecDeque<String>,
    /// 序列进度；空闲时 None
    pub(crate) progress: Option<Progress>,
    /// 按键引导弹窗；无按键测试进行时 None
    pub keys_prompt: Option<KeysPrompt>,
    /// 工位配置（V2.2）
    pub config: Config,
    /// 设置窗口开关
    pub settings_open: bool,
    /// 最近一次报告路径（Finished 时更新，报告中心默认选中用）
    pub last_report: Option<PathBuf>,
    /// 报告中心窗口开关
    pub report_open: bool,
    /// 报告目录（数据目录下 reports/，V3）
    pub reports_dir: PathBuf,
    /// 今日统计（V3 报告中心）
    pub today: Option<report::DayStats>,
    /// 报告中心：历史列表（新→旧）
    pub report_list: Vec<PathBuf>,
    /// 报告中心：当前选中
    pub report_sel: Option<PathBuf>,
    /// 报告中心：选中文件内容缓存（路径, 文本）
    report_text: Option<(PathBuf, String)>,
    event_rx: Option<Receiver<UiEvent>>,
    /// 在位监测线程事件（V3.8）
    dev_rx: Receiver<DevState>,
    manual_tx: Option<Sender<UiResponse>>,
    /// 录像回放声音（V3.6）：OutputStream 须保活，Sink 可替换曲目
    audio_out: Option<(rodio::OutputStream, rodio::OutputStreamHandle)>,
    audio_sink: Option<rodio::Sink>,
}

impl E02App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        ui::theme::apply(&cc.egui_ctx);
        // USB 在位监测（V3.8）：后台线程轮询 adb devices，插拔经事件驱动 UI
        let dev_rx = devwatch::spawn(&cc.egui_ctx);
        // 配置：exe 同目录 config.toml；缺失用内置默认，坏文件降级并提示
        let (cfg, cfg_err) = match Config::load(&config::default_path()) {
            Ok(c) => (c, None),
            Err(e) => (
                Config::builtin(),
                Some(format!("配置加载失败，已用内置默认: {e:#}")),
            ),
        };
        let mut app = Self {
            device: DeviceInfo::default(),
            dev: DevState::Disconnected,
            results: TestItem::ALL.map(TestResult::new).to_vec(),
            busy: false,
            current: None,
            manual: None,
            media: MediaState::Note("媒体区：拍照显示图片，录像解码后回放".into()),
            log: VecDeque::new(),
            progress: None,
            keys_prompt: None,
            config: cfg,
            settings_open: false,
            last_report: None,
            report_open: false,
            reports_dir: crate::paths::data_dir().join("reports"),
            today: None,
            report_list: Vec::new(),
            report_sel: None,
            report_text: None,
            event_rx: None,
            dev_rx,
            manual_tx: None,
            audio_out: None,
            audio_sink: None,
        };
        if let Some(msg) = cfg_err {
            app.push_log(msg);
        }
        app.push_log(format!(
            "工位: {}（启用 {} 项），数据目录 {}",
            app.config.station,
            app.config.active_items().len(),
            crate::paths::data_dir().display()
        ));
        app.push_log("等待设备插入（USB 连接后自动检测）");
        app.refresh_stats();
        app
    }

    /// 开始测试项集：当前工位启用项，按原程序批次顺序执行。
    pub fn batch_items(&self) -> Vec<TestItem> {
        let active = self.config.active_items();
        worker::BATCH_ORDER
            .into_iter()
            .filter(|i| active.contains(i))
            .collect()
    }

    /// 设备是否在线且唯一（可开始测试）。
    pub fn device_online(&self) -> bool {
        matches!(self.dev, DevState::Online(_))
    }

    /// 启动测试序列（单项或全测）；busy 期间忽略重复触发。
    pub fn start_run(&mut self, items: Vec<TestItem>) {
        if self.busy || items.is_empty() {
            return;
        }
        // 在位门槛（V3.8）：所有测试依赖 USB 连接，未插/多台都不开测
        if !self.device_online() {
            self.push_log(match &self.dev {
                DevState::Disconnected => "设备未连接：请插入 USB 线后再开始".to_string(),
                DevState::Multiple => "检测到多台设备：请只连接一台被测耳机".to_string(),
                DevState::Online(_) => unreachable!("device_online 已排除"),
            });
            return;
        }
        self.busy = true;
        self.current = items.first().copied();
        self.progress = Some(Progress {
            total: items.len(),
            done: 0,
            started: Instant::now(),
        });
        self.keys_prompt = None;
        let (tx, rx) = mpsc::channel();
        let (mtx, mrx) = mpsc::channel();
        self.event_rx = Some(rx);
        self.manual_tx = Some(mtx);
        let media = crate::paths::data_dir().join("media");
        let reports = self.reports_dir.clone();
        let n = items.len();
        let cfg = self.config.clone();
        // 单项补测以当前整机结果为底稿（V3.9）：测完按项合并、重写整机
        // 报告，不另散一份单项报告；全测从空白开始
        let base = if items.len() == 1 {
            let active = self.config.active_items();
            worker::RunBase {
                device: self.device.clone(),
                results: self
                    .results
                    .iter()
                    .filter(|r| active.contains(&r.item))
                    .cloned()
                    .collect(),
            }
        } else {
            worker::RunBase::default()
        };
        thread::spawn(move || {
            let report = worker::run_sequence(&items, &tx, &mrx, &media, &reports, &cfg, base);
            let _ = tx.send(UiEvent::Finished { report });
        });
        self.push_log(format!("开始测试，共 {n} 项（自动项先行，人工项弹窗判定）"));
    }

    /// 操作员请求停止：杀掉当前命令、剩余项记跳过（worker 侧配合）。
    /// 弹窗立即收起给"已停"反馈（worker 侧 Result/Finished 事件再兜底），
    /// 回放声音同步停止。
    pub fn request_stop(&mut self, reason: StopReason) {
        crate::exec::request_abort(reason);
        self.keys_prompt = None;
        self.manual = None;
        self.stop_audio();
        let msg = match reason {
            StopReason::User => "已请求停止，等待当前命令退出...",
            StopReason::DeviceGone => "设备已拔出，测试中止，等待当前命令退出...",
        };
        self.push_log(msg);
    }

    /// 清空上一台的界面显示（V3.9）：结果回待测、设备信息清空、媒体区
    /// 复位、停回放声音；报告文件与日志不动（磁盘审计留痕）。
    /// 在"下一台插入"沿调用而非拔出沿——操作员拔线后还要看结果。
    fn clear_unit_state(&mut self) {
        self.results = TestItem::ALL.map(TestResult::new).to_vec();
        self.device = DeviceInfo::default();
        self.stop_audio();
        self.media = MediaState::Note("媒体区：拍照显示图片，录像解码后回放".into());
    }

    /// 播放录像音轨（V3.6）。OutputStream 首次播放时创建并保活。
    fn play_audio(&mut self, wav: &Path) {
        let handle = match &self.audio_out {
            Some((_, h)) => h.clone(),
            None => match rodio::OutputStream::try_default() {
                Ok((stream, handle)) => {
                    self.audio_out = Some((stream, handle.clone()));
                    handle
                }
                Err(e) => {
                    self.push_log(format!("音频设备不可用（静音回放）: {e}"));
                    return;
                }
            },
        };
        match rodio::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(wav).expect("音轨文件刚抽出来"),
        )) {
            Ok(source) => match rodio::Sink::try_new(&handle) {
                Ok(sink) => {
                    sink.append(source);
                    self.audio_sink = Some(sink);
                }
                Err(e) => self.push_log(format!("音频播放失败（静音回放）: {e}")),
            },
            Err(e) => self.push_log(format!("音轨解码失败（静音回放）: {e}")),
        }
    }

    /// 停止回放声音（不销毁输出流，下段录像复用）。
    fn stop_audio(&mut self) {
        if let Some(sink) = self.audio_sink.take() {
            sink.stop();
        }
    }

    /// 重扫今日统计（启动与每轮结束后调用）。
    pub fn refresh_stats(&mut self) {
        self.today = Some(report::scan_stats(&self.reports_dir, &report::today_str()));
    }

    /// 重扫报告历史列表（报告中心打开/刷新/一轮结束调用）。
    pub fn refresh_reports(&mut self) {
        self.report_list = report::list_reports(&self.reports_dir, 300);
        self.report_sel = if self
            .report_sel
            .as_ref()
            .is_some_and(|p| self.report_list.contains(p))
        {
            self.report_sel.clone()
        } else {
            self.last_report
                .clone()
                .filter(|p| self.report_list.contains(p))
                .or_else(|| self.report_list.first().cloned())
        };
        self.report_text = None; // 选中可能变化，缓存失效
    }

    /// 读取当前选中报告内容（带缓存，报告中心用）。
    pub fn report_content(&mut self) -> Option<&str> {
        let want = self.report_sel.clone()?;
        if self.report_text.as_ref().is_some_and(|(p, _)| *p == want) {
            return self.report_text.as_ref().map(|(_, t)| t.as_str());
        }
        let text = std::fs::read_to_string(&want).unwrap_or_else(|e| format!("报告读取失败: {e}"));
        // 旧报告可能带控制字符（V3.7 之前未过滤），显示时同样清洗
        let text = report::sanitize(&text);
        self.report_text = Some((want, text));
        self.report_text.as_ref().map(|(_, t)| t.as_str())
    }

    pub fn push_log(&mut self, line: impl Into<String>) {
        // 控制字符（\r、ANSI 转义等）在内嵌 CJK 字体里是豆腐块，滤掉（V3.6）
        let clean: String = line.into().chars().filter(|c| !c.is_control()).collect();
        self.log
            .push_back(format!("{} {}", report::now_hms(), clean));
        while self.log.len() > 200 {
            self.log.pop_front();
        }
    }

    /// 人工应答截止时刻（0 = 不限时，V2.3 超时）。
    fn manual_deadline(&self) -> Option<Instant> {
        let secs = self.config.timeouts.manual_secs;
        (secs > 0).then(|| Instant::now() + Duration::from_secs(secs))
    }

    /// 消费在位监测事件（V3.8）：插入可测（配置可开自动开始）；
    /// 测试中拔出立即走停止路径中止（各项记"设备已拔出"）；多台提示。
    fn drain_dev_events(&mut self) {
        while let Ok(next) = self.dev_rx.try_recv() {
            if next == self.dev {
                continue;
            }
            let was_online = matches!(self.dev, DevState::Online(_));
            // 先落状态再动作：插入自动开始要过 start_run 的在位门槛
            self.dev = next.clone();
            match next {
                DevState::Multiple => {
                    self.push_log("检测到多台设备，请只连接一台被测耳机");
                }
                DevState::Online(sn) => {
                    self.push_log(format!("设备已连接: {sn}"));
                    if !self.busy {
                        // 新机插入（V3.9）：清上一台界面显示再（可选）自动开测
                        self.clear_unit_state();
                        self.push_log("已清空上一台显示");
                        if self.config.conn.auto_start {
                            let items = self.batch_items();
                            if !items.is_empty() {
                                self.push_log("自动开始全测");
                                self.start_run(items);
                            }
                        }
                    }
                }
                DevState::Disconnected => {
                    if was_online {
                        self.push_log("设备已拔出");
                        if self.busy {
                            self.request_stop(StopReason::DeviceGone);
                        }
                    }
                }
            }
        }
    }

    /// 排空 worker 事件并落到界面状态。
    fn drain_events(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        if let Some(rx) = &self.event_rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
        }
        for ev in events {
            self.handle_event(ev, ctx);
        }
    }

    fn handle_event(&mut self, ev: UiEvent, ctx: &egui::Context) {
        match ev {
            UiEvent::Started => {}
            UiEvent::Current(item) => self.current = Some(item),
            UiEvent::Result(r, dev) => {
                if let Some(d) = dev {
                    self.device.merge(d);
                }
                if let Some(p) = &mut self.progress {
                    p.done += 1;
                }
                if r.item == TestItem::Keys {
                    self.keys_prompt = None; // 结果已定，弹窗收起（兜底）
                }
                if matches!(
                    r.item,
                    TestItem::Led | TestItem::MicSpeaker | TestItem::Video | TestItem::Photo
                ) {
                    self.manual = None; // 已答/超时，弹窗收起（兜底）
                }
                if let Some(slot) = self.results.iter_mut().find(|s| s.item == r.item) {
                    *slot = r;
                }
            }
            UiEvent::Manual { title, question } => {
                if let Some(reply) = self.manual_tx.clone() {
                    attention(ctx); // 及时弹窗（V3）：任务栏闪烁提醒操作员
                    self.manual = Some(ManualAsk {
                        title,
                        question,
                        deadline: self.manual_deadline(),
                        reply,
                    });
                }
            }
            UiEvent::KeysUpdate { detected, awaiting } => {
                if let Some(reply) = self.manual_tx.clone() {
                    if awaiting {
                        attention(ctx);
                    }
                    // 任何状态都开窗/更新（V3.5 修复：此前仅 awaiting 开窗，
                    // 检测阶段无事件可发时操作员全程看不到弹窗）。
                    // 收窗由 Result/Finished/停止时兜底。
                    self.keys_prompt = Some(KeysPrompt {
                        detected,
                        awaiting,
                        deadline: self.manual_deadline(),
                        reply,
                    });
                }
            }
            UiEvent::PhotoReady(path) => match load_texture(ctx, &path) {
                Ok((texture, size)) => self.media = MediaState::Photo { texture, size },
                Err(e) => self.push_log(format!("图片解码失败: {e:#}")),
            },
            UiEvent::VideoReady {
                frames_dir,
                count,
                has_audio,
                audio,
            } => {
                // 按目录实际文件名排序加载（V3.6：ffmpeg 图序从 f0001 起，
                // 旧代码按 f0000 起猜名必然落空，媒体区永远空——真机
                // "录像没有回显"的根因）
                let mut names: Vec<PathBuf> = match std::fs::read_dir(&frames_dir) {
                    Ok(rd) => rd
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
                        .collect(),
                    Err(_) => Vec::new(),
                };
                names.sort();
                let mut frames = Vec::new();
                let mut sizes = Vec::new();
                for path in names {
                    match load_texture(ctx, &path) {
                        Ok((t, s)) => {
                            frames.push(t);
                            sizes.push(s);
                        }
                        Err(_) => continue,
                    }
                }
                if frames.is_empty() {
                    self.push_log(format!("视频帧加载失败（期望 {count} 帧）"));
                } else {
                    // 新录像先停上一段的声音，再按需播放本段音轨
                    self.stop_audio();
                    if let Some(wav) = audio {
                        self.play_audio(&wav);
                    }
                    self.media = MediaState::Video {
                        frames,
                        sizes,
                        idx: 0,
                        last: Instant::now(),
                        has_audio,
                    };
                }
            }
            UiEvent::Log(s) => self.push_log(s),
            UiEvent::Finished { report } => {
                self.busy = false;
                self.current = None;
                self.event_rx = None;
                self.manual_tx = None;
                self.progress = None;
                self.keys_prompt = None;
                self.manual = None;
                self.push_log("序列结束");
                self.last_report = report.clone();
                match report {
                    Some(p) => self.push_log(format!("报告: {}", p.display())),
                    None => self.push_log("报告写入失败"),
                }
                self.refresh_stats();
                self.refresh_reports();
            }
        }
    }

    /// 视频回放推进：按抽取帧率循环播放（V3.6 改循环，操作员判定期间
    /// 画面反复滚动，不再播一轮定住）。
    fn tick_video(&mut self, ctx: &egui::Context) {
        if let MediaState::Video {
            frames, idx, last, ..
        } = &mut self.media
        {
            if last.elapsed() >= PLAYBACK_INTERVAL {
                *idx = (*idx + 1) % frames.len().max(1);
                *last = Instant::now();
            }
            ctx.request_repaint();
        }
    }
}

/// 注册中文字体到 egui，并设为 Proportional 家族首位。
fn install_cjk_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("droid-cjk".into(), egui::FontData::from_static(CJK_FONT));
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "droid-cjk".into());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("droid-cjk".into());
    ctx.set_fonts(fonts);
}

/// 解码图片文件为 egui 纹理（等比显示的原始尺寸一并返回）。
fn load_texture(
    ctx: &egui::Context,
    path: &Path,
) -> anyhow::Result<(egui::TextureHandle, [f32; 2])> {
    let img = image::open(path)?;
    let size = [img.width() as f32, img.height() as f32];
    let rgba = img.to_rgba8();
    let pixels = egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        &rgba,
    );
    let texture = ctx.load_texture(
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "media".into()),
        pixels,
        egui::TextureOptions::default(),
    );
    Ok((texture, size))
}

impl eframe::App for E02App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 主题守卫（V3.7）：eframe/系统主题事件可能在启动后改写 visuals
        // （真机 Linux 黑色主题下弹窗/设置出现 egui 默认灰底白字），
        // 发现偏离即重刷；正常帧为一次字段比较，无开销
        if ctx.style().visuals.window_fill != ui::theme::CANVAS {
            ui::theme::apply(ctx);
        }
        if self.busy {
            ctx.request_repaint(); // 测试进行期间持续刷新
        }
        self.drain_dev_events(); // 先处理插拔（拔出要尽快触发中止）
        self.drain_events(ctx);
        self.tick_video(ctx);

        egui::TopBottomPanel::top("top_bar")
            .frame(
                egui::Frame::default()
                    .fill(ui::theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        egui::RichText::new("E02 工厂测试")
                            .strong()
                            .size(18.0)
                            .color(ui::theme::INK),
                    );
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new(format!("工位 {}", self.config.station))
                            .small()
                            .color(ui::theme::MUTED),
                    );
                    if self.busy {
                        if let Some(p) = &self.progress {
                            let frac = p.done as f32 / p.total.max(1) as f32;
                            let elapsed = p.started.elapsed();
                            // V3.7：文字移出条外——ProgressBar 内嵌文字色取
                            // visuals.selection.stroke（我们=珊瑚发丝线），
                            // 画在珊瑚填充上不可读；条只作视觉，数字放旁边
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::ProgressBar::new(frac)
                                        .desired_width(190.0)
                                        .desired_height(10.0)
                                        .fill(ui::theme::PRIMARY),
                                );
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}/{} {} {:02}:{:02}",
                                        p.done,
                                        p.total,
                                        self.current
                                            .map(|i| i.label().to_string())
                                            .unwrap_or_default(),
                                        elapsed.as_secs() / 60,
                                        elapsed.as_secs() % 60
                                    ))
                                    .small()
                                    .color(ui::theme::BODY),
                                );
                            });
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.busy && ui.add(ui::theme::danger("停止测试")).clicked() {
                            self.request_stop(StopReason::User);
                        }
                        if ui.button("报告中心").clicked() {
                            self.refresh_reports();
                            self.report_open = true;
                        }
                        if ui.button("设置").clicked() {
                            self.settings_open = true;
                        }
                        // 设备在位指示（V3.8）：灰=未连接 绿=在线 琥珀=多台
                        // （right_to_left 布局后加的在最左，呈 "● 设备 …" 排列）
                        ui.add_space(10.0);
                        let (dot_color, text) = match &self.dev {
                            DevState::Disconnected => (ui::theme::MUTED, "设备未连接".to_string()),
                            DevState::Online(sn) => (ui::theme::OK, format!("设备 {sn}")),
                            DevState::Multiple => (ui::theme::WARN, "多台设备".to_string()),
                        };
                        ui.label(egui::RichText::new(text).small().color(ui::theme::BODY));
                        ui::theme::dot(ui, dot_color);
                    });
                });
            });
        // 深底日志面板贴底（状态并入日志标题行，去掉独立状态条）
        egui::TopBottomPanel::bottom("log_panel")
            .default_height(140.0)
            .resizable(true)
            .frame(
                egui::Frame::default()
                    .fill(ui::theme::DARK)
                    .inner_margin(egui::Margin::symmetric(16.0, 8.0)),
            )
            .show(ctx, |ui| ui::log_panel::show(ui, self));
        egui::SidePanel::left("test_panel")
            .resizable(false)
            // 固定宽度：PanelState 会跨帧/跨次启动记忆面板实际宽度，
            // 内容一旦收缩会被"锁"在窄宽度上，钉死免受污染
            .exact_width(240.0)
            .frame(
                egui::Frame::default()
                    .fill(ui::theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(14.0, 12.0)),
            )
            .show(ctx, |ui| ui::panel_tests::show(ui, self));
        egui::SidePanel::right("device_panel")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                egui::Frame::default()
                    .fill(ui::theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(14.0, 12.0)),
            )
            .show(ctx, |ui| ui::panel_device::show(ui, self));
        // 媒体区：深底"屏幕井"，拍照/录像内容突出呈现
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(ui::theme::DARK))
            .show(ctx, |ui| ui::panel_media::show(ui, self));
        // 人工判定模态窗（指示灯 / MIC·喇叭）
        ui::dialog_manual::show(ctx, self);
        // 按键引导弹窗（V2.1）
        ui::dialog_keys::show(ctx, self);
        // 设置窗口（V2.2）
        ui::panel_settings::show(ctx, self);
        // 报告中心（V3：历史浏览 + 今日统计）
        ui::dialog_report::show(ctx, self);
    }
}

/// 请求窗口管理器关注本窗口（任务栏闪烁），人工弹窗及时可见（V3）。
fn attention(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
        egui::UserAttentionType::Critical,
    ));
}
