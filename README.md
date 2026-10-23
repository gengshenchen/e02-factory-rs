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

运行期依赖：`adb` 在 PATH 中（或用环境变量 `ADB` 指定路径）；录像测试需要 `ffmpeg` / `ffprobe`，WiFi 深度吞吐测试需要 PC 侧 `iperf3`——zip / MSI 安装包均已随包附带（与程序同目录自动识别），其他安装方式需置于 PATH 或用环境变量 `FFMPEG` / `FFPROBE` / `IPERF3` 指定路径；Linux 无其他系统库依赖。

## CLI（M1）

无界面对真机跑自动判定测试，产线可脚本化：

```bash
e02-factory-rs list                # 测试项清单
e02-factory-rs devices             # adb 在线设备
e02-factory-rs test info wifi      # 跑指定测试项（空格分隔）
e02-factory-rs all --report r.txt  # 全部自动判定项 + 文本报告
e02-factory-rs all --profile module # 按"模组工位"档案执行
e02-factory-rs config > config.toml # 打印生效配置（可重定向为模板）
```

人工判定项（指示灯/MIC·喇叭）与媒体项（录像/拍照）在 CLI 下标记为跳过，请在 GUI 中执行。退出码：有 FAIL 为 1。

## 工位配置（V2.2）

exe 同目录 `config.toml`（缺失用内置默认；GUI "⚙ 设置" 可视化编辑并保存）。要点：

- **工位档案**：`module`（模组工位：info/btmac/wifi/sdcard/photo/led 浅测）、`complete`（整机工位：全项）；顶栏/设置页切换，左栏只启用当前工位的项
- **WiFi 深度**：`driver`（camera-factory 5 驱动通即过）/ `throughput`（追加 ping 网关统计丢包率与 avg RTT，阈值可配）
- **按键**：必测键集合（4=多功能 5=开机 6=音量）可裁剪；strict 解析只认独立数字行
- **陀螺仪**：无 camera-factory 命令，默认 `dumpsys sensorservice` 采样两次比数值；固件支持后改 `cmd` 即可
- **超时**：`timeouts.item_secs`（单项命令，默认 60s，超时强杀进程判 FAIL 并继续）、`timeouts.manual_secs`（人工判定/按键应答等待，默认 60s，超时自动判不合格），0 = 不限制
- **报告**：数据目录 `reports/` 下每测一份 `e02-<本地时间>-<SN>.txt`，逐项覆盖写盘（重测/中途异常都保留最新），SN 中途解析到后自动改名

数据目录 = exe 同目录（可写时，绿色包习惯）；MSI / deb 安装到系统目录后自动退回用户数据目录（Windows `%APPDATA%\e02-factory-rs`，Linux `~/.local/share/e02-factory-rs`），config.toml 同目录。

## GUI（M2–M5）

无参数启动图形界面：左侧测试项面板（一键全测 / 单项触发）、中央媒体区（拍照图片等比显示；录像经 ffmpeg 全程解码验证后 2fps 抽帧回放）、右侧设备信息与逐项判定（绿 PASS / 红 FAIL）。

- **按键实时回显**（V3.1）：固件监听窗口内每按一键弹窗立即点亮"已按下"（流式读 adb 输出，原 MFC 版要等命令结束才显示）
- **自动项先行、人工项殿后**（V3）：自动判定项全部跑完后才进入指示灯 / MIC / 按键等人工项，操作员可在自动项期间离手；进入人工段弹窗并任务栏闪烁提醒
- **可停止**（V3）：测试进行中顶栏"停止测试"按钮——当前命令立即强杀、当前项与剩余项记"手动结束测试"跳过，报告照常落盘
- **报告中心**（V3）：顶栏"查看报告"打开历史报告浏览（左列表右内容），顶栏实时显示"今日 N 台: PASS x / FAIL y"；报告含整机总判定（结果: PASS/FAIL）、工位/耗时表头、每项固定一行（带单项耗时）与汇总行
- 指示灯 / MIC·喇叭测试弹模态窗人工判定（对应原 MessageBox）；拉回的媒体文件保存在数据目录 `media/`

## CI / 发布

- 推送任意分支：GitHub Actions 在 Ubuntu / Windows / macOS 上跑 `fmt + clippy + test + release build`
- 推送 `v*` 标签：自动构建各平台安装包并发布到 GitHub Releases
  - Windows：zip（exe + 随包 ffmpeg/ffprobe/iperf3）+ MSI（同 zip 内容，另自动加 iperf3 防火墙入站规则）
  - Linux：tar.gz（含 ffmpeg/ffprobe）+ deb（安装时自动依赖 ffmpeg）

```bash
git tag v0.1.0 && git push origin v0.1.0
```

## 产线部署

安装包来自 GitHub Releases（tag 构建）。部署到产线电脑后需预装：

| 依赖 | 说明 |
|---|---|
| adb | Android platform-tools，置于 PATH；或设环境变量 `ADB` 指向绝对路径 |
| ffmpeg / ffprobe | 仅录像测试需要。zip / tar.gz 已随包附带（解压即用，程序自动识别同目录）；deb 安装时自动装；MSI v0.3.1 起随包附带 |
| iperf3 | 仅 WiFi 深度吞吐测试需要（PC 侧）。zip / MSI 已随包附带；自备版本设环境变量 `IPERF3` 指向路径。**zip 用户需手动加防火墙规则**（MSI 安装时已自动加，zip 没有安装过程）：管理员运行 `netsh advfirewall firewall add rule name="E02FactoryTest iperf3" dir=in action=allow program="<zip解压目录>\iperf3.exe" enable=yes`，否则吞吐测试会被防火墙拦下 |
| USB 驱动 | Windows 需安装设备 ADB 驱动（通常随 platform-tools 或厂商驱动） |

首次运行在数据目录生成 `media/`（拉回的照片/视频）与 `reports/`（每测一份 txt 报告，文件名带时间与 SN），换台设备重新打开即可。

## V2 产线反馈改进（方案见 [docs/V2_PLAN.md](docs/V2_PLAN.md)）

- [x] V2.1 按键引导式流程（弹窗按键回显/收齐自动过/未收齐人工改判）+ UI 优化（字号放大、顶栏进度条、底部实时日志）
- [x] V2.2 配置页 + 模组/整机工位档案 + WiFi 测试深度（驱动/速率）+ 陀螺仪项（通用自定义项框架待固件新增测试项后按需实现）
- [x] V2.3 报告增强 + 超时加固（单项命令/人工应答超时、报告边测边写）
- [x] V3 自动/手动分组先行执行、停止测试、报告中心（今日统计 + 历史浏览 + SN 文件名 + 整机总判定）、单 txt 报告、数据目录回退、MSI 桌面/开始菜单快捷方式

## 进度

- [x] M0 骨架：窗口 + 中文字体 + 布局
- [x] M1 无 UI 核心：adb 层 + 协议解析 + CLI（真机回归待验证）
- [x] M2 UI + 自动判定项
- [x] M3 拍照图片显示
- [x] M4 视频验证（ffmpeg sidecar 抽帧解码，替代 libmpv，见迁移方案）
- [x] M5 人工判定 + 一键全测 + 报告
- [x] M6 打包完善与产线部署
  - [x] CI / Release 流水线（M0 已建：三平台测试 + tag 自动出 zip / MSI / deb / tar.gz）
  - [x] 应用图标（assets/icon.png 窗口图标；icon.ico 已用于 MSI/快捷方式）
  - [x] MSI 桌面 + 开始菜单快捷方式（wix/main.wxs 定制，版本号跟 Cargo.toml）——安装效果待 Windows 真机验证
  - [x] ffmpeg 随包分发：zip/tar.gz 附带 LGPL 静态版（BtbN n8.1，与 exe 同目录自动识别）；deb 声明 apt 依赖；MSI v0.3.1 起随包附带
  - [x] iperf3 随包分发（WiFi 吞吐测试）：tools-build 工作流 mingw 交叉编译 ESnet 3.17.1 静态版（sha256 钉死）→ Windows runner 冒烟 → 提交 `tools/iperf3-win64.exe` 进仓库，zip / MSI 取用；MSI 安装期自动加防火墙入站规则
  - [ ] 安装包元数据定稿（license / 维护者信息）
  - [ ] 真机回归（adb + E02 设备）与产线试运行
