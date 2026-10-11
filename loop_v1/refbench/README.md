# loop_v1/refbench — 白盒靶子的驱动面

驱动 [HGamey/refbench](https://github.com/HGamey/refbench) 的确定性 Vulkan 负载跑 M0
六条判据。靶子只提供负载/旋钮/ground truth；采集、解析、归因、统计判定全在这边
（复用 `../tools/` 的纯函数链，回放纪律不变）。

```
bash run_m0.sh                    # 全套电池, 零交互, 产物在 ../runs_refbench/
bash run_refbench.sh <label> <outdir> <scene> <intensity> <loadop> <frames>   # 单轮
phonefarm refbench-report --root ../runs_refbench                            # 六判据汇总
```

要点:

- **接口契约**在靶子仓库 `contract/launch.json`：intent 字符串 extras 进，
  `refbench_out.json` 出，进程渲染完固定帧数自退（`pidof` 存活轮询判结束）。
- **trace 过滤**用 `--comm RefbenchDrv`：Adreno Vulkan 驱动在进程内部线程发 kgsl
  提交，靶子把它们改名成稳定契约（细节见靶子 DESIGN.md §2）。每帧恰 1 次提交。
- **无效轮纪律**：采集窗内出现热事件或非干净退出 → 该轮标 `INVALID`，证据保留、
  样本不计、就地重试（至多 2 次）。每轮起跑前有 GPU 温度回落轮询；跑测期间开
  设备风扇、结束还原（风扇不在 38 行快照内，单独存取还原）。
- **判据 4 的映射**（归因结论 vs 靶子自报瓶颈）与全部阈值冻结在 `rules.json`
  （数据，不是代码），报告里的 `rules_sha256` 就是**这份文件的 sha256**——改任何
  一个值，新报告的哈希跟着变（harness v2 判据 5 的种子）。报告本体由
  `phonefarm refbench-report --root <runs目录>` 产出。
- **口径切换点（2026-09-23）**：切换前的报告哈希对应当时的判读源码（原封留在
  `rules_frozen.py`，生成过 `runs_refbench/report.json` 的那份）。因此切换点之前
  的归档在 replay 时会恰好差 `rules_sha256` 一行——记录在案的口径切换，不是回放
  坏了；切换点之后新产的证据哈希互相一致。
- **harness v2 衔接**：`run_refbench.sh` 是刻意收窄的负载专属面；负载插件接口
  落地后本文件整体即插件实现，编排一行不用改。

M0 六判据（refbench DESIGN.md §8）：可重复 <0.5% 且单调比 <0.75 · 强度阶梯必须
推动帧时间 · 旋钮两臂 p<0.05 · 三变体归因与自报一致 · 快照零差异 · 离线回放
字节一致。

## selftest：判定链的已知答案真机自检

`phonefarm selftest` 在 refbench 上出「答案事先知道」的考题，看现有判定链答得对不对。
负载是 refbench 仓库的 `selftest/load.spv`（装进 `sr_pipeline` 的 postfx 插槽），
两臂同一份字节码，只改特化常量「多少个工作组跑重循环」——负载差由构造给定。

| 考题 | 构造 | 应当的判定 |
|---|---|---|
| `aa` | 两臂完全相同 | 不判 KEEP，也不报显著变差（误判率应 ≤ α） |
| `better:<帧时%>` | B 臂少跑若干工作组 | KEEP；多档幅度 → 检出率曲线 → MDE |
| `worse:<帧时%>` | B 臂多跑若干工作组 | REJECT 且列出显著变差的指标 |
| `quality` | B 臂少跑 + 采样降分辨率 | 画面判据 POOR_QUALITY；只少跑不降采样的对照不得被拦 |

```
phonefarm selftest calibrate --out D/cal --levels 0,4000,8000 --rounds 2 --fan-on   # 帧时 ≈ t0 + k·工作组数
phonefarm selftest run --out D/run --calib D/cal/calib.json \
    --cases aa*4,better:0.5,better:1,better:2,better:5,worse:2 --pairs 10 --fan-on
phonefarm selftest quality --out D/q --calib D/cal/calib.json --pct 5 --downs 2
phonefarm selftest report --root D/run --ns 5,8,10           # 不碰设备, 可离线重算
```

- **判定不另写统计**：帧时/帧率/功耗走 `loopstat::compare`（精确置换）+ `sysparam::decide`，
  与 `autoloop` 同一份代码、同一口径（alpha_win = 0.05/K、alpha_regression = 0.05）；
  判定视图用 `eval::run_view`（summary 并上 env 的功耗/温度）。画面走
  `phonefarm eval --request`（kind=shader，gpu-op 的画质地板），子进程调用。
- **报告**：逐题在前 5/8/10 轮上的判定、混淆矩阵（每个 n 一张）、按幅度的检出率表、
  A/A 噪声（CV）、注入法 MDE（在 A/A 真实噪声上叠已知效应扫 0.25%–10%，只作平滑补充，
  与实测检出率对不上以实测为准）、每个 n 的时间账（分钟/实验）。
- 每臂流程与 `eval`/`autoloop` 的 run_one 同构：等冷 → 起负载 → ftrace + sample_env 并行
  → 解析。ABBA 交替；基础设施失败重试一次（不看量到的数）；量到但算子没生效
  （`apply_ok=false`）的臂由判定链按 ABORT 处理，不计入。
- 现场：停充、风扇（`--fan-on`）、可选锁频（`--lock-freq`）都在会话 guard 里，任何返回路径
  都还原；改之前的值存在 `~/.local/state/phonefarm/`（`PHONEFARM_STATE_DIR` 可改），
  宿主机崩溃重启后下次启动能回滚。`selftest` 和其他碰设备的子命令一样自己拿设备锁
  （`--lock-label`），`selftest report` 不拿锁。
- **分段跑**（单次持锁 ≤ 45 分钟）：各段给同一份 `--cases` 与同样的条件，用 `--only aa_3,better1_1`
  挑本段要跑的考题编号、各写一个新目录；之后把各段的考题目录和其中一份 `plan.json` 合到一个根下跑
  `report`（各段 `plan.json` 一字不差，可核对）。走手机节点时就是每段一个 `pf-node -o … --out @out/…`。
  同一目录续跑时已完成的考题自动跳过；`--budget-min`（缺省 40）估不下一整道就不开。
- 证据：原始 ftrace 在设备端先按解析器的预筛条件过滤再拉回、本地 gzip，一臂几百 KB。

**现状（2026-10-11）**：判定、报告、SPIR-V 改写有离线单测。真机只完成了标定（帧时随工作组数线性，
残差 ±0.05%）；A/A、已知变好/变差、画质作弊考题还没跑。进度、数字和查清的现场问题（ftrace 被冲掉、
原神留前台发热、GPU 频率上限中途被改、满电 AC 停充不生效）见 [判官体检报告](JUDGE_CHECKUP.md)。