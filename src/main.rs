//! E02 工厂测试工具（跨平台重写版）入口。
//!
//! GUI 三栏布局（测试项/媒体/设备信息）+ 无参启动 CLI 模式。

mod adb;
mod app;
mod cli;
mod engine;
mod player;
mod protocol;
mod report;
mod ui;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    tracing::info!("E02 工厂测试工具启动");

    // 有命令行参数 → CLI 模式（M1）；无参数 → 图形界面
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        std::process::exit(cli::run(&args));
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "E02 工厂测试工具",
        options,
        Box::new(|cc| Ok(Box::new(app::E02App::new(cc)))),
    )
}
