# SPEC: GPU_OP — Compute Shader 算子的真机标尺与 A/B 裁决

> 状态: 实施中 · 2026-09-22 起
> 角色: `game_opt_loop` 自主进化闭环的**物理采样与统计裁决端**。
> 上游负责变异与防爆门禁, 本通路只负责在真机上量出数字并做冻结规则判定,
> **不做任何算法判断** —— 快慢好坏由数字说话。
>
> 代码分工: 本仓库提供 `phonefarm gpu-op` 一个独立 CLI 子命令 (`src/gpuop.rs`),
> 共享的设备条件化与功耗遥测在 `src/hwcond.rs`, 统计在 `src/gpustat.rs`。
> 基因组、变异、门禁、Pareto 归档全部在 `~/projects/game_opt_loop` (Rust)。
>
> **统一入口**: v2 起 (contract 加了 kind 信封) 上层只打 `phonefarm eval`
> (`src/eval.rs`) —— v1 扁平请求原样转交 gpu-op, v2 shader 降级成 v1 转交,
> sysparam / gray 由 eval 直接实现。gpu-op 仍是 shader 通路的实现本体。
> 真机验证状态 (2026-09-23): sysparam 的 probe_only 与候选路径已在原神上端到端
> 跑通 (game_opt_loop optimize --budget 30m 走完 1 候选, 出统计判定);
> gray 的机械链路已通 (施加/测量/还原/ABORT), 但命中计数与 knobs-layer 的
> marker 口径未对齐, hits 恒 0 时按 require_hit 如实 ABORT。

## 0. 铁律

| # | 铁律 | 实现落点 |
|---|---|---|
| 1 | 不侵入任何游戏进程, 设备上零常驻、零残留 | runner 推到 `/data/local/tmp/phonefarm_gpuop`, 跑完 `rm -rf`; 采样循环有界, adb 断开自行到头 |
| 2 | sysfs 写入退出前全部恢复, 回读一致才算这轮成立 | `hwcond::Lock`; 回读不符即整轮作废并报错 |
| 3 | 判定规则在看到数据之前冻结 | `FrozenRules` 由 `eval_request.protocol` 带入, 代码中无任何事后可调的阈值 |
| 4 | 量不到的指标如实留空, 绝不用默认值填补 | 取不到的回 `null` (契约里声明成可空, 见 §6); 功耗轨不可用时当场报错退出, 不报 `0 W` |
| 5 | 严禁人工评分 | 时延来自 runner 的 `VkQueryPool` 时间戳; 功耗来自 `power_supply` sysfs; p 值由 Welch 检验算出 |

## 1. 命令契约

```
phonefarm gpu-op --request <eval_request.json> [--serial S] [--json]
                 [--runner <本地 runner 路径>] [--power-rail usb|battery]
                 [--carrier headless|refbench] [--reference <参考帧.png | 参考帧目录>]
                 [--intensity N] [--frames N]
                 [--out 目录] [--cool-timeout-s N] [--gpu-level N]
                 [--no-quality-pass]
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

`--reference <帧.png | 帧目录>` 指定画质真值。**中心裁剪**到目标分辨率, 不缩放:
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

### 一张不够: 参考帧集

`--reference` 也可以指一个**目录**, 目录里的 png/jpg 全部参与, 按文件名排序
(排序而非目录序: 目录序随文件系统走, 换台机器跑就换了顺序, 报告里的逐帧 PSNR
对不上是哪一张)。上限 32 张。

为什么不是一张: 单张真机截帧的 PSNR 强烈依赖那一帧**拍到了什么**。
对着天空的一帧几乎没有高频细节, 任何算子都能拿高分; 对着草地灌木的一帧
高频拉满, 同一个算子掉好几个 dB。一组覆盖不同内容的帧取平均, 量的才是算子本身。

`psnr_db` = **逐帧 dB 的算术平均**, 不是先平均 MSE 再转 dB。前者是视频画质评测
的通行口径 (每帧一个分数再平均); 后者会让一张特别糟的帧几乎吃掉整组分数。
逐帧值在报告的 `quality_reference.{baseline,candidate}_psnr_db` 里原样留着 ——
均值看不出"一组帧里某一张特别差"。

**帧集只在画质补测里展开。** headless 载体的主测循环 (A/B/A/B) 内联量画质,
那里只用帧集的第一张: 每张都跑一遍等于把整轮真机时间乘以帧数, 而该载体的主指标
是时延, 不是画质。

### 2026-09-23 真机验证 (NX809J, 19 张原神截帧)

帧集用 `phonefarm capture` 采于璃月港大世界探索态, 2688x1216 原生截帧,
中心裁剪到 1920x1080。上游证据: `game_opt_loop/evidence/M2_realframes_conditions.md`。

赛道基线模板在这 19 张上的**逐帧** PSNR:

| | 值 |
|---|---|
| 均值 | **34.521 dB** |
| 单帧最低 | 29.674 dB (`ref_00.png`) |
| 单帧最高 | 35.682 dB |
| 极差 | **6.008 dB** |

这个极差就是「为什么是一组帧而不是一张」的实测答案: 只取第一张会比帧集均值低 **4.9 dB**。
单帧的 PSNR 强烈依赖那一帧拍到了什么 —— 对着天空几乎没有高频细节, 对着灌木高频拉满。

同一算子 `gen1_llm1` 换真值 (headless 载体, 单帧) 的对照, 与 §4.1 表格一致:

| 参考图 | 基线臂 | 候选臂 |
|---|---|---|
| runner 内置程序化图案 | 38.666 dB | 42.826 dB |
| 真机截帧 `ref_00.png` | 29.674 dB | 30.168 dB |

19 张帧 x 2 臂 = 38 次 runner 调用, 混在 refbench 主测之后跑, 未对帧时与瓦数产生影响
(补测在 cleanup 之前、主测之后)。

### 报告要写清楚真值是什么

`eval_report.quality_reference` 跟着报告走:

```json
"quality_reference": {
  "kind": "real_frames",
  "source": "../phonefarm/tasks/genshin_refframes",
  "frames": 12,
  "frame_files": ["frame_00000.png", "..."],
  "candidate_psnr_db": [31.2, 30.8, "..."],
  "baseline_psnr_db":  [29.1, 28.7, "..."]
}
```

`kind` 只有两个值: `real_frames` (真机截帧) 与 `procedural` (runner 内置图案)。
不写这一项的话, 31.3 dB 和 38.7 dB 会被当成同一把尺子上的数 —— 那正是上面那张表
要防的事。

参考帧来源的优先级: 命令行 `--reference` > `eval_request.quality_reference` >
程序化图案。命令行在前是因为它是人手动指定的一次性覆盖, 契约字段是上游的常设配置。
指定了却读不出帧 (目录空、路径不存在、源图比目标小) **当场报错退出**,
不会悄悄退回程序化图案 —— 否则一份自称"用了真机参考帧"的报告其实量的是合成图案。

## 4.2 插帧赛道的伪影门禁

PSNR 是全图平均, 对插帧最刺眼的两类伪影**极不敏感**: HUD 只占几个百分点面积,
拉扯只发生在运动边界。全图 PSNR 看着还行, 人眼已经无法忍受。所以两项单独量、
单独一票否决。

合成场景: 世界横向平移 8 px (模拟镜头转动), HUD 在屏幕空间静止且带高对比细竖条
(文字类高频是重影最先暴露的地方)。真值是**中间帧** (平移 4 px)。

> 早先这里是错的: 拿插出来的帧去和**前帧**比。那样一个什么都不做、直接把前帧
> 抄出来的算子会拿满分 —— 指标反过来奖励"不插帧"。

| 指标 | 量什么 | 怎么算 |
|---|---|---|
| `hud_ghost_db` | HUD 重影 | 只在 HUD 区域内算 PSNR。HUD 屏幕空间静止, 正确插帧应与真值逐像素吻合; 块匹配把 HUD 当成运动内容时字被拉成双份, 这个数断崖下跌 |
| `stretch_pct` | 拉扯果冻 | 落在前后帧**邻域包络** (半径 12, 可分离 min/max) 之外的像素占比。出了包络 = 算子凭空造了前后帧都没有的内容。用邻域而非同位, 否则正常的运动补偿会被全判成伪影 |

判据是**相对本场基线臂不得明显变差** (HUD 重影容差 3 dB, 拉扯容差 1 个百分点),
不是绝对阈值 —— 这两个量和场景强相关 (运动幅度、HUD 面积、纹理复杂度),
跟绝对 dB 一样不可跨场景搬。

### 2026-09-22 真机验证 (NX809J)

| 算子 | 耗时 | psnr_db | hud_ghost_db | stretch% | 裁决 |
|---|---|---|---|---|---|
| 赛道基线 (朴素混合) | 0.352 ms | 37.29 | 99.00 | 0.00 | 基线 |
| r=2 块搜索 | 3.771 ms | 44.79 | 99.00 | 0.00 | 超预算 |
| **对照组: 强制 ±8px 位移** | 0.351 ms | 17.76 | **5.32** | 0.00 | **POOR_QUALITY** |
| **对照组: 3x 线性外推** | — | 11.96 | 99.00 | **55.72** | **POOR_QUALITY** |

两个对照组**各自只触发一个门**, 说明两项量的是不同的失效模式, 不是换个名字
重读一遍 PSNR。

注意那个重影对照组的耗时: **0.351 ms, 和基线 0.352 ms 一模一样**。
只看耗时和全图 PSNR 的话它像个免费的胜利; 是伪影门禁把它拦下来的。

## 4.3 两个评测载体

`--carrier` 选算子挂在哪里跑。两个载体量的**不是同一个量**, 不可混着比。

| | `headless` (缺省) | `refbench` |
|---|---|---|
| 载体 | `vkop_runner`, 孤立跑 dispatch | refbench `sr_pipeline` 场景, 算子挂在管线尾部 |
| `operator_latency_ms` | VkQueryPool 时间戳量到的**单次 dispatch** 耗时 | 每帧 GPU 忙时的 **A/B 之差**, 即算子的**边际**开销 |
| `fps_p95_ms` | `null` (没有渲染上下文) | 真实帧时 p95 |
| `power_watt` | 空设备上的功耗 | **场景内**整机功耗 |
| `psnr_db` | 主测内联量到 | 主测量不到, 由**画质补测**补上 (见 4.4) |

refbench 载体下 t 检验的主指标是**帧时 p95**, 不是算子耗时: 算子挂在整条管线上,
分不出"只属于它"的那一段。直接拿 B 臂的整帧 GPU 忙时当算子耗时是错的 ——
那里面绝大部分是场景 pass。

帧时序取自 raw ftrace 的 `adreno_cmdbatch_submitted`, 按 refbench 改名过的提交线程
`RefbenchDrv` 过滤; GPU 忙时取自 `adreno_cmdbatch_retired` 的 `active` ticks
(19.2 MHz), 按 ctx 归属回被测应用 —— retired 事件由 GMU 线程发出, 不带应用 comm,
不按 ctx 过滤就会把 SurfaceFlinger 的提交一起算进来。

refbench 契约写死 `submits_per_frame: 1`, 所以提交间隔可直接当帧间隔。
原神那种每帧两次提交的必须先做自检, 否则帧率会算成两倍
(见 `src/looptrace.rs` 的 `detect_submits_per_frame`)。

**A/B 污染防护**: 候选臂 `postfx.active` 必须为 true, 基线臂必须为 false;
不符即整轮作废。算子没挂上却当成有效样本, 等于拿 A 去和 A 比。

## 4.4 画质补测 (refbench 载体)

refbench 是渲染靶场, 按边界约定不带任何测量逻辑, 给不出 PSNR。
主测跑完之后, 用 headless runner 把基线与候选**各跑一次**, 只取画质。
给了参考帧集时是**每臂每帧各跑一次** (帧集 N 张 = 2N 次 runner 调用), 取逐帧均值。

**为什么不需要 A/B/A/B + t 检验**: 画质对 (着色器, 参考帧) 是**确定性**的,
同一份输入跑一百遍是同一个 dB。等冷、锁频、交替、Welch 检验那一整套是用来
对付**热漂移**的, 而热漂移污染时延和功耗, 污染不了算术。所以补测不等冷、
不锁频、两臂各跑一次 —— 省下的是几十分钟真机时间, 换不来任何精度。

补测放在主测之后、cleanup 之前: 主测期间设备要么在等冷要么在锁频跑分,
插进去会搅乱热状态; 跑完再补则完全不影响已经落袋的帧时与瓦数。

`--no-quality-pass` 可以关掉它。缺省是开 —— 画质是硬底线, 关掉必须是个显式动作。
补测失败 (runner 找不到 / 跑崩) 不作废主测: 帧时与瓦数是真跑出来的,
不该被画质这一步连坐; 画质如实留 `null`, 绝不拿基线值顶上。

**这一步是补上一个"永不触发的门禁"**: 在它存在之前, refbench 载体的
`psnr_db` 恒为 `null`, 而判定代码写的是 `candidate.psnr_db < quality_baseline_db`
—— `NaN < x` 恒假, 于是 POOR_QUALITY 分支一次都不会走到。更要命的是上游:
Pareto 前沿的三个目标 `[时延↓, 功耗↓, 画质↑]` 塌成两个, "什么都不做"
在时延和功耗上永远最优, 演化再也没有理由偏好一个更贵但更清晰的算子。
实跑验证过 (2026-09-22, 2 代 6 候选): 前沿最后留下的是赛道基线模板本身。

## 4.5 `incumbent_latency_ms`: "更快"要跟谁比

refbench 的 A 臂跑的是 `knob.postfx off` —— **不挂任何算子**的场景,
所以基线臂的算子耗时按定义是 0。判定代码原本写 `候选 < 基线`,
在这个载体上等价于 `正数 < 0`, 恒假: `is_pareto_improvement` 永远回 false,
哪怕候选确实比上一代冠军便宜一半。实跑验证过: 一整轮 5 个候选 p 值
全在 1e-7 量级 (测量极显著), 却一个 `true` 都没有。

修法是让上游把在位冠军的实测耗时随请求带下来 (`eval_request.incumbent_latency_ms`,
可空)。有它就跟冠军比 —— 那才是演化真正要问的问题: "它在真实场景里比现任便宜吗";
没有就退回跟基线臂比 (headless 载体下 A 臂跑的就是上一代算子, 原语义成立)。

显著性仍是必要条件: 不显著的差异不算改进, 哪怕数字更小。

### 2026-09-22 首次完整跑组 (NX809J, 每臂 2 轮 x 1200 帧, intensity 5)

| 臂 | 帧时 p95 | 每帧 GPU 忙 | 场景内功耗 |
|---|---|---|---|
| A (postfx=off) | 6.854 ms | 6.316 ms | 6.17 W |
| B (postfx=on) | 7.331 / 7.386 ms | 6.797 / 6.795 ms | 6.60 / 6.42 W |

报告: `operator_latency_ms` 0.477 ms (边际), `fps_p95_ms` **7.358 ms (不再是 null)**,
`power_watt` 6.507 W, p = 0.005, `is_pareto_improvement` = false (显著更慢)。

**值得注意的对照**: 同一个算子 `gen1_loc1`, headless 孤立跑是 **0.942 ms**,
挂进管线后的边际开销只有 **0.477 ms** —— 约一半。孤立跑的微秒数不等于它在真实
渲染上下文里的代价。这正是要做这层挂接的原因。
(该结论建立在每臂 2 轮之上, 样本偏薄; 要下定论需要更多轮次。)

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

## 7. 当前状态

整条通路已打通。`vkop_runner` 已构建, 产物在
`tools/vkop/android_aarch64_vkop_runner`, 源码与零交互的 `build.sh` 同目录
(出包与单独跑法见 `tools/vkop/README.md`)。2026-09-22 在 NX809J 上首次实测,
2026-09-23 接上真机参考帧集。

其余部分 (契约、设备条件化、A/B 调度、功耗遥测、Welch 检验、冻结判定、报告渲染)
已完成并有单测覆盖。上游 `game_opt_loop` 的演化闭环走的是 refbench 载体,
两条载体的取舍见 §4.3。

找不到 runner、SPIR-V 预检不过或缺 root 时, `phonefarm gpu-op` 仍会走完
「读契约 → 预检 SPIR-V → root 检查 → 找 runner」然后如实报错退出 (退出码 2),
不产出任何假数字。

留一条容易记错的: headless 载体量不到 `fps_p95_ms` (帧时 p95 需要真实渲染上下文),
该字段回 `null` —— 不是 `0` 也不是 `NaN`。契约两边都声明成可空, 判定侧 `null`
不参与比较, 对应单测在 `src/gpuop.rs`。
