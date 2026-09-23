# loop_v1 — 最小可信闭环

对一个**可重复**的游戏负载, 无人值守地跑通一次完整的性能优化闭环:

```
选负载 → 量基线 → 归因 → 改旋钮 → 复量 → 统计判定 → 回滚 → 留证据
```

本轮只验证**闭环本身成立**, 不追求优化幅度 (改善 1% 也算过), 不追求覆盖多个优化域,
不跨设备, 不碰 NN。

设备: NX809J (红魔, Android 16 / SDK 36 / 骁龙 canoe / **Adreno 840v2** / Magisk root)
负载: 原神 `com.miHoYo.Yuanshen`, 蒙德城定点匀速转视角

---

## 为什么不用现成的那些测帧手段

开工前逐个试过, 在这台机器 + 这个游戏上都不成立:

| 手段 | 结果 |
|---|---|
| `dumpsys SurfaceFlinger --latency` | **已失效**。Android 16 上只回一行 vsync 周期 (16666666), 没有逐帧数据 |
| `dumpsys gfxinfo <pkg>` | **测不到游戏帧**。原神走 SurfaceView, 绕开 HWUI。实测 48 秒里 "Total frames rendered" 纹丝不动停在 43 |
| phonefarm 自带遥测的 FPS 字段 | 同上, 底层就是 gfxinfo, 整局报 `FPS:0` |
| Perfetto `gpu.renderstages` / `gpu.counters` | **驱动没注册**。本机 91 个数据源里 GPU 相关只有 `android.gpu.memory` 与 surfaceflinger 那几个, 高通没在这个驱动上出 GPU producer。所以 render pass 级归因在本机做不到 |

最终用的是 **raw ftrace 的 kgsl 事件**: 纯文本、root 可开、不侵入游戏进程,
一次采集同时拿到帧节奏 + GPU 执行时长 + 带宽 + 热降频。

---

## 一帧不等于一次 GPU 提交

原神在本机 **每帧发 2 次 cmdbatch**, 提交间隔严格交替 (~4ms / ~29ms), 每对之和 33.3ms = 30fps。

直接把提交间隔当帧间隔会得出 "58.9 fps / 帧时间 17ms" 的错误结论。所以解析器每次都从
数据自检每帧几次提交 (`detect_submits_per_frame`), 不写死。

自检用**提交间隔序列的自相关**, 不用方差:

- 方差法 (取归一化 CV 最小的 N) 在 night4 上判成 6, 帧时间虚高 3 倍 (100.6ms 而非 33.5ms)。
  根因是把 N 个间隔求和天然按 1/sqrt(N) 缩小 CV, 而帧内间隔又彼此相关, sqrt(N) 归一化
  消不掉残余偏置 —— night1 上 N=2 只比 N=6 差 3.5% (容差救得回来), night4 差 18% (救不回来)。
- 自相关直接量周期性: 交替序列 lag1 强负、lag2 强正。实测所有 run 都是
  **lag1 ≈ -0.93, lag2 ≈ +0.90**, 判别余量极大, 不靠任何容差。

---

## 目录

```
tools/
  device_snapshot.sh    设备可变状态快照 (38 行), 进入前/退出后 diff = 判据 4
  ftrace_capture.sh     ftrace 采集, 自带四项状态存档与 trap 还原
  knob_ddr_boost.sh     旋钮: DDR/LLCC 总线下限钉到硬件上限, apply/restore/status
  pf_bin.sh             解析 phonefarm 二进制路径 (供各脚本 source)
  analyze.py            离散度 + 漂移 + 精确置换检验 + 置换反演 CI (判据 1/3)
  report.py             五条判据汇总成一份可字节复现的 report.json
  replay_test.sh        离线回放自检 (判据 5)
  run_once.sh           一轮"负载 + 采集"的编排
scripts/
  workload_spin_v1.json 定点匀速转视角负载
fixtures/             切片过的真实 trace + 旧 Python 版输出, Rust 侧 golden 对照用
runs/<标签>/
  trace.txt             原始 ftrace (唯一事实来源)
  summary.json          解析产物
  attribution.json      归因产物
  snap_at_run.txt       轮内快照 (证明旋钮生效)
  snap_after_run.txt    轮末快照
  workload.log          phonefarm script 日志
```

下游全是原始 trace 的纯函数 —— 不读时钟、不读设备、不用随机数。置换检验全枚举而非抽样,
分位数用固定插值, CI 用固定网格扫描。所以同一份 trace 每次重算逐字节一致。

---

## 负载设计与它的硬约束

`workload_spin_v1.json`: 手柄右摇杆匀速水平旋转 36 秒, 不走位、不按任何功能键。

- **不按 A 键**。前人的 `scenario_fixed_spot_v1.json` 每轮按 4 次 A —— 在原神里 A 是交互/确认,
  一旦身边有可交互物就会开箱子或触发对话, 直接毁掉可重复性。
- **不推左摇杆**, 所以人物位置由构造保证不变。
- **匀速连续旋转**而非分段转停, 让 GPU 负载在一圈内自然平均, 起始朝向的影响被摊薄。

### 必须在光照稳定窗口内跑

昼夜循环是本负载最大的不可重复性来源, 实测数据:

| 批次 | frame_p95 逐轮 (ms) | 离散度 | gpu_active 单调比 |
|---|---|---|---|
| 跨黄昏 (昼→夜) | 36.33 / 35.99 / 36.29 / **37.93** / 36.68 | **5.35%** ❌ | **1.00** |
| 深夜窗口 | 36.28 / 36.30 / 36.13 / 35.85 / 36.34 | **1.37%** ✅ | 0.25 |

黄昏批次的 `gpu_active_mean` 单调比 = 1.00, 即**每一轮都比上一轮高** (21.68 → 22.66,
+1.13%/轮)。这不是抖动, 是有外生变量在单向移动。避开光照过渡后离散度降到 1/4。

所以 `analyze.py` 除了离散度还报**漂移**: 最小二乘斜率 + 相邻递增比例。
离散度只说"散得多开", 说不出"是不是一直往一个方向走" —— 单调比贴近 1 时,
即使离散度达标也不算可重复。

游戏内时间本应在每轮前钉死, 但原神的时间表盘对 `input swipe` / `input motionevent` /
手柄推杆都无响应 (三种都试过), 未解。当前靠"在深夜窗口内跑完"规避, 这是已知的脆弱点。

---

## 用法

```bash
# 前置: 设备已 root, 原神已在大世界探索态
adb push tools/device_snapshot.sh tools/ftrace_capture.sh tools/knob_ddr_boost.sh /data/local/tmp/
adb shell chmod 755 /data/local/tmp/{device_snapshot,ftrace_capture,knob_ddr_boost}.sh

# 基线快照
adb shell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > runs/snap_before.txt

# 交错跑 A/B (旋钮/对照), 让残余漂移对两臂等量影响
for i in 1 2 3 4 5; do
  adb shell "su -c 'sh /data/local/tmp/knob_ddr_boost.sh apply'"
  bash tools/run_once.sh knob$i runs/knob$i
  adb shell "su -c 'sh /data/local/tmp/knob_ddr_boost.sh restore'"
  bash tools/run_once.sh ctrl$i runs/ctrl$i
done

# 出报告 + 回放自检
python3 tools/report.py --baseline 'runs/ctrl*' --knob 'runs/knob*' \
    --snap-before runs/snap_before.txt --snap-after runs/snap_final.txt > runs/report.json
bash tools/replay_test.sh runs
```

---

## 全自动版本: 连「挑哪个旋钮」也交给机器

上面这套是手工挑一个旋钮跑一次。`auto/` 把挑旋钮这一步接进循环:
真机探出可写系统参数的白名单 → 冻结判定规则 → 大模型每代挑几组参数 →
逐组「等冷 → A/B 交替 → 置换检验 → 保留/淘汰 → 还原」→ 结果喂回模型挑下一组。

```bash
python3 auto/autoloop.py --out runs_sysparam/<标签> --generations 2 --children 3 --pairs 5
cd auto && python3 -m unittest      # 纯函数单测
```

判定看三样 (帧时 p95 / 帧率 / 整机功耗), 不是只看省电; 温控保护相关的节点永远不进
白名单。白名单、判定口径、旋钮实现与证据目录说明见 [`auto/README.md`](auto/README.md)。
