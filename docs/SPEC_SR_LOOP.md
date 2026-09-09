# SPEC: SR_LOOP — 端侧超分网络自主进化环 (v1.1 落地契约)

> 状态: 实施中 · 2026-09-09 起 · **Gate 0 已通过** (2026-09-09, 证据 sr_loop/runs/gate0/report.json), Gate 1~4 逐门补记
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
- `budget()` 解析式计 params 与 FLOPs (= 2×MACs, 在 `input` 分辨率上); 超限直接不上真机。
- `genome_to_model` 确定性: 每层派生独立种子, 同一基因组两次生成权重逐位相同。
- 指纹 = 去掉 `id` 后的规范化 JSON 的 sha256 前 16 位, 用于去重 (同构基因组不重复上真机)。

Gen 0 = 上述 ESPCN 两变体 (`gen0-espcn-ps` / `gen0-espcn-bc`): params 1706 / 1211, FLOPs 1.74 G。

## 4. `phonefarm capture` (Gate 2, 待定稿)

原神无 UI 自动巡航截图: 复用 `src/plugins/genshin.rs` 的巡航能力作为独立 CLI 子命令输出原始帧, 不改插件决策;
对齐切块、Bicubic PSNR (> 28 dB) 与相位相关平移校验在 sr_loop 离线脚本完成, 丢弃率 > 5% 熔断。本节在 Gate 1 通过后补全。

## 5. 测试契约

- `src/bench.rs` 全部解析器为纯函数并单测: 覆盖行 / 计时行 / 算子档案 (只取 Regular 段 Run Order) / 快照往返 /
  热区筛选 / 离散度 / 采样器众数 / 参数形态。`cd src && cargo test` 100% 通过。
- 真机验收 (Gate 0): 见 §1 表; 证据是两份 `bench --json` 输出, 归档在 sr_loop 仓库 `runs/gate0/`。
  2026-09-09 结果 (`python -m sr_loop.gate0`, GATE0_PASS): gen0-espcn-ps 两次冷机 1.099 / 1.088 ms (轮内离散 1.36% / 0.37%,
  复测差 1.01%); gen0-espcn-bc 1.682 / 1.674 ms (1.07% / 0.78%, 复测差 0.48%); 四次调用全部 4/4 节点 OpenCL 单分区,
  gpuclk / DDR / LLCC 采样众数均等于锁定值, 每轮锁态回读恢复。
