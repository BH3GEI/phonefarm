# SPEC: SR_LOOP — 端侧超分网络自主进化环 (v1.1 落地契约)

> 状态: 实施中 · 2026-09-09 起 · **Gate 0/1/2 已通过** (2026-09-09; 证据 sr_loop/runs/gate0/report.json, runs/gate1/, runs/gate2/report.json:
> 数据集 A 150 帧 1440/360 块, 丢弃率 0%, Bicubic 基线 37.286 dB, Gen 0 ps +1.376 dB / bc +1.537 dB), Gate 3~4 进行中
> 目标: 依托红魔 NX809J (SM8850 / Adreno 840, Magisk root) 与 phonefarm, 构建
> "候选生成 → 零样本真机秒筛 → 画质短训 → Pareto 归档 → 反馈迭代" 的全自主闭环,
> 验证结构变异能否在物理硬件约束下推动超分模型正向演进。
> 代码分工: **phonefarm 只提供 `bench` / `capture` 两个独立 CLI 能力** (本仓库);
> 基因组、模型生成、短训、归档、代际推进全部在独立仓库 `~/projects/sr_loop` (Python)。

## 0. 铁律 (来自 SR_LOOP_SPEC v1.1, 逐条映射到实现)

| # | 铁律 | 实现落点 |
|---|---|---|
| 1 | 严禁日历时间预设与估算, 全程条件门禁状态机驱动 | 本文 §1 的门禁只写"进入条件/退出条件"; 代码里唯一的秒数是安全阀 (等冷超时、adb 超时), 不参与任何判定 |
| 2 | 严禁算力倒挂: 未初始化模型先上真机, GPU 延迟 > 4.0ms 或 CPU 回退一票否决 | `phonefarm bench` 的 `summary.feasible` = 全图 GPU 且 median <= limit; sr_loop 只对 feasible 的基因组短训 |
| 3 | 严禁人工评分: 延迟/算子分布/PSNR/对齐度全部程序与物理设备产出 | bench 的每个数字都解析自 benchmark_model 真机日志与 sysfs; PSNR/对齐由 sr_loop 脚本算 |
| 4 | 跑测分离: phonefarm 只做 CLI, 不侵入游戏, 不改决策核心 | `src/bench.rs` 纯增量子命令, 不碰 runtime/universal/plugins; sysfs 写入全部在退出前恢复 |

## 1. 门禁状态机

每个门只有"进入条件"和"退出判据", 退出判据全部可由程序断言。不满足退出判据就停在本门, 不进下一门。

| 门 | 进入条件 | 退出判据 (程序断言) |
|---|---|---|
| Gate 0 物理标尺 | 设备在线 + root | `bench` 对 Gen 0 两个变体: `full_gpu=true`, 3 轮 `dispersion_pct<=5`, 两次独立冷机调用的 median 相差 <= 5% |
| Gate 1 零样本秒筛 | Gate 0 通过 | 任意合法基因组 → `genome_to_model` → TFLite → `bench --json` 得到 `feasible` 布尔; 预算校验 params<=50000, FLOPs<=2G 由 `sr_loop.genome.budget` 程序判定; 回退模型在首轮即被否决 (`sr_loop.screen` 状态机: BUDGET_FAIL / INVALID / EXPORT_FAIL / FALLBACK / UNSTABLE / SLOW / FEASIBLE) |
| Gate 2 数据与基线 | Gate 1 通过 | 数据集 A 含 `split.json`; Bicubic PSNR 锁定; 丢弃率 <= 5% (超过熔断); Gen 0 短训后 PSNR - Bicubic >= 0.5 dB |
| Gate 3 单代闭环 | Gate 2 通过 | 变异 → 秒筛 → 短训 → Pareto → `gen_N.json` 全程无人工介入、无异常阻断 |
| Gate 4 20 代演进 | Gate 3 通过 | 20 代可行域内最高 PSNR 序列的 Spearman rho 与 p-value 报告落盘 |

## 2. `phonefarm bench` 命令契约

```
phonefarm bench --serial <设备> --model <PATH.tflite> [--runs 3] [--max-rounds 6] [--json]
                [--num-runs 100] [--warmup 20] [--limit-ms 4.0] [--metric gpu|invoke] [--threads 1]
                [--gpu-level N | --gpu-mhz M] [--no-lock] [--cool-c 40] [--cool-timeout-s 600]
                [--fp32] [--bin <benchmark_model>] [--out <目录>] [--keep]
phonefarm bench --serial <设备> --unlock        # 回滚上次异常退出遗留的锁频态
```

- 退出码: `0` = PASS; `1` = 测量有效但判定失败 (`FAIL_FALLBACK` / `FAIL_UNSTABLE` / `FAIL_LATENCY`);
  `2` = 用法错 / 设备错 / 测量无效 (`ERROR`)。
- `--json` 时 stdout 只有一个 JSON 对象 (schema v1, 见 §2.4); 进度一律走 stderr。
- 只支持 Android/adb 设备 (kgsl + TFLite GPU Delegate); hdc 目标直接拒绝。
- 不读 phonefarm.toml, 不写 tasks/, 不烧 token。

### 2.1 每轮协议 (顺序即契约)

| # | 步骤 | 判定 / 说明 |
|---|---|---|
| 1 | 未锁频等冷 | SoC 结温热区 (`cpu-*` / `cpullc*` / `gpuss*`, 跳过 0 与 >=100C 哨兵值) 最高值 < `--cool-c` 才放行; 皮肤温不参与 |
| 2 | 锁频 | 每个 `cpufreq/policy*` 写 `performance`; kgsl `max_pwrlevel` 与 `min_pwrlevel` 同写目标档 (顺序 max→min→max, 兼容 kgsl 互钳位); `bus_dcvs/{DDR,LLCC}/boost_freq` 写 `hw_max_freq` (总线钉顶); 回读不符立即回滚并报错 |
| 3 | 跑测 | `benchmark_model --use_gpu=true --gpu_precision_loss_allowed=<fp16> --enable_op_profiling=true --num_runs N --min_secs=0 --warmup_runs W --warmup_min_secs=0`; 同时一路 root 采样器每 250ms 记 `gpuclk`、各簇 `scaling_cur_freq`、DDR/LLCC `cur_freq` |
| 4 | 立刻解锁 | 写回锁前快照 (调速器 / kgsl 档位 / boost_freq); 回读一致才算 `restored=true` |
| 5 | 干扰判定 | 该轮门禁口径分布的变异系数 std/avg > 5% 记 `clean=false` (总线抢占 / 后台突发), 不进判定窗口 |
| 6 | 窗口判定 | 最近 `--runs` 轮全部 clean 且离散度 <= 5% → 窗口成立, 停; 否则继续下一轮, 直到 `--max-rounds` (缺省 runs+3) 用尽 → `FAIL_UNSTABLE` |

首轮即见 CPU 回退或 `ERROR:` 的模型直接定案, 不再多测 (零样本秒筛要快)。

**为什么锁频只包住跑测那几秒**: 2026-09-09 实测, 先锁再等冷时 `performance` 调速器让 8 核待机在最高电压,
结温从 33C 爬到 48C, 40C 门禁永远过不了; 进程被杀后设备还停在锁频态。因此:
锁前把快照写到本机 `$TMPDIR/phonefarm-bench-lock-<serial>.txt`, 下次任何 bench 启动或 `--unlock` 先按它回滚。
kgsl 的 `force_clk_on / force_bus_on / force_rail_on / force_no_nap` 在此内核 (6.12, Adreno 840) 写入即失败, 不纳入协议;
`bus_dcvs/*/hw_min_freq` 是内核私有只读节点 (root 亦 EACCES), 用户态下限是 `boost_freq` (写入后 DDR 即刻 547→5333 MHz, 回写即刻回落, 2026-09-09 实测)。

**为什么要钉总线**: 未钉时 bilinear_conv 变体 3 轮 GPU 内核时延 2129/2144/2243 us (离散 5.32%), 每轮 per-run 最小值却稳定在 2084~2093 us,
即噪声是访存算子 (`resize`) 遇到 DDR DCVS 迟滞的突发慢迭代, 不是频率漂移; pixelshuffle 变体 (纯算) 同一时段 1714/1720/1719 us (0.35%)。

### 2.2 延迟口径 (定论)

| 口径 | 定义 | 来源 | 用途 |
|---|---|---|---|
| `gpu` (缺省) | GPU Delegate 各内核 avg 之和 | benchmark_model 算子档案里 `Delegate/` 前缀行 (OpenCL 事件计时) | **门禁口径**: 只含模型算子, 随结构变化 |
| `invoke` | 端到端 Invoke 均值 | `Inference (avg)` 行 | 参考: 含 CPU<->GPU 张量拷贝与同步 |

2026-09-09 NX809J 未锁频实测 Gen 0 (540x960→1080x1920, fp16 计算): `gpu` 1.72 ms, `invoke` 8.71 ms。
差值 7 ms 来自每帧 31 MB 的输入上传与输出回读 (fp32 NHWC), 与网络结构无关; 端侧部署走 GPU 常驻缓冲区不付这笔钱。
门禁若用 `invoke`, 可行域在该分辨率下为空集, 进化无从开始; 故门禁口径为 `gpu`, `invoke` 保留在报告里作参考。

### 2.3 判定

- `full_gpu`: 日志 `Replacing N out of N node(s) ... yielding 1 partitions`, 算子档案无任何非 `Delegate/` 行, 后端为 OpenCL
  (`Initialized OpenCL-based API`), 且无 `ERROR:` 行。命令行强制 `--gpu_backend=cl`: 不这样做时 OpenCL 不认的算子会让 delegate
  静默退到 OpenGL 后端 (无 per-op profiler, 也不是被校准的路径; 2026-09-09 `floor_mod` 实测); 强制后表现为
  `TfLiteGpuDelegate Init: No selector for floor_mod` → delegate 申请失败 → benchmark 中止。
- 只要日志里出现过 `Created TensorFlow Lite delegate for GPU` 而整图没落在 OpenCL 上 (覆盖不满 / 多分区 / CPU 行 / 后端非 CL /
  申请失败 / benchmark 中止), 一律 `FAIL_FALLBACK` (退出码 1), `summary.fallback_reason` 给出 CPU 算子名、覆盖率、后端与错误行,
  作为上层变异器的反馈。`ERROR` 只留给与模型 GPU 兼容性无关的失败 (模型加载不了、设备无心跳、无输出)。
- `dispersion_pct` = (Max − Min) / Median × 100, 样本是判定窗口内各轮 `gpu_kernel_us` (或 `invoke_avg_us`); 上限 5。
  窗口 = 最近 `--runs` 轮连续 clean 且离散度达标的那一段 (`summary.window` 给出 1 起的 [起, 止]); 没有窗口即 `FAIL_UNSTABLE`,
  报告里仍如实给出最后 N 轮的统计与每轮 `clean` / `cv_pct`。
- `within_limit`: 各轮 median (ms) <= `--limit-ms`。
- `feasible` = `full_gpu && within_limit` (Gate 1 用它, 与离散度无关: 不稳定的测量会以 `FAIL_UNSTABLE` 退出码 1 让上层重测)。
- verdict 优先级: ERROR > FAIL_FALLBACK > FAIL_UNSTABLE > FAIL_LATENCY > PASS。

### 2.4 JSON schema v1 (要点)

```
v, ok, verdict, serial, device{model,platform,soc,android,sdk}, model{path,bytes,sha256}, bench_bin,
params{runs,num_runs,warmup,threads,fp16,metric,limit_ms,cool_c},
lock{enabled, gpu{model,level,hz,num_levels,level_before}, cpu_before[], bus[{name,pin_khz,floor_before_khz}], recovered_stale, restored_all},
delegate{replaced,total,partitions}, backend, ops[{type,avg_ms,pct,name,gpu}],
rounds[{round, clean, cv_pct, gpu_kernel_us, invoke_avg_us, invoke{count,first,min,max,avg,std,median,p5,p95}, profile_total{...},
        init_us, first_us, warmup_avg_us, delegate, backend, full_gpu, ops[], error,
        thermal{start_zone,start_c,waited_s,end_zone,end_c},
        delegate_attempted, fallback_reason, lock{applied,verified,gpu_verified,bus_verified,restored,cpu[],bus[],gpu_thermal_level,gpu_throttled},
        gpuclk_hz{mode,min,max}, cpu_khz[{mode,min,max}], bus_khz[{name,mode,min,max}], wall_ms, log}],
summary{metric, rounds_total, rounds_interfered, window, latency_ms, min_ms, max_ms, dispersion_pct, dispersion_limit_pct, dispersion_ok, fallback_reason,
        limit_ms, within_limit, full_gpu, gpu_lock_verified, feasible, errors[]}, wall_s
```

`lock.verified` = 跑测期间 `gpuclk` 众数等于目标档频率, 且 DDR/LLCC `cur_freq` 众数等于钉顶值 (采样器地面真值), 不信任写入返回值。

### 2.5 标尺的固定参数 (Gate 0 定稿)

- 标准输入形状 `[1, 540, 960, 3]` float32, `scale=2` → 输出 `[1, 1080, 1920, 3]`。理由: 1080p 是手游主流渲染目标,
  2x 超分从 540p 起步; 预算 FLOPs<=2G 在该分辨率对应 ~1.9k MACs/像素, 与 4.0ms 门禁量级匹配。
- TFLite 文件权重 fp32 (参数 <= 50k 时 < 200KB, 避免 DEQUANTIZE 节点); GPU 计算 fp16 (`precision_loss_allowed`)。
  导出必须冻结变量 (`convert_variables_to_constants_v2`), 否则 Keras 3 会导出 VAR_HANDLE/READ_VARIABLE/TRANSPOSE, 整图退回 CPU。
- GPU 档位缺省 = 厂商当前 `max_pwrlevel` (NX809J: 档 3 = 902 MHz, 共 18 档 160~1200 MHz), 不越过厂商正常模式上限;
  `--gpu-level` / `--gpu-mhz` 可改, 但同一轮进化必须固定。
- `--num-runs 100 --warmup 20 --threads 1`; benchmark_model 取官方 nightly 预编译 (tools/tflite/fetch.sh)。

## 3. 基因组规范 v1 (sr_loop/genome.py, Gate 1 契约)

```json
{"v": 1, "id": "gen0-espcn-ps", "input": [540, 960, 3], "scale": 2,
 "layers": [{"op": "conv", "k": 5, "c": 8, "act": "relu"}, {"op": "conv", "k": 3, "c": 6, "act": "relu"}],
 "upsample": "pixelshuffle", "seed": 0}
```

- `op` ∈ conv | dwsep (depthwise k + pointwise 1x1) | res (conv-act-conv + 残差, 要求 c == 输入通道);
  `k` ∈ 1/3/5/7; `c` ∈ 1..64; `act` ∈ relu/relu6/tanh/linear; 1..16 层。
- `upsample`: `pixelshuffle` = conv k3 → 3·r² 通道 → depth_to_space(r); `bilinear_conv` = resize_bilinear(×r) → conv k3 → 3。
- `skip` (v1 追加, 缺省 none): `bilinear` = 输出加上双线性放大的输入 (全局残差, TFLite 为 RESIZE_BILINEAR + ADD, GPU 支持)。
  2026-09-09 实测定论: 无跳连的 Gen 0 短训 2000 步 val PSNR 34.7 dB, 比 Bicubic 基线 (38.7 dB) 低 4 dB, 步数全耗在学直流与上采样;
  加跳连后 500 步即 39.4 dB, 2000 步 40.5 dB (+1.8 dB)。Gen 0 两变体均带 skip=bilinear; 变异器可自由切换该字段。
- `budget()` 解析式计 params 与 FLOPs (= 2×MACs, 在 `input` 分辨率上); 超限直接不上真机。
- `genome_to_model` 确定性: 每层派生独立种子, 同一基因组两次生成权重逐位相同。
- 指纹 = 去掉 `id` 后的规范化 JSON 的 sha256 前 16 位, 用于去重 (同构基因组不重复上真机)。

Gen 0 = 上述两变体 (`gen0-espcn-ps` / `gen0-espcn-bc`, 均 skip=bilinear): params 1706 / 1211, FLOPs 1.80 G (含跳连)。

## 4. `phonefarm capture` 与数据集 A (Gate 2)

```
phonefarm capture --serial <设备> [--out <目录>] [--frames 200] [--max-steps N] [--settle-ms 800] [--mode auto] [--no-shutdown] [--json]
```

- 复用 `plugins/genshin.rs` 的公开生命周期与单步 (`ensure_game_ready` / `step` / `shutdown_and_lock`), **不改插件决策**;
  两步之间用 `adb exec-out screencap -p` 抓原始 PNG, 不重编码。登录/穿门/掉线重连不是本命令的职责: 人先进到大世界即可。
- 只保留同时满足三条的帧: 横屏; `classify_state == OpenWorldExplore`; `hud_present` 小地图与技能栏都在场 (第二道证据)。
  其余按状态计数跳过; 同一非探索态连续 6 步不变发一次 BACK (弹窗通用关闭键), 有界计数。
- 冻结去重: 64x36 灰度缩略图与上一保留帧平均绝对差 < 1.0 视为画面冻结, 跳过。
- `manifest.jsonl` 每行: i / file / ts_ms / step / state / w / h / bytes / sha256 / segment (每 10 张保留帧一段) / diff_prev;
  结束写 `capture.json`。退出码 0 = 收满 `--frames`; 1 = `--max-steps` 用尽; 2 = 用法/设备错。
- 缺省写 `tasks/sr_capture_<stamp>/` (DESIGN 写入权限); `--out` 显式指定则写该目录。

### 4.1 离线对齐切块 (sr_loop/dataset.py) → 数据集 A

| # | 步骤 | 判定 |
|---|---|---|
| 1 | 取横屏探索态帧 | manifest state=explore 且 w>h |
| 2 | HR = 全帧 (对齐到 2*scale); LR = PIL 抗混叠 bicubic 下采样 ×1/2 | 像素中心约定与上采样一致 |
| 3 | 相位相关平移校验: bicubic 上采样回 HR 尺寸, 与 HR 在中央区 (0.20,0.15)-(0.80,0.80) 做相位相关, 峰值三点抛物线亚像素 | \|dx\|,\|dy\| <= 0.25 px 否则整帧丢弃 (半像素错位是 SR 数据集的经典暗坑) |
| 4 | 全帧铺 256x256 块, 与任一 HUD 排除区相交的块不要 | 排除区 (相对坐标, 2026-09-09 NX809J 实测帧标定): 顶栏 (0,0,1,0.13); 左上小地图+任务 (0,0,0.17,0.40); 右侧队伍 (0.80,0.13,1,0.50); 右下技能 (0.60,0.58,1,1); 左下摇杆/聊天 (0,0.62,0.25,1); 底部血条 (0.35,0.85,0.65,1); 底栏 (0,0.93,1,1) — "无 UI" 由构造保证 |
| 5 | 逐块 Bicubic 重建 PSNR | PSNR(bicubic_up(LR块), HR块) > 28 dB 才保留 |
| 6 | 熔断 | (PSNR 丢弃块 + 对齐丢弃帧的块) / 可用块 > 5% → 不产出数据集, 退出码 3 |
| 7 | 固定路线划分 | manifest 的 segment: segment % 5 == 4 → val, 其余 train; 同段帧不跨集 |
| 8 | 落盘 | `{train,val}_{lr,hr}.npy` (uint8), `split.json` (帧清单/块坐标/阈值/指纹), `baseline.json` (val 块 Bicubic PSNR 均值 = **锁定的基线**), `dataset_report.json` |

PSNR 定义 (全环统一): RGB 三通道联合 MSE, 像素域 [0,255] (或 [0,1] 等价), 逐块计算后取均值。

### 4.2 画质短训 (sr_loop/train.py)

固定种子 (`tf.keras.utils.set_random_seed`) + 固定步数 + L1 损失 + Adam 余弦退火 (lr 2e-3 → 5%), batch 8 个 128→256 块,
增广只做与种子绑定的翻转。评估 = val 全部块的 PSNR 均值; `delta_db` = PSNR − 锁定基线。训练图用动态空间尺寸的同构模型
(`genome_to_model(g, input_hw=None)`), 权重与导出图逐位同形。所有代际使用同一组 (steps, batch, lr, seed, 数据集指纹)。

### 4.3 Gate 2 验收 (sr_loop/gate2.py)

数据集构建未熔断 → `baseline.json` 锁定 → Gen 0 两变体各短训一次 → 最佳 `delta_db >= 0.5` 才 GATE2_PASS; 证据 `runs/gate2/report.json`。

**真机秒筛的前提**: 跑 `bench` 时目标游戏不得在前台渲染 (前台游戏会占满 GPU, 标尺失真); capture 结束后先 HOME 把游戏切后台
(Android 暂停后台 Activity 的渲染), 再进入秒筛/进化。bench 报告的 `device.focus` 记录当时的前台窗口作为证据。

## 5. 单代闭环与多代推进 (Gate 3 / Gate 4)

### 5.1 一代的状态机 (sr_loop/evolve.py)

| 阶段 | 输入 | 输出 / 门禁 |
|---|---|---|
| 变异 | 父代 = 当前 Pareto 前沿 (可行且已短训); 上一代全部候选及结局 | `sr_loop/mutate_llm.py` 向 LLM 要 `--children` 个基因组; 提示词只含语法/预算/真机反馈, **不含任何论文或网络名**; id 由程序分配; 解析失败/重复/非法逐个剔除; 不足用随机变异补齐并标注 `source=random` |
| 秒筛 | 每个子代 | `sr_loop/screen.py`: BUDGET_FAIL / INVALID / EXPORT_FAIL 不上机; 真机 FALLBACK / SLOW / UNSTABLE(重测一次) / FEASIBLE |
| 短训 | 仅 FEASIBLE | 与 Gate 2 同参数; 得 psnr_val / delta_db |
| 归档 | 全部子代 | `archive.json` (指纹去重, 原子写), `gen_N.json` (子代/前沿/LLM 统计/序列项); 已存在的代直接跳过 (可续跑) |

Pareto 前沿: 两目标 (延迟最小, PSNR 最大), 只在 FEASIBLE 且已短训的个体上计算。任何单个候选的异常记为 status=ERROR, 不中断整代;
LLM 全链失败整代仍靠随机变异推进 — 闭环零人工介入的底线。

### 5.2 Gate 4: 20 代与 Spearman

`series` 每代记录: `best_psnr_so_far` (可行域内历史最高 PSNR) 与 `gen_best_psnr` (本代最高)。`sr_loop/analyze.py` 对代序号与两条序列分别做
Spearman 秩相关 (scipy), 输出 rho / p-value / 增益 dB; 结论 "显著正向演进" 当且仅当累计序列 rho > 0 且 p < 0.05。

## 5. 测试契约

- `src/bench.rs` 全部解析器为纯函数并单测: 覆盖行 / 计时行 / 算子档案 (只取 Regular 段 Run Order) / 快照往返 /
  热区筛选 / 离散度 / 采样器众数 / 参数形态。`cd src && cargo test` 100% 通过。
- 真机验收 (Gate 0): 见 §1 表; 证据是两份 `bench --json` 输出, 归档在 sr_loop 仓库 `runs/gate0/`。
  2026-09-09 结果 (`python -m sr_loop.gate0`, GATE0_PASS): gen0-espcn-ps 两次冷机 1.099 / 1.088 ms (轮内离散 1.36% / 0.37%,
  复测差 1.01%); gen0-espcn-bc 1.682 / 1.674 ms (1.07% / 0.78%, 复测差 0.48%); 四次调用全部 4/4 节点 OpenCL 单分区,
  gpuclk / DDR / LLCC 采样众数均等于锁定值, 每轮锁态回读恢复。
