# E02 工厂测试工具跨平台化改造方案

> 原项目：`E02fasctoryTest`（MFC + VLC + CSerialPort，仅 Windows）
> 目标：用 Rust 重写为跨平台（Windows / Linux）工厂测试上位机
> 日期：2026-10-21

---

## 1. 现有程序分析

### 1.1 程序本质

通过 **adb** 与被测设备（E02/T32 相机）通信，执行设备侧 `camera-factory N` 测试命令，
解析输出自动判定，或弹出 MessageBox 由产线员工人工判定，结果用绿/红文字显示。

核心通信链路：`CreateProcess 管道 → adb.exe → shell camera-factory N → stdout 解析`。

### 1.2 测试项清单（与源码逐一对应）

| 命令 | 测试项 | 判定方式 | 原实现 |
|---|---|---|---|
| `camera-factory 1` | 录像 | 人工看视频 | 录制→`adb pull`→libvlc 播放 2.5s |
| `camera-factory 2` | 拍照 | 人工看图 | 拍照→`adb pull`→CImage 缩放显示 |
| `camera-factory 3` | 设备信息 | 自动 | 解析 `address:`/`wifi-mac:`/`soft-ware:` 前缀取固定长度子串 |
| `camera-factory 5` | WiFi | 自动 | 输出含 `FAIL` 即失败 |
| `camera-factory 8` | SD 卡 | 自动 | 输出含 `true` 即成功 |
| `camera-factory 9` | 指示灯 | **人工** | 命令发 3 次，MessageBox YES/NO |
| `camera-factory 10` | 蓝牙 MAC | 自动 | 解析 `MAC:` 开头 12 字符 |
| `camera-factory 11` | 充电器 | 自动 | 含 `Voltage`，取 5 字符电压显示 |
| `camera-factory 12` | 电池 | 自动 | 含 `Battery voltage:`，取百分比 |
| `camera-factory 13` | MIC/喇叭 | **人工** | MessageBox YES/NO |
| `camera-factory 15` | 按键 | 自动 | 输出含 `5`/`6`/`4` 分别判定 |
| 按钮"一键全测" | 全部 | 混合 | 顺序调用以上全部 |

### 1.3 依赖盘点

| 依赖 | 用途 | 实际使用情况 |
|---|---|---|
| MFC (CDialogEx/CImage/CFile…) | 整个 UI | ✅ 核心 |
| libvlc + 125M plugins | 播放测试视频 | ✅ 核心依赖之一 |
| CSerialPort (itas109) | 串口 115200 8N1 | ⚠️ **半成品**：端口枚举/打开代码被注释，读事件回调为空 |
| include/src 的 RealTimeInstruct/HBuffer | 自定义串口协议（10 字节帧） | ⚠️ 仅 `UartCmd()` 一处调用，未见真正使用 |
| jsoncpp (jsonDLL.lib) | JSON | ❌ **完全未使用**（链接了但无代码引用） |
| WinSock (AfxSocketInit) | Socket 客户端 | ❌ **死代码**（消息映射存在，初始化全部被注释） |
| AVPlayer 类 | VLC 完整封装 | ❌ 死代码（Dlg 用的是裸 libvlc + Player 类） |

### 1.4 Windows 专属耦合点（移植必须替换）

1. **MFC 对话框/消息循环** —— 整个 UI 层
2. **CreateProcess + 管道** 执行 adb —— 换跨平台进程调用
3. **libvlc_media_player_set_hwnd** —— HWND 嵌入仅 Windows
4. **CImage 显示 JPG** —— 换跨平台图像库
5. **MessageBox 人工判定**（YES/NO 阻塞 UI 线程）
6. **源码 GBK 编码** —— Rust 强制 UTF-8，中文串需重录
7. **写死的绝对路径**：源码中仍有 `H:\HEC\vc++\VLCplayer\VLCplayer\testvideo.mp4`（`adb pull` 落点依赖旧机器的工作目录）——必须改为程序相对路径

---

## 2. 技术选型

### 2.1 UI 框架对比

| 方案 | 优点 | 缺点 | 结论 |
|---|---|---|---|
| **egui (eframe)** ★推荐 | 纯 Rust、单二进制、即时模式写界面快、图片纹理原生支持、无前端工具链 | 非原生控件外观；视频需自集成 | **主选**：工业上位机常用，12 个按钮+1 个视频窗+几个文本框的规模非常合适 |
| Slint | 声明式 UI、工业/嵌入式定位、性能好 | 需学 .slint DSL；视频嵌入同样要额外做；许可需留意（GPL/商业/桌面免费） | 备选 |
| Tauri | WebView 前端，视频用 HTML5 `<video>` 免 FFI | 引入 Node 前端工具链，二进制+WebView 依赖；团队需前端技能 | 备选（若想要 Web 界面） |
| iced | 纯 Rust、Elm 架构 | 中文输入/成熟度一般 | 不选 |
| fltk-rs | 轻量 | C++ 底座、观感老 | 不选 |

### 2.2 视频播放

| 方案 | 说明 |
|---|---|
| **libmpv render API** ★推荐 | mpv 渲染回调 → egui 纹理逐帧上屏；全平台硬解、解码能力≈VLC、分发体积远小于带 125M plugins 的 VLC。有 `libmpv-rs` 绑定（也可手写 ~30 个符号的 FFI） |
| libvlc FFI | 与现程序引擎一致、行为最接近；但 crate 年久失修需手写绑定，且 Windows 分发要带整套 VLC（libvlc.dll + libvlccore.dll + plugins/） |
| egui-video (ffmpeg-next) | 纯 Rust 集成；成熟度一般，解码器要自己编 |

> 测试场景只是"确认录像能正常播放数秒"，libmpv 完全够用且最省事。

### 2.3 其余组件

| 需求 | 选型 | 说明 |
|---|---|---|
| 执行 adb | `std::process::Command` | adb CLI 全平台行为一致；测试机上本就装有 adb |
| JPG 解码显示 | `image` crate | 解码为 `ColorImage` → egui 纹理，含等比缩放逻辑 |
| 串口（如需） | `serialport` crate | 维护良好；`RealTimeInstruct` 10 字节帧协议用 Rust 重写（量很小） |
| JSON/配置 | `serde` + `serde_json` | 替换 jsoncpp（虽然现在没用到，做配置文件时会用） |
| 人工判定弹窗 | egui 模态 Window | 替代 MessageBox，产线键盘/鼠标/触屏操作 |
| 线程模型 | worker 线程 + `std::sync::mpsc` | adb 调用阻塞式跑在后台线程，UI 通过 channel 收状态，不卡界面 |
| 字体 | 内嵌思源黑体等 | egui 默认不带中文，需注册字体 |
| 日志 | `tracing` + 滚动文件 | 便于产线追溯 |
| 打包 | cargo + (Windows) NSIS/wix，GitLab CI | 后续做 |

### 2.4 推荐依赖清单（Cargo.toml 草案）

```toml
[dependencies]
eframe = "0.29"            # egui 框架
image = { version = "0.25", default-features = false, features = ["jpeg", "png"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"               # 错误处理
tracing = "0.1"
tracing-appender = "0.2"
serialport = "4"           # 可选：串口阶段启用
libmpv = { package = "libmpv2", version = "4" }  # 视频阶段启用

[target.'cfg(windows)'.build-dependencies]
# 如需嵌入图标: winresource = "0.1"
```

---

## 3. 新项目架构

```
e02-factory-rs/
├── Cargo.toml
├── assets/
│   ├── fonts/             # 中文字体（内嵌进二进制）
│   └── icon.png
├── media/                 # 运行期工作目录：pull 下来的 testjpeg.jpg / testvideo.mp4（.gitignore）
├── docs/
│   └── MIGRATION_PLAN.md  # 本文档
└── src/
    ├── main.rs            # 入口：加载字体/日志/工作目录
    ├── app.rs             # egui App：聚合状态、每帧驱动
    ├── ui/
    │   ├── mod.rs         # 主布局：左侧测试按钮 / 中间媒体区 / 右侧设备信息+结果
    │   ├── panel_tests.rs # 12 个测试项按钮 + 全测进度
    │   ├── panel_media.rs # 图片/视频显示区（egui 纹理 + libmpv 画面）
    │   ├── panel_device.rs# SN / WiFi MAC / BT MAC / 版本 / 电池 只读字段
    │   └── dialog_manual.rs # 人工判定模态（指示灯、MIC/喇叭）
    ├── adb.rs             # adb 封装：shell(cmd) -> String、pull(remote, local)、设备在线检测
    ├── protocol.rs        # camera-factory 命令号枚举 + 各命令输出解析器（原 Find/Mid 逻辑）
    ├── engine/
    │   ├── mod.rs         # TestItem trait + TestStatus；测试编排器（线程）
    │   └── items.rs       # 12 个测试项实现（自动判定/人工判定/媒体展示）
    ├── player.rs          # libmpv render API 封装 → egui 纹理
    ├── report.rs          # 测试结果落盘（T32.txt 风格 / JSON，附时间戳）
    └── serial/            # 可选阶段：serialport + RealTimeInstruct 协议移植
```

### 数据流

```
[UI 线程 egui] --命令请求 channel--> [worker 线程]
      ^                                  |
      |            adb shell camera-factory N
      |                                  v
      +--------状态/结果 channel-------- 解析/判定
                (TestStatus / 媒体文件路径 / 人工判定请求)
```

人工判定的同步化：worker 发出 `PendingManual{question}` 后阻塞在应答 channel 上，
UI 弹模态窗，员工点"合格/不合格"把结果写回应答 channel，worker 继续跑——
复刻原程序 MessageBox 的同步语义，但跨平台。

### 状态模型

```rust
enum Verdict { Pass, Fail, Pending, ManualYes, ManualNo, Skipped }

struct TestResult {
    item: TestItem,        // Video, Camera, DeviceInfo, Wifi, Sdcard, Light,
                           // BtMac, Charger, Battery, MicSpeaker, Keys
    verdict: Verdict,
    detail: String,        // 电压值、百分比、失败原因等
    at: std::time::SystemTime,
}

struct DeviceInfo {                        // 对应原 Deviceinfo 结构
    sn: String, wifi_mac: String, bt_mac: String,
    t32_ver: String, battery: String,
}
```

---

## 4. 分阶段实施计划

| 阶段 | 内容 | 验收标准 |
|---|---|---|
| **M0 骨架** | cargo init、依赖、字体、布局空壳 | 两个平台编译通过，窗口显示中文 |
| **M1 核心无 UI** | `adb.rs` + `protocol.rs` + 解析器 + CLI 入口 | 不开界面即可对真机跑单项测试并打印判定（回归对照原程序行为） |
| **M2 UI + 自动判定项** | 按钮面板、设备信息、结果绿/红显示、信息/SD/电池/充电器/WiFi/按键测试 | Windows/Linux 上单项自动判定全通过 |
| **M3 拍照显示** | image 解码 + 等比缩放上屏 | 拍照测试可见图片 |
| **M4 视频播放** | libmpv render → egui 纹理；播放数秒自动停止 | 录像测试正常播放（对照原 2.5 秒逻辑改为可配置） |
| **M5 人工判定 + 全测 + 报告** | 模态判定、一键全测编排、结果落盘（JSON+文本）、日志 | 全流程与原程序等效 |
| **M6 打包与 CI** | Windows 下Release 单 exe + mpv 依赖携带；GitLab CI 编译两平台 | 产线可部署 |

**先做 Windows、Linux 跟随**（egui/libmpv/adb 全跨平台，M2 起 Linux 顺手验证即可）。

## 5. 风险与对策

| 风险 | 对策 |
|---|---|
| libmpv Windows 分发 | 随程序带 mpv-2.dll + mpv 编解码依赖（约 40–70M），远小于 VLC 125M；或回退 libvlc FFI 方案 |
| 解析器行为差异导致误判 | M1 阶段用真机输出做对照测试（把 T32.txt 样例做成单元测试） |
| 人工判定线程同步 | channel 暂停/恢复语义，先写集成测试再接 UI |
| 产线习惯改变 | UI 布局按原对话框 1:1 排布，按钮文字沿用原中文 |
| 串口/Socket 死代码 | 默认**不移植**；若后续产线确需串口指令，`serial/` 模块按 RealTimeInstruct 协议补齐 |

## 6. 明确不移植的内容

- jsoncpp 链接（未使用）
- WinSock 客户端（死代码）
- AVPlayer 类（死代码）
- 拖放播放列表 / IDC_BUTTON1 播放器残留（死代码）
- 串口半成品（默认不移植，需要时另起阶段）
