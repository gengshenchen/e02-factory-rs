# e02-factory-rs

E02（T32 相机）产线工厂测试上位机的跨平台重写版（Windows / Linux）。

原 MFC 版本见旧仓库 `E02fasctoryTest`；移植分析与完整方案见 [docs/MIGRATION_PLAN.md](docs/MIGRATION_PLAN.md)。

## 功能（对应原程序 camera-factory N）

| 测试项 | 命令 | 判定 |
|---|---|---|
| 录像 | `camera-factory 1` | 拉回视频播放，人工确认 |
| 拍照 | `camera-factory 2` | 拉回图片显示，人工确认 |
| 设备信息 | `camera-factory 3 / 10` | 自动解析 SN / WiFi MAC / BT MAC / 版本 |
| WiFi / SD 卡 / 充电 / 电池 / 按键 | `5 / 8 / 11 / 12 / 15` | 自动解析输出 |
| 指示灯 / MIC·喇叭 | `9 / 13` | 弹窗人工判定 |

## 构建

依赖：Rust stable（rust-toolchain.toml 已固定）。

```bash
cargo build --release        # 开发调试用 cargo run
cargo test                   # 解析器等单元测试（无需真机）
```

运行期依赖：`adb` 在 PATH 中；Linux 无其他系统库依赖。

## CI / 发布

- 推送任意分支：GitHub Actions 在 Ubuntu / Windows / macOS 上跑 `fmt + clippy + test + release build`
- 推送 `v*` 标签：自动构建各平台安装包并发布到 GitHub Releases
  - Windows：zip（裸 exe）+ MSI（cargo-wix）
  - Linux：tar.gz + deb（cargo-deb）

```bash
git tag v0.1.0 && git push origin v0.1.0
```

## 进度

- [x] M0 骨架：窗口 + 中文字体 + 布局（本仓库当前状态）
- [ ] M1 无 UI 核心：adb 层 + 协议解析 + CLI
- [ ] M2 UI + 自动判定项
- [ ] M3 拍照图片显示
- [ ] M4 视频播放（libmpv）
- [ ] M5 人工判定 + 一键全测 + 报告
- [ ] M6 打包完善与产线部署
