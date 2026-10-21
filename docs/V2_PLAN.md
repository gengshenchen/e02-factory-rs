# V2 改进方案（产线反馈）

来源：2026-10-21 产线反馈 5 点（UI 体验、工位配置、报告、按键测试失败、陀螺仪缺失）。
按优先级拆三个批次：V2.1 修复+体验（当天可完成）、V2.2 配置与工位（核心新功能）、V2.3 报告与陀螺仪。

## 1. 按键测试失败 —— 原因分析（先修，属 bug）

**原版行为**（`OnBnClickedButton1Key`）：执行 `camera-factory 15`，在输出里找字符
`5`（开机键）/`6`（音量键）/`4`（多功能键）；找到哪个就打印哪个键 PASS，
**一个都没找到时什么都不打印**——原版把"无人按键"静默吞掉了。

**我们的移植**：`keys.is_empty() → Verdict::Fail`（比原版严格，见 runner.rs）。
一键全测时没人守着按键，固件的监听窗口内无按键输出 → 必然 FAIL。

**结论**：按键是**半自动测试**（需要操作员在监听窗口内按键），不该当纯自动项跑。

### 修复方案（V2.1）

1. 改为**引导式流程**（类似指示灯/MIC）：
   - 弹非阻塞提示窗"请依次按下：开机 / 音量 / 多功能 键"，实时回显已检测到的键；
   - worker 侧带超时轮询（`listen_secs`，默认 10s，可配）；
   - 收齐"必测键集合"（默认 4/5/6 三个，工位可裁剪）→ 自动 PASS；
   - 超时未收齐 → 显示缺哪些键，操作员可选"强制通过/不通过"（与产线确认默认值）。
2. 解析加严（可选开关，默认兼容原版 `contains`）：
   strict 模式只认**独立数字行**（`^\s*[456]\s*$`），避免版本号"1.0.**5**"、
   电压"**4**.2V"这类假阳性——原版同样有此隐患，只是被静默行为掩盖。
3. 原版细节差异记录：MFC `Find("5") > 0` 要求匹配位置 > 0，我们的 `contains` 含位置 0，
   对真实输出（有多行前缀）无行为差异，不单独处理。

## 2. UI 优化（V2.1）

- **字号**：默认 `pixels_per_point ≥ 1.25`（取系统值与 1.25 的较大者），
  保留 egui Ctrl+滚轮缩放；正文字号 14→16，面板标签 18。
- **进度显示**：顶栏 `ProgressBar`（已完成/总数）+ 当前项名 + 已耗时；
  worker 已有 `Current/Result` 事件，UI 侧补计数即可。
- **实时日志**：底部日志面板（等宽小字、时间戳、自动滚动、保留 200 条）。
  重新引入 `UiEvent::Log`（M5 清死代码时删过 `Note`，当时确实无发送方；
  现在 worker 每步发：发送的 adb 命令、输出首行摘要、判定原因、耗时）。
- 状态栏保留最后一行日志，与日志面板同源。

## 3. 测试项配置页 + 工位档案（V2.2，核心）

两个工位需求：**模组工位**（无整机，浅测：如 WiFi 只验驱动）、**整机工位**（深测：
如 WiFi 还要速率）。

### 配置模型（`config.toml`，serde 双向）

```toml
# 顶栏下拉选择当前工位
station = "module"                     # module | complete

[profile.module]                       # 模组工位：浅测
items = ["info", "btmac", "wifi", "sdcard", "photo", "led"]

[profile.complete]                     # 整机工位：全项深测
items = ["info", "btmac", "wifi", "sdcard", "charger", "battery",
         "keys", "photo", "video", "led", "mic", "gyro"]

[item.wifi]
depth = "driver"                       # driver | throughput；complete 工位配 throughput

[item.wifi.throughput]                 # depth = throughput 时生效
method = "ping"                        # ping（默认，普适） | iperf3（设备端需预装）
ping_count = 20
loss_max_pct = 0.0
rtt_max_ms = 100

[item.keys]
listen_secs = 10
required = [4, 5, 6]                   # 模组工位可裁剪

[item.gyro]
# 固件暂无 camera-factory 命令：先用通用 shell 采样，固件加上后改 cmd 即可
cmd = "dumpsys sensorservice | grep -A2 Gyroscope"
# 通过判定：两次采样数值有变化（芯片活着且在输出数据）
```

- 配置文件优先级：exe 同目录 `config.toml` > 内置默认；设置页可视化编辑（勾选测试项、
  每项展开深度参数），保存即写回。
- **自定义测试项框架**：`[custom.<name>]` = 任意 `cmd` + `pass_regex` + `fail_regex` +
  是否人工确认。陀螺仪先用内置定义走此框架，固件支持后无需改代码换命令。

### WiFi 速率实现（depth = throughput）

- `ping` 法（默认）：`adb shell ip route` 取网关 → `adb shell ping -c N 网关`，
  解析丢包率与 avg RTT，超阈值 FAIL；detail 记录原始统计行。
- `iperf3` 法（可选）：`adb forward tcp:5201` + 设备端 `iperf3 -s`，宿主机打流测 Mbps，
  阈值 `min_mbps` 可配。设备端未预装 iperf3 的产线用 ping 法即可。
- 深度为 driver 时保持现状（`camera-factory 5` 不含 FAIL 即过）。

## 4. 报告系统增强（V2.3）

现状只有 T32 风格文本，且未测/跳过项不落行。增强为：

```text
E02 工厂测试报告
工位: 整机工位    配置: default    操作员: ___
开始: 2026-10-21T13:40:16Z    耗时: 00:02:35
sn-address:002
wifi-mac:00:60:a7:0b:9c:f2
bt-mac:...
soft-ware:E02-1.0.1    hard-ware:E02-1.0.1

录像测试:PASS  解码 5 帧，含音频流（detail…）
按键测试:PASS  开机键/音量键/多功能键（10s 内收齐）
WiFi 测试:PASS  驱动 OK；ping 20 包 0% 丢包 avg 23ms
...
汇总: 12 项，PASS 11，FAIL 1，跳过 0
```

- 每项固定一行（未测/跳过也写明原因），汇总行一眼定论；
- 同名 `.json` 并存（机器可读，便于产线 MES 对接/汇总统计）；
- 报告与 media 文件按 SN 归档可选：`reports/<SN>/e02-<时间戳>.txt`（配置开关）。

## 5. 陀螺仪测试（V2.3）

原工程无陀螺仪命令（全源码检索无 sensor 相关 camera-factory 调用），**需要固件配合**。
落地路径：

1. 短期（不依赖固件）：用自定义项框架 + `dumpsys sensorservice` 采样两次，
   数值变化即 PASS（验证陀螺仪芯片上电、注册、出数）；
2. 建议 firmware 侧新增 `camera-factory 16`（返回陀螺仪自检结果），
  上线后改配置 `cmd` 即可，程序不用重发版。

## 批次与验收

| 批次 | 内容 | 验收 |
|---|---|---|
| V2.1 | 按键引导式流程修复；字号/缩放；进度条；实时日志面板 | 按键弹窗收齐 3 键自动 PASS；全测时可见逐项日志与进度 |
| V2.2 | config.toml + 设置页 + 工位档案切换 + WiFi 深度 + 自定义项框架 | 模组/整机档案一键切换，WiFi 两档行为不同 |
| V2.3 | 报告增强（文本+JSON+汇总+SN 归档）+ 陀螺仪项 | 报告含汇总与配置信息；陀螺仪按 dumpsys 方案可跑 |

测试配套：配置解析/报告渲染/strict 按键解析单测；假 adb 端到端覆盖按键引导与 ping 速率。
