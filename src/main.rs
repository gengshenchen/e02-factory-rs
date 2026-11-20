//! E02 工厂测试工具（跨平台重写版）入口。
//!
//! GUI 三栏布局（测试项/媒体/设备信息）+ 无参启动 CLI 模式。

// Windows release 构建用 GUI 子系统（V3.15）：MSI 安装后双击快捷方式
// 不再弹终端。debug 构建保留控制台便于开发；CLI 子命令在 main 开头
// AttachConsole 挂回调用方终端，list/test 输出不受影响。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adb;
mod app;
mod cli;
mod config;
mod devwatch;
mod engine;
mod exec;
mod ledger;
mod netip;
mod paths;
mod player;
mod protocol;
mod report;
mod ui;
// 仅 release 需要（GUI 子系统无控制台，CLI 要挂回终端）；debug 保留
// 控制台，模块不编入（否则 debug 构建视其为死代码）
#[cfg(all(windows, not(debug_assertions)))]
mod win_console;

fn main() -> eframe::Result<()> {
    // CLI 子命令挂回父终端（须在任何 stdout/stderr 使用前，std 首用即
    // 绑定标准句柄）；GUI 直启（无参数）不弹终端也不挂控制台
    #[cfg(all(windows, not(debug_assertions)))]
    if std::env::args_os().nth(1).is_some() {
        win_console::attach_parent();
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr) // CLI 输出走 stdout，日志勿混入
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
            .with_min_inner_size([960.0, 600.0])
            .with_icon(load_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "E02 工厂测试工具",
        options,
        Box::new(|cc| Ok(Box::new(app::E02App::new(cc)))),
    )
}

/// 应用图标（assets/icon.png 内嵌，编译期保证存在）。
fn load_icon() -> egui::IconData {
    let img =
        image::load_from_memory(include_bytes!("../assets/icon.png")).expect("内嵌图标解码失败");
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    egui::IconData {
        rgba: rgba.into_raw(),
        width: w,
        height: h,
    }
}
