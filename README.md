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

运行期依赖：`adb` 在 PATH 中（或用环境变量 `ADB` 指定路径）；录像测试另需 `ffmpeg` / `ffprobe`（或用环境变量 `FFMPEG` / `FFPROBE` 指定路径）；Linux 无其他系统库依赖。

## CLI（M1）

无界面对真机跑自动判定测试，产线可脚本化：

```bash
e02-factory-rs list                # 测试项清单
e02-factory-rs devices             # adb 在线设备
e02-factory-rs test info wifi      # 跑指定测试项（空格分隔）
e02-factory-rs all --report r.txt  # 全部自动判定项 + 文本报告
```

人工判定项（指示灯/MIC·喇叭）与媒体项（录像/拍照）在 CLI 下标记为跳过，请在 GUI 中执行。退出码：有 FAIL 为 1。

## GUI（M2–M5）

无参数启动图形界面：左侧测试项面板（一键全测 / 单项触发）、中央媒体区（拍照图片等比显示；录像经 ffmpeg 全程解码验证后 2fps 抽帧回放）、右侧设备信息与逐项判定（绿 PASS / 红 FAIL）。指示灯 / MIC·喇叭测试弹模态窗人工判定（对应原 MessageBox）；全测结束自动写报告到 `reports/e02-<UTC 时间戳>.txt`，拉回的媒体文件保存在 `media/`。

## CI / 发布

- 推送任意分支：GitHub Actions 在 Ubuntu / Windows / macOS 上跑 `fmt + clippy + test + release build`
- 推送 `v*` 标签：自动构建各平台安装包并发布到 GitHub Releases
  - Windows：zip（裸 exe）+ MSI（cargo-wix）
  - Linux：tar.gz + deb（cargo-deb）

```bash
git tag v0.1.0 && git push origin v0.1.0
```

## 产线部署

安装包来自 GitHub Releases（tag 构建）。部署到产线电脑后需预装：

| 依赖 | 说明 |
|---|---|
| adb | Android platform-tools，置于 PATH；或设环境变量 `ADB` 指向绝对路径 |
| ffmpeg / ffprobe | 仅录像测试需要；置于 PATH 或设 `FFMPEG` / `FFPROBE` |
| USB 驱动 | Windows 需安装设备 ADB 驱动（通常随 platform-tools 或厂商驱动） |

首次运行在工作目录生成 `media/`（拉回的照片/视频）与 `reports/`（测试报告），换台设备重新打开即可，报告按 UTC 时间戳归档。

## 进度

- [x] M0 骨架：窗口 + 中文字体 + 布局
- [x] M1 无 UI 核心：adb 层 + 协议解析 + CLI（真机回归待验证）
- [x] M2 UI + 自动判定项
- [x] M3 拍照图片显示
- [x] M4 视频验证（ffmpeg sidecar 抽帧解码，替代 libmpv，见迁移方案）
- [x] M5 人工判定 + 一键全测 + 报告
- [ ] M6 打包完善与产线部署
  - [x] CI / Release 流水线（M0 已建：三平台测试 + tag 自动出 zip / MSI / deb / tar.gz）
  - [x] 应用图标（assets/icon.png 窗口图标；icon.ico 留作 MSI 品牌化）
  - [ ] MSI 开始菜单快捷方式（需在 Windows 上用 cargo-wix 定制 main.wxs）
  - [ ] ffmpeg 分发方式定稿：随包携带（+~100M）或产线预装置于 PATH（当前按预装）
  - [ ] 安装包元数据定稿（license / 维护者信息）
  - [ ] 真机回归（adb + E02 设备）与产线试运行
