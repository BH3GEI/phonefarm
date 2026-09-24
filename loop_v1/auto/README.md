# auto — 系统参数的全自动优化闭环

loop_v1 验证了闭环本身成立 (手工挑一个旋钮, 跑出 p95 −1.61% / p=0.0079)。
`auto/` 把「挑旋钮」这一步也交给机器, 变成无人值守的循环:

```
真机探白名单 → 冻结判定规则 → ┌─ 大模型挑一组参数 ─┐
                              │  应用到手机         │
                              │  等冷               │
                              │  原神 A/B 交替实测   │
                              │  精确置换检验        │
                              │  保留 / 淘汰         │
                              │  还原               │
                              └─ 结果喂回模型 ──────┘  × N 代
```

## 三条各自独立的判断

| 谁 | 决定什么 | 不决定什么 |
|---|---|---|
| 白名单 (`src/sysparam.rs`) | 准改哪些参数、准改成哪些值 | 不决定改成什么 |
| 模型交互 (`src/llm.rs`) | 这一轮改成什么 | 不决定好不好 |
| 判定规则 (`src/sysparam.rs`) | 好不好 | 不决定改什么 |

编排本体已搬进 `src/autoloop.rs` (`phonefarm autoloop`), **不含任何判定口径**; 判定口径全在 `src/sysparam.rs`。

白名单、判定、模型交互都已搬进 phonefarm 二进制 (`phonefarm loopstat` 的
`build_whitelist` / `validate_candidate` / `plan_text` / `rule_doc` / `env_stats` /
`decide` / `build_prompt` / `parse_candidates` / `local_mutate` / `llm_chat` 等),
Python 侧经 `loop_v1/tools/pybridge.py` 转调 —— 口径只留一份。
autoloop 已全量在 Rust 侧。

对应的单测也跟着搬成了 Rust 测试:
`cd src && cargo test sysparam`(白名单/判定)、`cargo test llm`(模型侧)、
`cargo test framecheck`(画面动没动)、`cargo test pyrandom`(局部变异的可复现性
靠 CPython random 的逐位复刻)。

## 白名单怎么来的

不是拍脑袋列的常量表, 是 `probe_sysparam.sh` 真机探测结果的函数。每个候选参数
要同时过三关才准进:

1. 节点存在且可读
2. 可写 (把当前值原样写回, 不改变状态)
3. **写了真生效** —— 写一个不同的合法值, 等 1s 回读, 看内核有没有把它退回去

第 3 关是关键。很多 sysfs 节点接受 `write()` 但随即被驱动或热策略夹回原值, 只看
write 的返回码会得出"能改"的假结论。探测脚本每次改动都在同一个函数里立刻还原,
探完再做一次全量快照比对, 确认探测本身不留痕。

候选覆盖: CPU 各簇 `scaling_min_freq` / `scaling_max_freq` / `scaling_governor`、
GPU `min_pwrlevel` / `max_pwrlevel` / devfreq 的 min/max/governor、DDR 与 LLCC 的
`boost_freq`、刷新率。厂商限帧控制点只做只读扫描并记进 `probe.txt` ——
没在真机确认之前不进白名单, 不乱写。

### NX809J 上实际探出来的 11 项

证据在 `runs_sysparam/probe3/`。10 项 sysfs 全部 `effect=live` (写进去、等 1s、
值稳住没被退回), 刷新率那项靠活动模式 fps 真变了才算数:

| 参数 | 探测时原值 | 档数 |
|---|---|---|
| `cpu.policy0.scaling_min_freq` / `_max_freq` | 787200 / 1785600 | 28 |
| `cpu.policy0.scaling_governor` | walt | 5 (walt/conservative/powersave/performance/schedutil) |
| `cpu.policy6.scaling_min_freq` / `_max_freq` | 883200 / 1497600 | 27 |
| `cpu.policy6.scaling_governor` | walt | 5 |
| `gpu.min_pwrlevel` / `gpu.max_pwrlevel` | 17 / 3 | 18 |
| `bus.DDR.boost_freq` | 547000 | 11 |
| `bus.LLCC.boost_freq` | 282000 | 9 |
| `setting.system.refresh_rate_mode` | 0 | 4 |

几条只有上机才知道的事:

- **GPU devfreq 不在 `$KGSL/devfreq`**, 在 `/sys/class/devfreq/3d00000.qcom,kgsl-3d0`。
  即便如此那几个节点在本机读不到, 所以没进白名单 —— GPU 频率边界走 `min/max_pwrlevel`。
- **AOSP 的 `peak_refresh_rate` / `min_refresh_rate` 在本机是 `null`**。红魔走自己的
  `refresh_rate_mode`, 取值表在 `system:all_refresh_rate` (`auto,60,90,120,144`)。
  厂商键的语义没有文档, 所以不按下标猜: 逐档写进去看 SurfaceFlinger 的活动模式
  fps 跟不跟着变。实测 1→60Hz、2→90Hz、3→120Hz、4→144Hz, 0 是 auto (当时也是 120Hz)。
  mode3 因为和当时的基准 fps 一样, 区分不出来, 按规矩不收。
- **CPU 频率上限被厂商压着**: policy0 的 `scaling_max_freq` 是 1785600, 而硬件
  上限有 3628800。所以生效测试的试写值必须落在 `[当前 min, 当前 max]` 里 ——
  拿「第二高的可用频点」去试 min, 内核会直接夹回 max, 测出来的是 min>max 被夹,
  不是「这个节点写不动」。
- **`kgsl.thermal_pwrlevel` 与 `kgsl.max_gpuclk` 由驱动按温度自己改**。实测探测
  前后 6→4 / 646MHz→826MHz (见 `probe3/report.json` 的 `driver_owned_diffs`),
  只是设备凉了一点。这两项归 `driver_owned`, 不算留痕。

### 温控保护永远不进白名单

关热保护、抬温控阈值能立刻换来漂亮数字, 但那是拿硬件安全换指标。
`thermal / trip_point / cooling / fan / tsens / bcl / throttl` 这几个关键词命中即拒,
**不看探测结果, 不看大模型怎么说**。主机端白名单 (`src/sysparam.rs`) 拦一道,
设备端 `knob_sysparam.sh` 再拦一道 —— 故意冗余, 设备端是最后一道。

另有一个温度上限: 超了这一组直接作废并还原, 不参与比较。上限 =
`max(45°C, 基线实测最高温 + 3°C)`, 在采第一组候选数据之前算好并写进 `rule.json`。

等冷目标是另一回事, 且不是绝对温度: 连跑几十轮原神之后设备根本降不到一个固定的
低温, 拿绝对值当门槛会把每组候选都卡死在等冷超时上。目标取
`min(max(40°C, 基线起跑温度 + 1°C), 温度上限 − 2°C)` —— 意思是「回到基线是在什么
热态下量的」, 同样在看候选数据之前冻结。**等冷超时不作废本组**: 组内 ABBA 交替已经
让两臂承受同样的残余热漂移, 起跑偏热会同等影响两臂, 不构成偏向; 安全由温度上限
单独把关。哪几组是热起跑的, `result.json` 的 `cooldown` 字段看得见。

## 判定规则 (看数据之前就冻结)

| 指标 | 方向 | 来源 | 当前是否计入判定 |
|---|---|---|---|
| `frame_p95` | 越小越好 | ftrace kgsl 帧时序 | 是 |
| `fps_mean` | 越大越好 | 同上 | 是 |
| `power_w_mean` | 越小越好 | `power_supply` 电池轨 | **否** (见下) |
| SoC 结温 | — | 热区 | 只作上限, 不作加分项 |

系统参数不动画面, 画质天然不变, 不必量。

### 功耗为什么暂时不进判定

设备插着 USB 时测到的不是整机功耗, 两个坑都是实测踩出来的:

- USB 输入功率里约 **46%** 是在给电池充电, 而且充电电流随电量**单调衰减** ——
  这个衰减会被当成"功耗随时间下降"混进 A/B 比较。
- `battery/power_now` 在本机**单位是错的**, 读出过 777W。所以这里不像
  `hwcond.rs::PowerSample::watt` 那样优先用它, 而是以 `|V x I|` 为准;
  `power_now` 只作为原始值记录并标一个 `power_now_plausible` 位。

解法是**停充**: `charge_suspend.sh` 在测量期间把充电关掉, 让整机真由电池供电,
测完自动恢复。节点表、12 秒等待、30% 电量下限、`on_battery` 判据全部照搬
phonefarm `src/hwcond.rs` (PR #27 真机验过的那一版), 不另造一套 —— 本机管用的是
`/sys/class/qcom-battery/charging_enabled`, 它不在 `power_supply` class 下,
只看 `power_supply` 会整个错过。

`on_battery` 要两个判据**同时**成立: `status` 不是 `Charging` **且** `current_now < 0`。
两个都会单独骗人 —— 停充之后有的内核仍写 `Not charging` 而不是 `Discharging`,
而 `current_now` 在充放平衡的瞬间会过零。

功耗进不进判定因此是 `--power-in-verdict {auto,on,off}`, 缺省 `auto`:
**基线轮每一轮都真的拿到放电态才计入**。不是"能测到就用" —— 充电态下测到的数
看起来很正常, 悄悄拿它判保留/淘汰比不测还糟。

停充失败不致命: 拿不到放电态就只是功耗这一项不进判定, 帧时与帧率照常测。
功耗数据无论如何照常逐轮记录 (`power_source` / `on_battery` / `battery_status` /
`battery_capacity_pct` / `current_now` / `voltage_now` / `usb_input_w_mean` /
`power_usable_reason` 全部落盘), 报告里 `test_conditions.power_caveat` 写明结论。

停充节点本身是**测量前提, 不是可调参数** —— 它进不了自动调参白名单, 有单测钉死。

- **保留** = 任一指标显著改善, 且没有任何指标显著变差
- **淘汰** = 其余情况

多重比较: 一次看多个指标, 都按 0.05 判会把假阳性抬上去 (3 个指标约 14%)。所以
「改善」一侧用 Bonferroni 收紧到 `alpha/K`, 「变差」一侧仍用 0.05 不收紧 ——
**两侧故意不对称**: 宁可漏掉一个真改善, 也不要把一个真退化放过去。

功耗不计入判定时 K=2, `alpha_win = 0.025`; 加回功耗后 K=3, `alpha_win = 0.0167`,
需要每臂至少 5 轮才够得着 (5v5 置换全枚举 252 种, 最小可能 p = 0.0079)。轮数不足时
`rule.json` 里 `reachable=false` 明写这一条, 任何候选都不可能判保留 —— 规则自己
交代它的分辨率上限, 不靠事后解释。

## 旋钮实现

`knob_sysparam.sh` 与 `knob_ddr_boost.sh` 同接口 (`contract/knob.md` 的三态),
区别是「改哪些项」不写死在脚本里, 而是由主机端下发一份 plan 文件:

```
sysfs<TAB>/sys/devices/system/cpu/bus_dcvs/DDR/boost_freq<TAB>5333000
setting<TAB>system:peak_refresh_rate<TAB>60
```

这样大模型每轮挑的任意一组参数都能用同一个旋钮落到设备上, 不必为每组参数生成脚本。
apply 前全量校验, 有任何一项越界就**整份拒绝, 一个字节都不写**。
写入走两遍 (第一遍可能被 `min<=max` 之类的顺序约束夹回, 第二遍补齐),
原值只在第一遍记录, 所以重复写不会污染回滚依据。

## 用法

```bash
# 只探白名单, 不跑实验 (几分钟, 会短暂改写几个节点并立刻还原)
phonefarm autoloop --out loop_v1/runs_sysparam/probe --probe-only

# 完整闭环: 2 代 × 3 组, 每组 5 对 A/B (测量期间自动停充, 测完恢复)
phonefarm autoloop --out loop_v1/runs_sysparam/<标签> \
    --generations 2 --children 3 --pairs 5

# 功耗强制不进判定 (例如电量低停不了充时)
phonefarm autoloop --out ... --power-in-verdict off

# 不用大模型, 只用本地变异器 (离线也能跑通闭环)
phonefarm autoloop --out ... --no-llm

# 纯函数单测 (编排与口径都在 Rust 侧)
cd src && cargo test autoloop sysparam llm
```


前置与 loop_v1 相同: 设备已 root, 原神已在大世界探索态, 并且在光照稳定窗口内跑
(昼夜循环是本负载最大的不可重复性来源, 见 loop_v1/README.md)。

## 负载: 必须先确认视角真的在转

原神 **7.1.0 之后手柄注入失效**, 而且失效是**安静的** —— `phonefarm script` 照常
跑完、退出码正常、ftrace 照样有帧时序, 只是画面根本没动, 采到的是静止场景。
这种数据看起来完全正常, 混进 A/B 比较里没有任何一处会报错。

所以:

- 默认负载已切到触控拖拽版 `scripts/workload_spin_touch_v1.json`
  (`(900,400) → (2000,400)` 匀速 12 秒 × 3)。**不从屏幕中心拖** —— 中心点会打开
  派蒙菜单。手柄版 `workload_spin_v1.json` 保留供回溯历史证据, 不要再用它开新实验。
- `autoloop.py` 在跑基线之前先做一次**负载自检**: 拖拽进行中抓两帧原始帧缓冲,
  数有多少像素真的变了。转视角时整个画面在平移, 绝大多数像素都会变; 静止场景只有
  草、云、UI 时钟这类小动画在动。门槛 20%, 离两边都远。不过这一关**一个数都不采**,
  直接退出 (退出码 3), 证据留在 `spin_check/`。

## 现状（2026-09-23 原神实测）

**测试条件**：红魔 NX809J / Android 16 / Adreno 840v2，已 root；原神 **7.1.0** 大世界探索态；
负载 `scripts/workload_spin_touch_v1.json`（触控拖拽转视角；自检画面变化 81.8% / 85.6%，门槛 20%）；
内置主动散热风扇开启（`fan_enable=1, fan_speed_level=5`）；测量期间停充
（`/sys/class/qcom-battery/charging_enabled 1→0`），全程放电态；
5v5 精确置换检验全枚举 252 种，`alpha_win = 0.05/3 = 0.0167`（Bonferroni over 3 指标），
最小可达 p = 0.0079；温度上限 70 °C，实测峰值 63.7 °C。

**重测基线**：`fps_mean` 59.1–59.4、`frame_p50` 16.65 ms、`frame_p95` 19.3–19.6 ms、
整机功耗 4.76 W、SoC 55.2 °C。
**原神已不再被钉在 30 fps** —— 旧基线（30 fps / p95 35 ms）与现在没有可比性。

**跑出判定的两组**（证据 `runs_sysparam/genshin2_run{1,2}/`）：

| 参数 | frame_p95 | fps_mean | power | 判定 |
|---|---|---|---|---|
| `bus.DDR.boost_freq=5333000` + `bus.LLCC.boost_freq=1350000` | −2.93%（p=0.135） | +0.28%（p=0.262） | **+16.5%（p=0.0079，d=3.25）** | **REJECT**：功耗显著变差 |
| `gpu.min_pwrlevel` 17→12 | +2.68%（p=0.079） | **−0.571%（p=0.0159，d=−2.39）** | −2.56%（p=0.429） | **REJECT**：帧率显著变差 |

第一条尤其值得记：把内存总线下限钉到硬件上限，**在 60 fps 的原神上是净亏** ——
多烧 16.5% 的电换不来统计显著的帧时改善。旧结论（p95 −1.61%）是 30 fps 时代测的，
而且当时根本没量功耗。

**没能跑完的候选，以及为什么**：`cpu.policyN.scaling_governor` / `scaling_min_freq`
这一类会被**厂商温控/性能管家**抢：写进去 1 秒回读没问题，十几秒后被压回去；
`scaling_max_freq` 甚至会被压到还原不回原值（想写 1785600、回读 1440000），
只能如实判 ABORT。`gpu.min_pwrlevel` 写 0 会被夹到 2（热限档位）。

---

## 证据目录

```
runs_sysparam/<标签>/
  snap_before.txt        进入前快照
  probe.txt              真机探测原始输出 (含每项的 writable / effect 结论)
  snap_after_probe.txt   探测后快照 —— 与进入前逐行相等才算探测没留痕
  whitelist.json         过了三关的参数与各自的合法取值表
  rule.json              **冻结的判定规则** (含温度上限的推导过程与可达性)
  baseline/base*/        基线轮, 用来定温度上限与确认功耗可测
  gen<N>/prompt.txt      喂给模型的完整 prompt
  gen<N>/llm_raw.txt     模型原始回包
  gen<N>/candidates.json 校验后真正上机的参数组 (含被作废的原因)
  gen<N>/cand<M>/
    plan.txt             下发到设备的 plan
    knob1..5/ ctrl1..5/  每轮的 trace.txt / summary.json / env.txt / metrics.json
    result.json          置换检验结果 + 判定 + 进出快照比对
  snap_final.txt         收尾快照
  report.json            全局结论
```

`trace.txt` 是大文件, 已在 `.gitignore` 里排除; 其余 JSON 证据入库。
