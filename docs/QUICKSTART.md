# QUICKSTART：从一台新手机到第一个优化候选（≤30 分钟）

目标：一台 root 的 Android 手机 + 一台常开电脑（节点），半小时内让 harness 在你的游戏上
测出第一个「省电/更流畅且画质不变」的候选。整套东西分三层：

- **phonefarm**（本仓库）：真机判定层。拿锁、上机、ABBA 对照、出判定，规则先于数据冻结。
- **game_opt_loop**：决策层。出方案（大模型/本地）、逐代优化、目标模式（`goal`）。
- **harness-ops**：节点层。node_service 把手机挂上网（`https://<你的节点>/dash` 实时看板）、
  pf-node 远程调用、pf-fleet 跨节点调度。

## 0. 准备（5 分钟）

- root 的 Android 手机一台（Magisk），USB 插到节点电脑，开发者选项开 USB 调试；
  关掉系统自动更新/云同步（它们会在测量中途抢 CPU）。
- 节点电脑（macOS/Linux）：装好 `adb`，克隆本仓库与
  [game_opt_loop](https://github.com/HGamey/game_opt_loop)、
  [harness-ops](https://github.com/HGamey/harness-ops)。
- 编译：`cd phonefarm/src && cargo build --release`；`cd game_opt_loop && cargo build --release`。

## 1. 验证手机通路（2 分钟）

```bash
phonefarm devices                 # 看到你的手机序列号
phonefarm preflight               # 环境不变量：风扇/静音/温度（--fix 自动修能修的）
phonefarm fleet --json | head     # 全场只读快照：电量/温度/风扇/前台
```

preflight 全过再往下。风扇开着、媒体没静音、手机烫手，都会让后面的数字作废。

## 2. 接入你的游戏（5–10 分钟，零人工）

```bash
phonefarm onboard --package com.your.game --rounds 3
```

onboard 会自动：冷启动 → 过新手引导/登录（ OCR + 输入注入）→ 进到游戏内 → 找到能
重复走的场景 → 写一份 `profile.json`（负载、渲染线程、坑）。**这是最容易卡的一步**：
内置条目认不出的游戏要人工补一次路线，补完就收进档案库，以后全自动。

## 3. 把游戏拆一帧（可选但强烈推荐，5 分钟）

```bash
phonefarm frame-audit capture --package com.your.game --out fa/
phonefarm frame-audit analyze fa/ --json > fa/analysis.json
```

这份分析会告诉你：一帧里哪些写回「确定没人读」（② 灰档层的开关候选就来自这里）、
MSAA/后处理搬运占多少。成熟商业游戏通常已经很干净；发现不了值得动的就早点换游戏，
别在频率旋钮上空耗——那是厂商已经优化到极限的地方。

## 4. 目标模式跑第一代（10 分钟起步）

```bash
cd game_opt_loop
game_opt_loop goal draft --game com.your.game \
    --intent "这个游戏尽量省电，帧率和卡顿不能变差，画面不能变" \
    --out goals/my-first-goal
# 检查/修改 goals/my-first-goal/goal.md，然后：
game_opt_loop goal run goals/my-first-goal
```

harness 会逐代自动：出方案 → 真机对照测量 → 判定（KEEP/REJECT）→ 接着出题。
想停就改 `goals/my-first-goal/human.md`：第一行写 `action: pause`（或 `stop`）。
进程死了/机器重启了：`game_opt_loop goal resume goals/my-first-goal` 断点续跑。

## 5. 挂上网（节点化，可选）

让手机成为 7×24 节点，看板和远程调用都通了：

```bash
# 在节点上（macOS 示例）
cp harness-ops/node/com.hgamey.node.daemon.plist /Library/LaunchDaemons/com.hgamey.node.plist
launchctl bootstrap system /Library/LaunchDaemons/com.hgamey.node.plist
# 开发机上
pf-fleet status        # 所有节点一览
pf-node devlock status # 锁状态
```

节点注册表 `.hgamey/nodes.json` 加一行就入池；多台节点之间 `pf-fleet run --any-free`
自动挑空闲节点，`pf-fleet migrate` 能把跑了一半的目标运行整体搬到别的节点续跑。

## 常见坑（都是真事）

- **风扇**：主动散热风扇会吃掉功耗读数还会被误判成优化收益——preflight 每轮必查。
- **电池满电**：Full 状态下功耗读数是假数，测量必须在放电/旁路态（harness 自动停充）。
- **温控**：SoC 到温控墙一切频率读数都失真，等冷是测量的一部分，不是浪费时间。
- **频率旋钮**：GPU/CPU 频率这类方向厂商已经卷完了，一晚上也磨不出收益；
  油水在渲染层（多余的写回、MSAA 搬运、后处理），先拆帧再出题。
