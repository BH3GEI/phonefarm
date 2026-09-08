# SPEC: phonefarm keepalive — 农场级设备保活巡检 (L1 契约)

> 状态: 定稿 v1 · 2026-09-08
> 背景: 挂机设备(Android 模拟器/OpenHarmony 真机)会息屏、重新上锁、不息屏配置被系统组件
> 冲掉,导致 run/benchmark 开局先死在黑屏上。quest.rs 的生命周期是**任务级**的(进任务
> stayon true、退出 false+锁屏保电池),不管任务之外的待命期。keepalive 补的是**农场级**:
> 只要设备挂在农场里,就该醒着、解着锁、不息屏。
> 实测教训(2026-09-08, 两台 OH 真机 + 三台 Android 模拟器对拍定论,见 §4 顺序契约)。

## 1. 命令形态

```
phonefarm keepalive [--serial S] [--status] [--watch [间隔秒]] [--json]
```

- 缺省: 对当前全部在线设备(adb `device` 态 + hdc targets)做一轮巡检——唤醒、解锁、
  写入/修复不息屏配置,逐台打印结论。
- `--serial S`: 只巡检一台(`emulator-5554` 或 `hdc:<key>`,与 run 同一 serial 契约)。
- `--status`: 只读报告(连接/屏幕亮灭/不息屏配置是否生效),**不发任何写动作**——
  不 wakeup、不 settings put、不 override。与 CLI 查看层同一只读纪律。
- `--watch [间隔秒]`: 常驻守护,每间隔秒重枚举设备并巡检一轮(默认 300s);
  每轮重新枚举,新上线设备自动纳入,掉线设备如实报告不 panic。Ctrl+C 退出。
- `--json`: 全形态支持,与 CLI 契约 v1.0 一致。
- 退出码: `0`=全部目标设备亮屏且策略生效;`1`=有设备未达标;`2`=用法错/无目标设备。
- 不烧 token、不写 `tasks/`、不需要 `secrets.env`(与局无关,不读 phonefarm.toml 亦可跑)。

## 2. 设备枚举

复用 `devices` 子命令的两族并列逻辑,各自 best-effort(工具不在 PATH 跳过该族,不报错):

- adb: `adb devices` 输出中取第二列为 `device` 的行(跳过 `offline`/`unauthorized`)。
- hdc: `hdc list targets` 非空非 `[Empty]` 的行,以 `hdc:<key>` 形态进入 serial 集。

## 3. Android 巡检序列(幂等)

顺序执行,全部幂等,可任意频率重放:

| # | 动作 | 命令 |
|---|---|---|
| 1 | 点亮 | `input keyevent KEYCODE_WAKEUP`(224;**绝不发 KEYCODE_POWER**——翻转键会把亮屏按灭,commit 1767830 的定论) |
| 2 | 解锁 | `wm dismiss-keyguard`(滑动锁/无锁直接消;密码/图案锁消不掉,如实报告,**不绕过**) |
| 3 | 不息屏 | `settings put system screen_off_timeout 2147483647`(int32 上限,Android 无"永不"值) |
| 4 | 充电常亮 | `svc power stayon true`(开发者选项"保持唤醒",模拟器等价永充) |

校验(判定依据,不是步骤): `dumpsys power` 的 `mWakefulness=Awake` 且
`settings get system screen_off_timeout` 回读为 `2147483647`。

## 4. HDC 巡检序列(顺序是契约)

**OH 锁屏应用(KeyGuard)在锁屏界面显示期间会把息屏覆盖值强写为 10000ms,解锁瞬间又会
把覆盖值"恢复"冲掉先前写入**。实测形态: 唤醒→亮在锁屏→10s 后被掐灭→熄灭即重新上锁,
巡检陷入死循环。因此顺序必须如下,override 永远落在最后:

| # | 动作 | 命令 |
|---|---|---|
| 1 | 点亮 | `power-shell wakeup` |
| 2 | 取分辨率 | `hidumper -s RenderService -a screen` 解析 `activeMode: <W>x<H>`(比截屏回传便宜;解析不到放弃本次滑动,仍继续后续步骤) |
| 3 | 解锁 | `uitest uiInput swipe <W/2> <H*0.92> <W/2> <H*0.30>`(底部中点上滑,比例坐标,不写死分辨率) |
| 4 | 等待 | 2s(KeyGuard 的恢复写发生在解锁后数秒内,实测 3~4s) |
| 5 | 不息屏 | `power-shell timeout -o 2147483647`(int32 上限;重启/锁屏恢复后失效,故每轮必重放) |

校验: RenderService `powerStatus=POWER_STATUS_ON` 且
PowerManagerService `OverrideTimeout=2147483647ms`。

设备带密码/图案锁时第 3 步解不开: 不绕过、不尝试。覆盖值落在锁屏之上,屏幕保持点亮停在
锁屏界面(农场语义: 亮屏优先);该台是否"可用"由上层任务开局自行判定,keepalive 如实报告
亮屏与覆盖值两个地面真值,不编造"已解锁"结论。

## 5. 边界与分工

- **与 quest 生命周期的分工**: quest 是任务级(保电池优先,退出恢复 stayon false + 锁屏);
  keepalive 是农场级(待命优先)。同时挂着 `--watch` 又跑 quest 的自动锁屏时,两者互相覆盖,
  以最后动作者为准——**同一台设备不要同时挂两边**,SPEC 只声明语义不强制互斥。
- **不进 MCP serve 面**: keepalive 是设备写操作,与 `probe`/`exec`/`parallel` 同一边界
  (SPEC_MCP_SERVE §3: 裸 shell 不进 MCP)。`--status` 的只读子集将来如需进 MCP,另起 SPEC。
- **通用性**(AGENTS.md 红线): 不写死 serial/分辨率/AVD 名/设备型号;枚举与坐标全部现场解析。
- **零新依赖**: 纯 std + 既有 device.rs 抽象(`Device::shell`/`health_check`/`swipe`)。

## 6. 测试契约

- 全部输出解析器为纯函数并单测: adb 设备行(跳过 offline)、hdc targets(滤 `[Empty]`)、
  `mWakefulness`、`powerStatus`、`OverrideTimeout`、`activeMode`、解锁滑动坐标换算。
- 真机回归: `keepalive --status` 只读不发写(hilog 无 override 记录);一轮巡检后 70s+
  两族屏幕均保持 ON 且 override 保持 2147483647(KeyGuard 死循环不复现)。
- `cd src && cargo test` 100% 通过(AGENTS.md 代码稳定性条款)。
