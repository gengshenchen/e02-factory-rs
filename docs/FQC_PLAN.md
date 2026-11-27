# FQC 出厂写号工位方案(V4 计划)

> 2026-11-24 与产线/固件逐项确认并经实机界面走查定稿。来源:旧 `HEC-E02-SNwrite.exe`
> (sn_tool) 逆向 + 真机实测(camera-factory 30 已在固件落地)。本工位替代旧写号工具,
> 集成进本程序,作为产线第三个独立工位(`station = "ship"`,与 `module` 模组工位、
> `complete` 整机工位并列,在「设置 → 工位选择」中切换并持久化到 `config.toml`),
> 型号统一由顶层 `model = "E02W" | "E02E"` 控制(WiFi 版标为 `E02W`,eSIM 版标为 `E02E`)。

## 1. 背景与目标

旧 sn_tool(Windows/MFC)流程:版本验证 → `adb push webrtc_serno_file.ini`
整文件覆盖 → 对时 → cf4 收尾。缺陷:

- **整文件覆盖会踩掉设备自己写入的数据**(见 §2)——旧工具的写法在新固件上是错的;
- 版本号/电压值只显示不落盘,旧写号记录文件(`sn.ini`,注:`serno.txt` 实为待写 SN 池)
  只有 SN+时间(且存在换行 bug 多条粘一行);
- 号池消费靠人肉,无"已写号拦截",同 SN 可无限重写;
- Windows 单机、无报告体系。

目标:扫码写号 + 出厂终检 + 收尾切正式版,全流程留痕,防呆完备,数据可追溯。
**不再兼容旧工具文本台账**,统一收口为 `ledger.json` + `ship` 报告 + CSV 导出。

## 2. 固件协议(camera-factory)

| 命令 | 语义 | 判定 |
|---|---|---|
| 3 | 设备信息(`sn-address` / `wifi-mac` / `soft-ware` / `hard-ware`) | 既有 `parse_device_info` |
| 4 | **清理 U 盘 + T32 切正式版**,必须最后执行,**重启后生效** | 输出含 `FAIL` 即失败 |
| 7 | 获取蓝牙固件版本(比对工单 `bt_ver`) | 经 `parse_bt_version` 提取后与工单版本比对(不区分大小写,V/v 均可);整机/模组工位同步采集展示 |
| 8 | SD 卡在位(可配开关 `check_sdcard`,默认关闭) | 输出含 `true` |
| 10 | 读取蓝牙 MAC(设备身份主键) | 既有 `parse_device_info` |
| 12 | 电池电压 | **数值 ≥ `min_batt_volts`**(新增 `parse_battery_volts`,**排除 `%` 百分比行**,兼容 V/mV) |
| 19 | 固件校时(配 `date -s`,复用 `worker::sync_time`) | 失败只记日志;测试工位单台设备接入后仅同步一次 |
| 20 | eSIM 型号读取 IMEI/CCID | 既有 `parse_device_info`(同时兼容 `imei:`/`imei=` 与 `ccid:`/`ccid=`);**放宽 `ccid_valid` 支持十六进制字母**(实机含 `D`) |
| **30** | **`camera-factory 30 <文件> <节> <键> <值>`** 按键写配置 | 见下 |

### cf30 判定与解析约定(2026-11-23 真机实测)

```text
camera-factory 30 webrtc_serno_file.ini ProvisionInfo sn 6302W1000288
[ProvisionInfo]
sn=6302W1000288        ← 写入后全文件回显
...
uuid=
auth_key=
config-update:true     ← 成功标记
```

- 成功 = 输出含 `config-update:true` **且** `[ProvisionInfo]` 节内回显的该键值与写入值精确匹配;
- 失败 = `config-update:false`;两者都无 = 协议异常,按失败处理;
- **回显即免费回读**,键值判定以回显文件内容为准,不信"命令已发出";
- 解析加固:先滤除串口控制字符(`\r`/DC1/EOT 等,同 `report::sanitize`),**按 `[Section]`
  作用域隔离匹配**(只认目标节内键),兼容空值键(`uuid=`)、忽略 `config-update:*` 状态行、
  键与值两侧空白 trim;
- 安全转义:下发前校验参数不含非法控制字符/换行,经 shell 单引号转义后再拼 `adb shell`。

### 设备自写文件的事实与预读方式(设计约束)

设备在 T32 产测模式插 USB 会**自己写** `webrtc_serno_file.ini`:自动从 4G 模组
取 IMEI/CCID 存入,值为空时自生成 `uuid`/`auth_key`(且包含 `enable_sub_record` 等业务键)。因此:

- **adb push 整文件覆盖被禁止**(会踩掉设备数据)——必须走 cf30 按键写;
- 工具写入集 = 型号档案 `keys`(默认只有 `sn`);IMEI/CCID 工具只读回记档;
- `webrtc_id` 预留未用,可选键默认不写;厂测 WiFi 配置与应用配置分离,出厂**不需要**清理;
- **写前预读与重启复核**:`cf3` 不返回 `uuid`/`auth_key`,配置设备端绝对路径
  `config_path`(经 `adb shell cat` / `adb pull` 读取;2026-11-24 已确认:`/config/webrtc_serno_file.ini`)。

## 3. 出厂流程

```text
扫码 SN(扫枪=键盘注入,自动剥回车、去空白、大写化;busy 期间锁输入防连扫)
 ⓪设备身份与现值采集:cf10(BT MAC 必填) + cf3(SN/WiFi MAC[E02 必填]/软件版本必填)
   + cf20[eSIM](IMEI/CCID,缺失时从 ini 兜底并严格校验合法性,缺码拒写防空码出货)
   + 读 config_path(取当前 sn/uuid/auth_key)
   + 台账硬件绑定对冲突检查(check_pair:同蓝牙异绑定值或同绑定值异蓝牙=硬拦防烧重地址覆盖原机)
 ①闸门[gate_mode]:查本机(BT MAC)整机工位是否全测 PASS(strict=硬拦/warn=警告/off=跳过)
 ②SN 校验与异号防呆:
   - 格式校验(当前型号 sn_pattern)
   - 设备现值比对(空/占位默认值/不匹配正式正则=未写号放行;已是另一合法正式 SN=硬拒)
   - 台账反查(该 SN 已绑其他设备=硬拦;已绑本机=标记同号重写)
 ③SN 池校验[已导入才启用]:在池且未写(同号重写允许复用已绑本机条目)
 ④版本核对:t32_ver/bt_ver 留空=配置缺失拒写;soft-ware 与 t32_ver、cf7 与 bt_ver 均不区分大小写(V/v 均可)
 ⑤硬件终检:电池 cf12 ≥ min_batt_volts;SD 卡 cf8[check_sdcard=true 时启用,默认关闭]
 ⑥确认卡与确认环节[auto_commit]:
   展示 SN、目标凭据全值(若有)、设备现值;仅响应鼠标点击放行(防扫码枪多余回车误确认);
   - 若 auto_commit=true 且无告警(非同号重写、非覆盖设备自生成凭据、无整机/型号提醒)→ 自动继续
   - 若 auto_commit=false 或触发上述任一告警 → 暂停等待操作员点击确认
 ⑦凭据领用与锁定[凭据型号](所有只读检查通过后、首条写入前执行):
   - 先按 bound_sn == target_sn 查既有 allocated/committed 凭据复用(幂等)
   - 否则池空=拒写;取下一个 available 凭据,在发首条 cf30 前原子落盘为
     allocated(记录 bound_sn + bound_bt_mac),防中途断电/崩溃导致双机撞凭据
 ⑧cf30 逐键写:按 keys 顺序写(sn → uuid → auth_key),设备现值已一致则跳过;
   每键判定:config-update:true + [ProvisionInfo] 回显键值精确匹配
 ⑨[eSIM 型号且 fourg.enable=true]4G 模组配置写入(§3.1;enable=true 时模板缺失/为空硬拦绝不静默漏写)
 ⑩回读校验(重启前):重读 config_path 复核全部目标键,并将全部回读键值对显示在过程日志与报告中
 ⑪对时 + 收尾切正式版:
   date -s + cf19(固件校时)→ cf4 清 U 盘+切正式版 → sync 刷写闪存缓冲
   → 置"预期重启"原子+事件双标记(屏蔽 devwatch 拔出中止)→ adb reboot
 → PASS:stock 推进为 committed + ledger.json 回填(mark_shipped upsert,带冲突保护)
   + ship 报告落盘
```

**失败与异常隔离**:
- ⑧⑨ 任一步失败立即停、判 FAIL、**不执行 cf4**(不切正式版,留产测态待返修);
- 部分写入可恢复:凭据保持 `allocated`(已绑当前 SN/本机,不会分给下一台);重跑时同 SN
  走"同号确认"、优先复原已分配凭据、已一致键自动跳过(幂等);若设备主板报废可在库存页手动标记作废;
- **与测试模式/在位监测隔离**:处于出厂写号模式时屏蔽 `conn.auto_start`(插机不自动触发测试);
  步骤 ⑪ 执行 `adb reboot` 前屏蔽 `devwatch` 的 `DeviceGone` 强杀,防止正式版重启断连误判为异常拔线。

**闸门实现(`gate_mode`)**:
- 升级 `LedgerRecord` 区分工位(见 §7):仅**整机工位(`complete`)全测 PASS** 才算满足闸门
  (防止仅过模组工位或单项补测绕过);
- `strict`:未查到记录或非整机全 PASS 一律硬拦(台账覆盖率上来后切此档);
- `warn`(**默认**):未查到或非整机 PASS 弹窗/黄条警告但允许继续(兼容旧工具测完过渡期);
- `off`:跳过闸门检查。

### 3.1 eSIM 型号附加:4G 模组配置写入

eSIM 产品有第二套可写区:4G 模组的 `U:/config.ini`(服务器/APN 等,AT 命令
协议同 esim-desktop 的 CFG 域)。模组挂在设备侧 UART(**2026-11-24 确认:
`/dev/ttyS2` @ 115200**),PC 不直接碰串口——系统同事提供**设备端 CLI
`4g-test`**(随固件落地,同 `camera-factory` 形态),本工具经 `adb shell`
转发调用:**系统层管串口,本工具管编排/判定/留痕**,不自己实现串口协议,
也不让操作员另开 esim-desktop 手工做(流程断裂、无留痕)。

```text
4g-test "AT+PROD=1"                进产测模式
── 配置写入事务(失败最多整轮重试 1 次,整体 60s 上限)──
  4g-test "AT+CFGSTART"            清理 U:/config.tmp 并开始配置会话
  4g-test "AT+CFGSET=\"<line>\"" ×N 逐行写(禁止原地重试单行——防超时重发写出重复行)
  4g-test "AT+CFGSAVE"             保存并校验
  4g-test "AT+CFGREAD?"            回读(剥 +CFGREAD: 前缀)→ 归一化后逐行比对,不一致判 FAIL(diff 落报告)
4g-test "AT+PROD=0"                退产测模式(best-effort,失败不阻断判定)
```

- **调用方式**:每条均经 `adb shell 4g-test "<AT命令>"` 在设备端执行;AT 命令内含
  双引号(`AT+CFGSET="…"`)时外层用 shell 单引号包裹再拼 `adb shell`,转义收口在 `exec` 层;
- **内容 = 静态模板 + 预留变量占位**(如 `{{sn}}`,当前先按纯静态模板用;型号维度:
  数据目录 `templates/<profile>_4g.ini`,设置页可编辑);
- **模板校验与比对归一化**:允许空行和 `#` 注释行;有效配置行必须含 `=` 且**禁止包含
  双引号 `"` 或控制字符**(防截断 `AT+CFGSET="..."`);回读比对前剥除 `+CFGREAD:` 前缀、
  过滤空行/注释行并 trim 两端空白;
- **重试策略**:由于模组每收到一条 `AT+CFGSET` 即向 `U:/config.tmp` 追加一行,单行超时/
  失败**严禁原地重试该行**,必须重发 `AT+CFGSTART` 清空临时文件后整轮重跑(最多重试 1 次);
- 位置在 `cf30` 之后、`cf4` 之前——失败不切正式版,留产测态返修;结果行进 `ship` 报告
  (写入行数/回读一致/耗时)。

**`4g-test` 接口契约**(设备端 CLI,系统同事开发、随固件落地):

| 项 | 要求 |
|---|---|
| CLI 形态 | `4g-test [-d <串口>] [-b <波特率>] "<AT命令>"`,stdout 只打模组原始响应行(`+CMD:…`/OK/ERROR),无日志噪声 |
| 串口参数 | **默认 `/dev/ttyS2` @ 115200**(2026-11-24 确认);`-d`/`-b` 覆盖兜底,联调换口才用 |
| 退出码 | 收到 OK=0;ERROR/超时=非 0。老版 adb 不透传远端退出码,**判定以输出内容为主**(同 cf30 风格),退出码仅参考 |
| 超时 | 默认 5s;`CFGSAVE`/`CFGREAD` 放宽 10s;超时打 `TIMEOUT` |
| 无状态与串口互斥 | 每次调用独立(开→发→收→关);**须与设备产测取号进程约定 `/dev/ttyS2` 互斥**(插 USB 自动取 IMEI/CCID 的逻辑若常驻占用,4g-test 需给出明确报错而非静默失败) |

## 4. 数据体系:`provision_stock.json`

**单文件双池,凭据池整体可选**(按产品/客户档案),**单机本地独占**(多台 FQC PC 采用
互不重叠子批次独立分批导入,避免共享盘并发撞号):

```json
{
  "sns": [
    {
      "sn": "6302W1000288",
      "status": "available | allocated | committed | voided",
      "bound_bt_mac": "",
      "updated_at": ""
    }
  ],
  "credentials": [
    {
      "uuid": "uuid95fe...",
      "auth_key": "B7uT...",
      "status": "available | allocated | committed | voided",
      "bound_sn": "",
      "bound_bt_mac": "",
      "updated_at": ""
    }
  ]
}
```

- 存数据目录,原子写(`tmp + rename`,同 ledger 风格);
- 状态字段使用稳定英文枚举(UI 层展示中文:未用 / 已分配 / 已写入 / 已作废);
- 库存维护:支持对报废机手动将 `allocated` 条目标记 `voided`,支持"仅清空未用批次"
  (只删 `available`,绝不碰已分配/已提交记录)。

### 行为矩阵

| 档案类型 | SN 池 | 凭据池 | 行为 |
|---|---|---|---|
| 普通(`keys=["sn"]`) | 已导入 | —(无此概念) | 在池校验 + 库存计数 |
| 普通 | **未导入** | — | 降级:仅格式+台账校验,界面提示"未导入批次" |
| 凭据客户 | 已导入 | 已导入 | 在池校验 → 领用凭据 → 写三键 |
| 凭据客户 | 已导入 | **未导入** | **拒写**:"当前档案要求凭据,请先导入" |

- **SN 池导入可选**(产线习惯兼容);**凭据档案的凭据导入强制**;
- 普通档案界面**不出现任何凭据入口**(按档案 `keys` 显隐);
- 库存余量:普通档案只显示"SN 剩 N";凭据档案显示"SN 剩 N / 凭据剩 M"。

### 导入格式与校验规则

- 两个 txt 独立导入:SN txt(发号系统,一行一个)+ 凭据 txt(`uuid`/`auth_key` 对子,**不带 SN**);
- **文本清洗**:自动剥 UTF-8 BOM(`\u{FEFF}`)与 `\r\n`,忽略纯空行与 `#` 注释行;凭据行支持
  **逗号 / Tab / 空白**分隔且须恰好 2 列;
- **拒入 vs 跳过(明确分离)**:
  - **整批拒入**(列出错误行号):格式不匹配档案正则、**批内重复**、凭据中同一 `uuid`
    与库存现有条目存在**不同 `auth_key` 冲突**;
  - **追加跳过**:与现有库存**完全一致**的重复条目静默跳过并计数(**已写/已分配状态永不重置**),
    仅追加新增条目;
- 数量对账:凭据档案下可用 SN 数 > 可用凭据数 → 警告(凭据写完即断供拒写)。

### 凭据取用制

客户凭据与 SN 无预先对应关系,**写号时领用**:在所有只读检查通过后、首条 `cf30` 之前,
优先按 `bound_sn == 当前SN` 复用既有分配;无则取下一个 `available` 凭据并原子落盘为
`allocated`(记录 `bound_sn` 与 `bound_bt_mac`),PASS 后推进为 `committed`。

## 5. 配置(`config.toml`)

工位统一由顶层 `station = "module" | "complete" | "ship"` 控制,产品型号统一由顶层
`model = "E02" | "E02E"` 控制(单一型号真源);`[provision]` 下存放公共开关与按型号分档的
`[provision.e02]` / `[provision.e02e]`(加载时自动迁移旧 `[provision.profiles.*]` 表)。

```toml
station = "ship"                          # 工位: module(模组) | complete(整机) | ship(出厂写号)
model = "E02W"                            # 产品型号: E02W(WiFi 版) | E02E(eSIM 版)

[provision]
gate_mode = "off"                         # 写号前整机测试检查: off(默认不检查) | warn(弹窗提示) | strict(禁止写号)
auto_commit = true                        # 校验无异常自动写入;false=逐台人工确认
config_path = "/config/webrtc_serno_file.ini" # 设备端 ini 绝对路径
placeholder_sns = ["00001", "002"]        # 出厂前占位 SN

[provision.e02w]                          # E02W(WiFi 版)写号参数(兼容读取旧 [provision.e02])
t32_ver = "V2.4.3"                        # 工单软件版本(不区分大小写,留空禁止写入)
bt_ver  = "V3_9_2"                        # 工单蓝牙版本(不区分大小写,留空禁止写入)
min_batt_volts = 3.5                      # 最低电池电压(V)
check_sdcard = false                      # 写入前检查 SD 卡在位(默认关闭)
sn_pattern = '^\d{4}W\d{7}$'              # 内部校验规则(界面隐藏),示例: 6472W1000016
keys = ["sn"]                             # 勾选"同时写入客户凭据"时为 ["sn", "uuid", "auth_key"]
uuid_pattern = '^uuid[0-9a-f]{16}$'
key_pattern  = '^[A-Za-z0-9]{32}$'

[provision.e02e]                          # E02E(eSIM 版)写号参数
t32_ver = "V2.4.3"
bt_ver  = "V3_9_2"
min_batt_volts = 3.5
check_sdcard = false
sn_pattern = '^\d{4}E\d{7}$'              # 内部校验规则(界面隐藏),示例: 6472E1000012
keys = ["sn"]

[provision.e02e.fourg]                    # 4G 参数写入(§3.1)
enable = false                            # false=跳过;true=模板缺失或回读不符判定不合格
port = "/dev/ttyS2"                       # 设备端串口(留空用 4g-test 默认)
baud = 115200                             # 波特率(0=默认 115200)
template = "templates/e02e_4g.ini"        # 设置页支持在线编辑与语法检查
```

## 6. 重写与幂等规则

| 对象 | 设备当前值 vs 目标值 | 动作 |
|---|---|---|
| SN | 空 / 命中 `placeholder_sns` / 不匹配正式 `sn_pattern` | 视作未写号新机,正常写入 |
| SN | 与目标 SN 相同 | 跳过写 SN(触发"同号确认重写"后重跑其余步骤) |
| SN | **已是另一个合法正式 SN 且不同** | **硬拒**(已写号设备换号,防串联) |
| `uuid`/`auth_key` | 空 | 写入 |
| `uuid`/`auth_key` | 与目标值相同 | 跳过(幂等) |
| `uuid`/`auth_key` | 非空且不同(设备自生成) | **警告 + 操作员确认后覆盖**(凭据档案必须写预分配值) |

同 SN 重写(返修/复测):弹窗确认 → 全流程重跑,复用原分配凭据、已一致项跳过,台账与报告每次更新。

## 7. 台账、报告与出货 CSV 导出

不再兼容旧工具文本台账(`sn.ini`/`serno.txt`),统一收口为三类产出:

- **绑定台账升级与回填(`ledger.json`)**:
  - `LedgerRecord` 新增测试工位字段(`last_station` / `complete_verdict` / `complete_pass_at`),
    仅整机工位全测 PASS 时更新 `complete_verdict = "PASS"`,支撑 §3 闸门;
  - 新增出厂独立字段(`shipped_sn` / `shipped_at` / `ship_verdict`),与测试回读 `sn` 隔离
    (防止返修重刷默认固件 `00001` 时被 `bind()` 覆盖正式出厂 SN);
  - 新增 `mark_shipped` 支持 **upsert**(台账尚无该 BT MAC 记录时按本次读到的
    `bt_mac` + `partner`(WiFi MAC/IMEI) + `model` 自动建档并写入出厂字段);
  - **凭据不进台账**,原有"绑定记录 CSV 导出"不含凭据。
- **ship 报告**:按日期子目录归档于 `reports/YYYY-MM-DD/ship-<日期>-<蓝牙MAC>.txt`(测试报告同样归档于 `reports/YYYY-MM-DD/`;未读到 BT MAC 兜底 `ship-<日期>-<时分秒>-unknown.txt`),
  完整记录:型号/档案、SN、凭据全值(若有)、WiFi MAC 或 IMEI/CCID(按产品有无落行)、
  两版本号、电池电压实测值、4G 写入/回读判定及 diff、各步判定与耗时。同机同日重跑覆盖最新,并兼容读取历史平铺报告。
- **出货明细 CSV 导出**(出厂页独立入口,UTF-8 BOM Excel 直开,保存至 `exports/shipments/`,绑定记录导出至 `exports/bindings/`):
  - 关联 `ledger.json` 已出厂记录与 `provision_stock.json` 导出,供交付客户/云端批量激活;
  - **字段按产品/档案自适应**(有则列/填,无则不出现或留空):基础列 `SN, 蓝牙MAC, 型号, 写号时间, 判定`,
    WiFi 型含 `WiFi MAC`,eSIM 型含 `IMEI, CCID`,凭据档案含 `uuid`(默认不含 `auth_key`,
    提供"包含 auth_key"勾选项按需导出)。
- **报告中心**:`ship-` 报告列表**可见**、**不计入**测试工位今日产量/良率。

## 8. 工位独立统计与报告改造(附带修隐藏 bug)

现状文件名 `<model_prefix>-<日期>-<蓝牙MAC>.txt`(`e02-` / `e02e-`)**不含工位**:同机同日过
模组+整机工位两份报告**互相覆盖丢数据**。改造:

- 文件名**加工位段**:`<model_prefix>-<日期>-<station>-<蓝牙MAC>.txt`
  (`model_prefix` ∈ `e02`/`e02e`,`station` 经 `sanitize_token` 清洗,常规为 `module`/`complete`,
  `unknown` 与 `-冲突<hms>` 后缀保持兼容);出厂报告独立前缀 `ship-`;
- 同机同日跨工位**各自保留、各自统计**;
- 旧报告(无工位段)照常列出,统计按报告内容 `工位:` 行归组(`scan_stats` 本就逐文件读
  `结果:` 行,顺手读 `工位:` 行,零额外开销);
- **修复 `list_reports` 排序 Bug**:现有代码按完整文件名字典序倒排并截断前 300 条,引入 `ship-`
  后会因 `'s' > 'e'` 导致历史出厂报告永久压在测试报告之前甚至挤爆上限——改为**提取日期段
  (+ 文件 mtime 兜底)统一倒排**;`dialog_report.rs::display_name` 同步适配 `e02-`/`e02e-`/`ship-`
  及工位段显示;
- 今日统计按工位分组 `{module, complete}`,界面显示**当前工位**数字;**设置页切换工位立即调用
  `refresh_stats()` 重扫**当日报告(目录扫描,无持久化状态);
- 出厂页独立统计:扫 `ship-` 前缀 → 今日写号 N / 成功 x / 失败 y。

## 9. UI(遵循 design-md/claude 规范)

- **设置页工位选择**:`模组工位` / `整机工位` / `出厂写号` 三工位并列持久化到 `config.toml`;
  下方配置卡按工位显示(测试工位仅显测试项与配网,写号工位仅显工单版本、写入校验规则与 4G 参数模板);
  顶栏统一显示 `v版本号` + `工位中文名 · 型号(E02W/E02E)`;
- **出厂写号三栏主界面**(暖米画布 → 奶油内容卡 → 暖黑执行明细卡,明暗交替,无特殊符号字形):
  - **左栏(240px `CANVAS`)**:写号流程清单 + 次级按钮「批次与库存」;
  - **中栏(`CANVAS` 暖米底)**:工单版本与余量胶囊条 + 奶油扫码卡(等宽输入框 + 唯一珊瑚主按钮「开始写号」)
    + 写入确认卡(仅鼠标点击放行)/ 结果色块横幅 / 深色「执行明细」卡(`DARK` + `DARK_SOFT`);
  - **右栏(300px `CANVAS`)**:「被测设备」实时字段表(点击复制)+「写号结论」色块卡 +「当日统计」2×2 等宽瓦片;
- **批次与库存窗口**:支持系统原生文件选择窗导入 `.txt/.csv`、拖拽文件导入与折叠粘贴导入、
  凭据余量提醒、未完成占用条目释放/作废、导出出货明细 CSV;
- **报告中心**:支持 `全部 / 整机 / 模组 / 出厂写号` 筛选与对应工位今日统计看板。

CLI 对应(`e02-factory-rs ship --sn X [--dry-run]`)为后续可选项,不阻塞。

## 10. 代码落点与实施顺序

| 步骤 | 内容 | 依赖真机 |
|---|---|---|
| 1 | `Cargo.toml` 加 `regex-lite`;`src/stock.rs`:双池三态枚举 + 导入清洗/校验/跳过合并 + 领用预落盘与幂等复原(纯逻辑,单测全覆盖) | 否 |
| 2 | `protocol.rs`:`CameraFactory` 补 `4`/`7`/`30`、`parse_cf30_response`(去控制符/Section 隔离/trim)、`parse_battery_volts`(排除 `%`、兼容 V/mV)、`parse_bt_version`、**修复 `ccid_valid` 支持十六进制字母** | 否 |
| 3 | `src/provision.rs`:流程引擎(§3 ⓪–⑪ 编排、占位 SN 判断、凭据锁定失败路径、幂等、预期重启隔离 `devwatch`) | 否(fake-adb mock) |
| 3.5 | `provision.rs` 4G 配置步骤:`adb shell 4g-test` 封装(引号转义/输出内容判定) + 模板校验/占位符替换 + `CFGSTART` 整轮重试 + `CFGREAD` 归一化比对(fake-4g-test mock) | 否 |
| 4 | 报告改造:文件名工位段(`e02-`/`e02e-`) + `list_reports` 按日期/mtime 倒排 + `display_name` 适配 + 按工位统计 + `ship-` 渲染与统计 + 旧文件兼容 | 否 |
| 5 | `ledger.rs`:整机工位判定字段 + `shipped_sn` 分离 + `mark_shipped` upsert;出货明细 CSV 自适应列导出;`config.rs [provision]` 强类型档案结构 | 否 |
| 6 | UI:工位选择 + 出厂页(含导入/库存维护/出货 CSV 导出与打开文件位置) + 设置页 `[provision]`(单一凭据开关、隐藏正则输入) + 切工位触发 `refresh_stats()` | 否 |
| 7 | 真机联调:cf30 三键实测、cf7 版本格式、`4g-test` 与设备取号进程的 `/dev/ttyS2` 互斥 | **是** |

测试策略沿用 `worker.rs` 的 fake-adb mock 模式;重点覆盖:新机占位 SN 放行与已写正式 SN 异号拒写、
同号重跑幂等与凭据复原、凭据预落盘防崩溃撞号、凭据断供拒写、cf30 false/协议异常/跨 Section 同名键、
电压百分比行防误判、4G 失败整轮重试无重复行、导入整批拒入与重复跳过、跨工位报告不覆盖与 `ship-` 排序。

## 11. 依赖与风险

| 项 | 状态 | 说明 |
|---|---|---|
| cf30 固件落地 | ✅ 已实测 | `config-update:true/false` + 回显判定约定已确认 |
| cf4 / cf7 语义 | ✅ 已确认 | cf4 最后执行、切正式版并重启生效(正式版关闭 adb,无需重启回读);cf7 返回蓝牙版本(如 `V3_9_2`) |
| `webrtc_serno_file.ini` 设备端路径 | ✅ 已确认 | `/config/webrtc_serno_file.ini`(2026-11-24),用于 ⓪ 预读现值与 ⑩ 回读校验 |
| **固件:uuid/auth_key 外写后不自生成** | ⚠️ 需固件保证 | 仅值为空时生成 |
| **4g-test 设备端 CLI(系统同事)** | 🔶 待开发/实测 | 默认 `/dev/ttyS2`@115200,随固件落地、经 `adb shell` 调用(§3.1 契约);未到位前档案不配 `fourg` 即跳过;**须与设备取号进程约定串口互斥,真机实测 20+ 轮 PROD→CFG 事务稳定** |
| E02E 适配 | ✅ 已实测 | `cf20` 支持 `imei=`/`ccid=` 与 `:` 双分隔符,绑定对为蓝牙↔IMEI,CCID 兼容十六进制字母 |
