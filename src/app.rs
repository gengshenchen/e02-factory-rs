//! 应用状态与窗口装配：worker 事件消费、媒体显示、人工判定应答。

use crate::config::{self, Config};
use crate::engine::events::{UiEvent, UiResponse};
use crate::engine::worker;
use crate::engine::{TestItem, TestResult};
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
const PLAYBACK_INTERVAL: Duration = Duration::from_millis(500);

/// 人工判定待应答请求（UI 弹窗收集应答后经 reply 传回 worker）。
pub struct ManualAsk {
    pub title: String,
    pub question: String,
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
        /// 播完一轮后定帧，停止重绘省 CPU
        done: bool,
    },
}

/// 应用全局状态。
pub struct E02App {
    /// 被测设备信息（原 Deviceinfo 结构）
    pub device: DeviceInfo,
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
    progress: Option<Progress>,
    /// 按键引导弹窗；无按键测试进行时 None
    pub keys_prompt: Option<KeysPrompt>,
    /// 工位配置（V2.2）
    pub config: Config,
    /// 设置窗口开关
    pub settings_open: bool,
    event_rx: Option<Receiver<UiEvent>>,
    manual_tx: Option<Sender<UiResponse>>,
}

impl E02App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        apply_style(cc);
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
            event_rx: None,
            manual_tx: None,
        };
        if let Some(msg) = cfg_err {
            app.push_log(msg);
        }
        app.push_log(format!(
            "工位: {}（启用 {} 项）",
            app.config.station,
            app.config.active_items().len()
        ));
        app
    }

    /// 一键全测项集：当前工位启用项，按原程序批次顺序执行。
    pub fn batch_items(&self) -> Vec<TestItem> {
        let active = self.config.active_items();
        worker::BATCH_ORDER
            .into_iter()
            .filter(|i| active.contains(i))
            .collect()
    }

    /// 启动测试序列（单项或全测）；busy 期间忽略重复触发。
    pub fn start_run(&mut self, items: Vec<TestItem>) {
        if self.busy || items.is_empty() {
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
        let media = PathBuf::from("media");
        let reports = PathBuf::from("reports");
        let n = items.len();
        let cfg = self.config.clone();
        thread::spawn(move || {
            let report = worker::run_sequence(&items, &tx, &mrx, &media, &reports, &cfg);
            let _ = tx.send(UiEvent::Finished { report });
        });
        self.push_log(format!("开始测试，共 {n} 项"));
    }

    pub fn push_log(&mut self, line: impl Into<String>) {
        self.log
            .push_back(format!("{} {}", report::now_hms(), line.into()));
        while self.log.len() > 200 {
            self.log.pop_front();
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
                if let Some(slot) = self.results.iter_mut().find(|s| s.item == r.item) {
                    *slot = r;
                }
            }
            UiEvent::Manual { item, question } => {
                if let Some(reply) = self.manual_tx.clone() {
                    self.manual = Some(ManualAsk {
                        title: item.label().to_string(),
                        question,
                        reply,
                    });
                }
            }
            UiEvent::KeysUpdate { detected, awaiting } => {
                if let Some(reply) = self.manual_tx.clone() {
                    if awaiting {
                        self.keys_prompt = Some(KeysPrompt {
                            detected,
                            awaiting: true,
                            reply,
                        });
                    } else {
                        // 收齐自动通过：弹窗即收
                        self.keys_prompt = None;
                    }
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
            } => {
                let mut frames = Vec::new();
                let mut sizes = Vec::new();
                for i in 0..count {
                    match load_texture(ctx, &frames_dir.join(format!("f{i:04}.png"))) {
                        Ok((t, s)) => {
                            frames.push(t);
                            sizes.push(s);
                        }
                        Err(_) => break,
                    }
                }
                if frames.is_empty() {
                    self.push_log("视频帧加载失败");
                } else {
                    self.media = MediaState::Video {
                        frames,
                        sizes,
                        idx: 0,
                        last: Instant::now(),
                        has_audio,
                        done: false,
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
                self.push_log("序列结束");
                match report {
                    Some(p) => self.push_log(format!("报告: {}", p.display())),
                    None => self.push_log("报告写入失败"),
                }
            }
        }
    }

    /// 视频回放推进：2fps 前进，播完一轮定帧。
    fn tick_video(&mut self, ctx: &egui::Context) {
        let playing = if let MediaState::Video {
            frames,
            idx,
            last,
            done,
            ..
        } = &mut self.media
        {
            if !*done && last.elapsed() >= PLAYBACK_INTERVAL {
                if *idx + 1 >= frames.len() {
                    *done = true;
                } else {
                    *idx += 1;
                }
                *last = Instant::now();
            }
            !*done
        } else {
            false
        };
        if playing {
            ctx.request_repaint();
        }
    }
}

/// V2.1：默认放大显示——像素密度取系统值与 1.25 的较大者（产线屏幕观看距离远），
/// 正文字号 14→16；Ctrl+滚轮缩放仍可用。
fn apply_style(cc: &eframe::CreationContext<'_>) {
    // zoom_factor 与 Ctrl+滚轮缩放同一机制：只抬基线，不锁死
    let native = cc.egui_ctx.native_pixels_per_point().unwrap_or(1.0);
    let target = 1.25_f32;
    if native < target {
        cc.egui_ctx.set_zoom_factor(target / native);
    }
    let mut style = (*cc.egui_ctx.style()).clone();
    style.text_styles = [
        (egui::TextStyle::Small, egui::FontId::proportional(13.0)),
        (egui::TextStyle::Body, egui::FontId::proportional(16.0)),
        (egui::TextStyle::Button, egui::FontId::proportional(16.0)),
        (egui::TextStyle::Monospace, egui::FontId::monospace(14.0)),
        (egui::TextStyle::Heading, egui::FontId::proportional(22.0)),
    ]
    .into();
    cc.egui_ctx.set_style(style);
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
        if self.busy {
            ctx.request_repaint(); // 测试进行期间持续刷新
        }
        self.drain_events(ctx);
        self.tick_video(ctx);

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.heading("E02 工厂测试工具");
                ui.label(format!("工位: {}", self.config.station));
                if self.busy {
                    if let Some(p) = &self.progress {
                        let frac = p.done as f32 / p.total.max(1) as f32;
                        let elapsed = p.started.elapsed();
                        let text = format!(
                            "{}/{} · {} · {:02}:{:02}",
                            p.done,
                            p.total,
                            self.current
                                .map(|i| i.label().to_string())
                                .unwrap_or_default(),
                            elapsed.as_secs() / 60,
                            elapsed.as_secs() % 60
                        );
                        ui.add(egui::ProgressBar::new(frac).text(text).desired_width(360.0));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ 设置").clicked() {
                        self.settings_open = true;
                    }
                });
            });
        });
        // 先注册的在最外沿：状态条贴底，日志面板在状态条上方
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.busy {
                    ui.spinner();
                    ui.label(format!(
                        "正在测试：{}",
                        self.current
                            .map(|i| i.label().to_string())
                            .unwrap_or_default()
                    ));
                } else {
                    ui.label("就绪");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(last) = self.log.back() {
                        ui.small(last);
                    }
                });
            });
        });
        egui::TopBottomPanel::bottom("log_panel")
            .default_height(140.0)
            .resizable(true)
            .show(ctx, |ui| ui::log_panel::show(ui, self));
        egui::SidePanel::left("test_panel")
            .resizable(false)
            .default_width(216.0)
            .show(ctx, |ui| ui::panel_tests::show(ui, self));
        egui::SidePanel::right("device_panel")
            .resizable(false)
            .default_width(300.0)
            .show(ctx, |ui| ui::panel_device::show(ui, self));
        egui::CentralPanel::default().show(ctx, |ui| ui::panel_media::show(ui, self));
        // 人工判定模态窗（指示灯 / MIC·喇叭）
        ui::dialog_manual::show(ctx, self);
        // 按键引导弹窗（V2.1）
        ui::dialog_keys::show(ctx, self);
        // 设置窗口（V2.2）
        ui::panel_settings::show(ctx, self);
    }
}
