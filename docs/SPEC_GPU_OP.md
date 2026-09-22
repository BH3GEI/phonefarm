# SPEC: GPU_OP — Compute Shader 算子的真机标尺与 A/B 裁决

> 状态: 实施中 · 2026-09-22 起
> 角色: `game_opt_loop` 自主进化闭环的**物理采样与统计裁决端**。
> 上游负责变异与防爆门禁, 本通路只负责在真机上量出数字并做冻结规则判定,
> **不做任何算法判断** —— 快慢好坏由数字说话。
>
> 代码分工: 本仓库提供 `phonefarm gpu-op` 一个独立 CLI 子命令 (`src/gpuop.rs`),
> 共享的设备条件化与功耗遥测在 `src/hwcond.rs`, 统计在 `src/gpustat.rs`。
> 基因组、变异、门禁、Pareto 归档全部在 `~/projects/game_opt_loop` (Rust)。

## 0. 铁律

| # | 铁律 | 实现落点 |
|---|---|---|
| 1 | 不侵入任何游戏进程, 设备上零常驻、零残留 | runner 推到 `/data/local/tmp/phonefarm_gpuop`, 跑完 `rm -rf`; 采样循环有界, adb 断开自行到头 |
| 2 | sysfs 写入退出前全部恢复, 回读一致才算这轮成立 | `hwcond::Lock`; 回读不符即整轮作废并报错 |
| 3 | 判定规则在看到数据之前冻结 | `FrozenRules` 由 `eval_request.protocol` 带入, 代码中无任何事后可调的阈值 |
| 4 | 量不到的指标如实留空, 绝不用默认值填补 | 取不到的记 `NaN`; 功耗轨不可用时当场报错退出, 不报 `0 W` |
| 5 | 严禁人工评分 | 时延来自 runner 的 `VkQueryPool` 时间戳; 功耗来自 `power_supply` sysfs; p 值由 Welch 检验算出 |

## 1. 命令契约

```
phonefarm gpu-op --request <eval_request.json> [--serial S] [--json]
                 [--runner <本地 runner 路径>] [--power-rail usb|battery]
                 [--out 目录] [--cool-timeout-s N] [--gpu-level N]
phonefarm gpu-op --serial S --unlock      # 回滚上次异常退出遗留的锁频态
```

- 退出码: `0` = PASS; `1` = 测量有效但判定失败 (SLOW / POOR_QUALITY / CRASH);
  `2` = 用法错 / 设备错 / 测量无效。与 `bench` 同一套口径。
- `--json` 时 stdout 只有一个 `eval_report` 对象, 进度一律走 stderr。
- 输入输出契约见 `game_opt_loop/contracts/eval_request.schema.json` 与 `eval_report.schema.json`。

## 2. 每轮协议 (顺序即契约)

| # | 步骤 | 判定 / 说明 |
|---|---|---|
| 1 | 本地预检 SPIR-V | 魔数 / 版本 / 单一 GLCompute 入口 / 工作组 <= 1024 / 指令流不越界。**坏字节码绝不下发真机** —— 轻则驱动报错, 重则 GPU 挂起要重启设备 |
| 2 | 功耗轨可用性探测 | 先采 3 个点; 恒为 0 当场报错。避免跑满十分钟才发现量的是 `0 W` |
| 3 | 未锁频等冷 | SoC 结温热区 (`cpu-*` / `cpullc*` / `gpuss*`) 最高值 < `protocol.cool_c` 才放行 |
| 4 | 锁频 | CPU 各簇 `performance`; kgsl `max/min_pwrlevel` 同写目标档; DDR/LLCC `boost_freq` 钉 `hw_max_freq`; 回读不符立即回滚 |
| 5 | 跑测 | `vkop_runner --shader <arm>.spv --seconds N --json`; 同时一路 root 采样器每 250ms 记电压/电流/功率 |
| 6 | 立刻解锁 | 写回锁前快照; 回读一致才算 `restored`。**不一致即整轮作废** |
| 7 | 下一臂 | 回到第 3 步 |

**锁频为什么只包住跑测那几秒**: 沿用 `bench` 2026-09-09 的实测定论 ——
先锁再等冷时 `performance` 调速器让核心待机在最高电压, 结温只升不降, 冷机门禁永远过不了。

## 3. A/B/A/B 交替

`protocol.rounds` 是**每臂**轮数, 总跑测次数 = `rounds x 2`。

交替 (A/B/A/B) 而不是 A 跑满再跑 B, 是防热漂移的关键: 设备跑久了升温降频,
若分块跑, B 臂全程都在更热的机器上, 温度差会被整包算进算子差异里。
交替后两臂均匀分布在整段时间上, 热漂移对两臂的影响一阶抵消。

- A 臂 = 基线算子 (`baseline.spv`), B 臂 = 待测候选 (`candidate.spv`);
  两者必须在同一目录下, 由上游一起下发。
- 基线臂跑不起来 = 工装有问题, 整次作废 (退出码 2);
  候选臂跑不起来 = 一个结论 (`CRASH`), 照常出报告。

## 4. 统计裁决

**Welch 两样本 t 检验 (不假设等方差)**, 实现在 `src/gpustat.rs`, 零外部依赖。

为什么不用 Student 合并方差: A/B 是两段不同的 GPU 负载, 方差本来就不同 ——
候选更重时轮间抖动也更大。合并方差在方差不等时会**低估 p 值**, 即把噪声判成显著,
正是本项目最要防的那件事。

- p 值 = `I_{df/(df+t^2)}(df/2, 1/2)`, 正则化不完全贝塔 (连分式) + Lanczos `ln_gamma`。
- 单测对着标准 t 表与 R 的 `t.test` 参考值钉死, 不只测自洽。
- 每臂样本 < 2 时不出 p 值, 记哨兵 `1.0` 表示「未检验」, **不伪造小 p 值**。

### 判定次序 (不可调换)

1. **崩了** → `CRASH`
2. **超预算** (`operator_latency_ms > budget_ms`) → `SLOW`
3. **画质掉基线** (`psnr_db < quality_baseline_db`) → `POOR_QUALITY`
4. 其余 → `PASS`

第 3 条排在显著性前面是刻意的: 画质是约束不是目标, 哪怕「显著地更快」,
掉了画质也是不合格品。

`is_pareto_improvement` 仅在 **统计显著 (p < `p_threshold`) 且确实更快** 时为真。
快了 0.3% 但 p=0.4 是噪声, 不是改进; 显著但更慢也不是改进 ——
显著性只说明差异真实, 不说明方向对。

## 4.1 画质地板必须与参考图同源

`--reference <帧.png>` 指定画质真值。**中心裁剪**到目标分辨率, 不缩放:
缩放本身就引入一次重采样, 算子再去"重建"这张已被重采样的图, 量出来的 PSNR
里混进了缩放器的特性, 不再只是算子画质。源图小于目标时直接报错, 不做放大 ——
放大出来的"真值"是假的。

**绝对 dB 不可跨参考图比较。** 2026-09-22 实测同一个算子 `gen1_loc1`:

| 参考图 | 基线臂 PSNR | 候选臂 PSNR |
|---|---|---|
| runner 内置程序化图案 | 38.666 dB | 42.826 dB |
| 真机截帧 (2688x1216 中心裁剪) | 29.145 dB | 31.333 dB |

同一个算子差了 11 dB —— 真实内容的高频细节远多于合成图案。

所以画质地板**优先用本场基线臂实测值**, 而不是 `eval_request` 里带来的
`quality_baseline_db` (后者只作兜底)。A/B 本来就是同场同参考图跑的,
用 A 臂的画质当地板是自洽的; 拿别处搬来的常数当地板则会得出荒谬结论 ——
上面那张表里, 用程序化图的地板 38.666 去判真机截帧的 31.333, 会把一个
比基线好 2.19 dB 的算子判成 POOR_QUALITY。

## 5. 功耗口径

两条轨, 各有各的适用条件, **不可混用**:

| 轨 | 读什么 | 适用条件 | 代价 |
|---|---|---|---|
| `usb` (缺省) | `power_supply/usb` 的 `voltage_now x current_now` | USB 供电时 | 只读, 不动任何充电状态。量的是**墙上抽走的功率**, 含充电与转换损耗; 电池充满停充时最接近整机功耗 |
| `battery` | `power_supply/battery/power_now` | 设备处于**放电态** | 量的是设备从电池真实抽走的功率, 更准; 但插着 USB 充电时恒为 0 |

2026-09-22 NX809J 实测: 电池 `status=Full`、`current_now=0`, 故电池轨此时量不到;
USB 轨待机读数 5.127V x 0.144A = 0.738 W, 随负载变化。

**恒为 0 不等于功耗为零, 而是「这条轨此刻量不了」。** `power_usable()` 负责把这两件事分开,
不做这层判断, 报告就会拿 `0 W` 当结论。

## 6. 设备侧 runner 契约

`vkop_runner` 是一个推到 `/data/local/tmp` 的原生可执行文件 (与 `bench` 用
`benchmark_model` 同一套路), 职责:

1. 建 Vulkan 实例与设备 (headless, compute 不需要 surface);
2. 分配**常驻显存**的输入 (低分辨率) 与输出 (送显分辨率) storage image;
3. 一次性上传测试帧, 之后画面**全程不出显存** —— 这是上游零拷贝红线在设备侧的落地点;
4. 跑 N 次 dispatch, 用 `VkQueryPool` 时间戳 (乘 `timestampPeriod`) 量 GPU 侧耗时;
5. 回读一次输出算 PSNR;
6. 往 stdout 打一个 JSON 对象。

### 回包 schema

```json
{
  "v": 1,
  "ok": true,
  "timing_us": { "samples": [1102.0, 1098.5, 1105.2], "median": 1102.0 },
  "psnr_db": 38.42,
  "device": { "name": "Adreno (TM) 840" },
  "error": null
}
```

- `ok=true` 却没有任何 `samples` = 回包有问题, 按错误处理, **不可当成 0 延迟**。
- 解析从**最前面**的 `{` 往后找第一个带 `ok` 或 `timing_us` 的对象。
  从尾部倒着找是错的: 嵌套的 `timing_us` 对象自己也是合法 JSON, 会先命中它。

## 7. 当前阻塞

`vkop_runner` **尚未构建**。工具链是齐的 ——
NDK r28 (`/opt/homebrew/share/android-commandlinetools/ndk/28.2.13676358`),
clang 19, Vulkan 头文件与 `libvulkan.so` stub 均在位;
同级 `../refbench/build/build.sh` 与 `../knobs/gray/build/build_layer.sh`
已有零交互直编 arm64-v8a 的成例, 照搬即可。

在此之前 `phonefarm gpu-op` 会走完「读契约 → 预检 SPIR-V → root 检查 → 找 runner」
然后如实报错退出 (退出码 2), 不产出任何假数字。

本通路其余部分 (契约、设备条件化、A/B 调度、功耗遥测、Welch 检验、冻结判定、
报告渲染) 已完成并有单测覆盖。

### 一个待上游确认的口径问题

本标尺是 **headless 算子标尺**: 它量的是算子自身的 GPU 耗时与重建画质,
量不到 `fps_p95_ms` (帧时 p95 需要真实渲染上下文)。该字段现记 `NaN`。

若上游需要真实帧时与在场景中的整机功耗, 被测物用同级的第一方白盒靶场
`../refbench` (纯 Vulkan 原生应用, 自带 720p 渲染管线): 把算子挂进它的后处理队列,
即可同时拿到真实帧时、USB 轨瓦数波动与确定的画质真值。
不碰任何第三方黑盒游戏的反作弊与注入。
