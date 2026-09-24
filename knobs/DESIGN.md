# knobs — 可改动面：黑盒系统旋钮 + 灰盒 API 注入层

> 立项 2026-09-21 · 上游依赖本仓库根目录的 phonefarm（`../loop_v1` 是驱动方）
> 分工：harness 那条线单独有会话在跑；本仓库只提供「能改什么」，不碰编排/采集/判定
> 配套：[HGamey/refbench](https://github.com/HGamey/refbench) 是白档靶子（已知答案的考场）

## 0. 三档可改动面

优化候选分三档，本仓库补齐**黑 + 灰**两档：

| 档 | 能碰的东西 | 手段 | 现状 |
|---|---|---|---|
| **黑** | 系统级：DVFS / 总线频率 / 限帧 / 热策略 / 调度 / 刷新率 | sysfs / settings / 厂商接口（需 root） | 真机探出 11 项可写可生效的参数并接进全自动闭环（见下「3.1 现状」）；旧的 DDR 下限结论（p95 −1.61%，p=0.0079）基于 30fps 时代的原神，**已不可比** |
| **灰** | **API 流：render pass、LoadOp、RT 格式与精度、分辨率、shader、VRS** | Vulkan/GLES layer 注入、资源包重打包、配置改写 | **几乎全空白**（只改过 PlayerPrefs 画质） |
| 白 | 源码：管线、shader、资产 | 自己的工程（refbench） | refbench 已立 |

**灰档是最大的空白，也是价值最高的一档**，因为它同时解两件事：

1. **归因**：本机 Perfetto 的 GPU producer 驱动未注册，render pass 级归因做不到。
   但在 API 层拦截，render pass 的边界是**你自己划的**，不依赖驱动是否配合。
2. **优化**：黑盒游戏改不了 render pass / shader / draw call —— 在 API 层就能改，
   **不需要源码**。

## 1. 一句话目标

给 harness 一个**可插拔的旋钮库**：黑档是系统级开关，灰档是 API 拦截层。
每一条对应 [`../docs/MOBILE_GPU_OPT_ROUTES.md`](../docs/MOBILE_GPU_OPT_ROUTES.md) 里的一条路线，能被 harness 从外部驱动。

## 2. 边界

| 本仓库做 | 本仓库不做 |
|---|---|
| 提供旋钮：`apply` / `restore` / `status` 三态 | 不做编排（loop_v1） |
| 提供 API 层的 render pass 计数与标记 | 不做统计判定（loop_v1） |
| 自报本次实际生效了什么 | **不自报帧率、不自算任何指标** |
| 保证可逆 | 不做优化决策（候选从路线图挑） |

接口契约见 [`contract/knob.md`](contract/knob.md)，与 loop_v1 现有的 `knob_ddr_boost.sh` 同构：
每个旋钮实现 `apply`/`restore`/`status`；`restore` 后设备快照必须与 `apply` 前逐行相等
（loop_v1 判据 4 会验）；退出时自报实际生效项 / 失败项 / 不可用原因。

## 3. 黑档：补齐系统旋钮（每条须过 loop_v1 判据 4 不留痕）

| 旋钮 | 打路线图哪类 | 备注 |
|---|---|---|
| **厂商限帧策略 / 游戏空间性能模式** | E | 立论前提（原神封顶 @30fps）已不成立——7.1.0 实测 60 fps，见 3.1 现状。系统级设置，动手前拿明确授权 |
| CPU/GPU governor 与频率下限 | E | bench.rs 已有部分实现，抽出复用 |
| 热策略 / 温控阈值 | E | 真 fps 是热稳态 fps；红魔另有风扇 sysfs（`/sys/kernel/fan/`，refbench 侧已用过） |
| 大小核调度 / affinity | B（CPU 提交） | 本机 queue_p50=4.37ms，占 13.05% |
| 刷新率 / 系统动态分辨率 | D | |

### 3.1 现状（2026-09-23 真机实测，NX809J）

编排在 `phonefarm autoloop`（`src/autoloop.rs`），判定口径在 `src/sysparam.rs` / `src/llm.rs`，本节只记「能改什么」的实测结论。
证据：`phonefarm/loop_v1/runs_sysparam/`。

**测试条件**：红魔 NX809J，Android 16 / Adreno 840v2，已 root；原神 7.1.0 大世界探索态；
负载 `workload_spin_touch_v1.json`（触控拖拽转视角，画面变化 81.8% ≫ 20% 门槛）；
内置主动散热风扇开启（`fan_enable=1, fan_speed_level=5`）；
测量期间停充（`/sys/class/qcom-battery/charging_enabled 1→0`），全程放电态。

**基线（重测，旧基线作废）**：`fps_mean` 59.1–59.8，`frame_p50` 16.65 ms，
`frame_p95` 19.2–19.6 ms，整机功耗 4.76 W，SoC 结温 55.2 °C。
**原神已不再被钉在 30fps** —— DESIGN 里所有基于「封顶 30fps、GPU 只用 64.5%」的论证都要重看。

**探出来的白名单（11 项，全部通过「存在 + 可写 + 写了真生效」三关）**：

| 参数 | 原值 | 档数 |
|---|---|---|
| `cpu.policy{0,6}.scaling_min_freq` / `_max_freq` | 787200/1785600、883200/1497600 | 28 / 27 |
| `cpu.policy{0,6}.scaling_governor` | walt | 5（walt/conservative/powersave/performance/schedutil）|
| `gpu.min_pwrlevel` / `gpu.max_pwrlevel` | 17 / 3 | 18 |
| `bus.DDR.boost_freq` / `bus.LLCC.boost_freq` | 547000 / 282000 | 11 / 9 |
| `setting.system.refresh_rate_mode` | 0 | 4（实测 1→60Hz、2→90Hz、3→120Hz、4→144Hz）|

**几条只有上机才知道的**：

- **厂商温控/性能管家会跟你抢**：游戏过程中它主动压 `cpu.policyN.scaling_max_freq`
  与 `kgsl.max_pwrlevel`。实测一组候选只写了 DDR/LLCC，退出时 policy0 的
  `scaling_max` 从 1785600 变成 1228800、policy6 从 1497600 变成 1382400、
  `max_pwrlevel` 2→0。另一组把 policy6 的 min 写成 1497600，回读是 1382400。
  所以**写得进不等于守得住**，探测要隔十几秒再回读一次，守不住的报 `contested` 不进白名单。
- **切 governor 有副作用**：`scaling_governor` 切成 performance 再切回 walt，
  `scaling_max_freq` 不会自己回来。旋钮回滚必须覆盖整组兄弟节点（governor → max → min）。
- **AOSP 的 `peak_refresh_rate` / `min_refresh_rate` 在本机是 `null`**，走厂商的
  `refresh_rate_mode`（取值表在 `system:all_refresh_rate`）。厂商键语义无文档，
  只认「写进去后 SurfaceFlinger 活动模式 fps 真的变了」的那几档。
- **GPU devfreq 不在 `$KGSL/devfreq`**，在 `/sys/class/devfreq/3d00000.qcom,kgsl-3d0`；
  本机那几个节点读不到，所以 GPU 频率边界走 `min/max_pwrlevel`。
- **温控保护永远不进白名单**（`thermal/trip_point/cooling/fan/tsens/bcl/throttl` 命中即拒，
  主机端与设备端各拦一道）。风扇转速与停充节点同理：它们是测量前提，不是可调参数。

**限帧解除**：本机原神已经不是 30fps，`black/knob_framecap.sh` 当初的立论前提消失，
控制点仍未确认，保持空转骨架。

`black/` 下每个旋钮一个脚本，接口同 `knob_ddr_boost.sh`。设备相关节点在**设备可用后**（refbench
电池不占用设备时）逐个验证再落实现——现在是骨架，不写未经真机确认的 sysfs 路径。

## 4. 灰档：API 注入层（本仓库核心）

### 4.1 先验可行性，再写功能

**第一件事不是写层，是回答三个问题**（`gray/FEASIBILITY.md` 记结论，允许以否定结论结案）：

| # | 问题 | 怎么验 | 不成立则 |
|---|---|---|---|
| 1 | 能给非 debuggable 第三方应用挂 Vulkan layer 吗？ | root 下 `settings put global gpu_debug_layers` + `gpu_debug_app` 那套，先用**自己的 demo**（refbench）验机制，再试目标游戏 | 退回资源包重打包 + 配置改写 |
| 2 | 目标游戏的反作弊会拦吗？ | 挂一个**只读、什么都不改**的空层，看游戏能否正常进大世界。**几分钟出结果，第一天就做** | 换无反作弊目标，或只做白档 |
| 3 | 鸿蒙上有等价机制吗？ | 设备到位后验 | 灰档只在 Android 成立，鸿蒙退回黑档 |

问题 2 尤其要早做：挂空层试探是唯一低风险的验证方式，别等写完拦截逻辑才发现进不去游戏。

### 4.2 层做什么（先只读，再改写）

| 阶段 | 能力 | 对应路线图 |
|---|---|---|
| **只读** | 划 render pass 边界、统计每 pass 的 draw call / RT 尺寸格式 / LoadOp·StoreOp 实际取值 / 多余 LOAD | A 类观测前提 |
| 改写·低风险 | LoadOp `LOAD`→`DONT_CARE`、RT 精度降级、格式替换 | A 类 |
| 改写·中风险 | 渲染分辨率缩放 + 上采样、VRS 注入 | D 类 |
| 改写·高风险 | shader 替换（fp32→fp16/mediump） | C 类 |

每档改写**可逆且自报**：层写出「本次改了哪些 pass 的哪些字段」供 loop_v1 记证据。
**层不判断改得好不好，那是 harness 的事。**

`gray/layer/vk_layer_refknobs.cpp` 是只读起步实现：pass-through + render pass 计数，
经 `/proc/self/cmdline` 取包名，把计数写进该应用的外部 files 目录（logcat 在本机会哑掉，
所以进度/证据全走文件系统——与 refbench 同一教训）。

## 5. 退出判据（可程序断言）

| # | 判据 | 断言 |
|---|---|---|
| 1 | 旋钮接口成立 | ≥2 黑档 + 1 灰档走同一 `apply/restore/status`，被 loop_v1 驱动跑通 |
| 2 | 可逆 | 每个旋钮 restore 后 loop_v1 判据 4 通过（快照逐行相等） |
| 3 | 层可行性有答案 | 4.1 三问各有带证据结论，写进文档。**结论是"不能"也算达标** |
| 4 | 归因粒度提升 | 只读层能对目标负载输出逐 render pass 的 draw call 数与 RT 格式，跨轮一致 |
| 5 | ≥1 条灰档改写生效且可测 | loop_v1 主指标 p<0.05（方向不限，改坏也算数据） |
| 6 | 天花板是真的 | 限帧旋钮解开后，同一负载帧时间不再钉死 33.3ms |

## 6. 硬规矩

- 可逆第一：任何写入退出前恢复，无论成败
- 旋钮不自报效果，只自报「实际生效了什么」
- 反作弊：只在自己设备、自己账号做，不分发、不做规避检测的功能
- 不预估时间、不用 sleep 盲等；等后台进程用存活轮询
- 证据落盘；回复用「判据」「判定」

## 7. 与另两条线的关系

```
harness (loop_v1)        knobs（本仓库）      refbench（白档靶子）
─────────────────        ──────────────      ──────────────────
编排/采集/归因/判定  ←→  能改什么          ←→  已知答案的考场
（另有会话在跑）          黑 + 灰               验证归因对不对
```

本仓库让 harness **有东西可改**；refbench 让 harness **知道自己有没有归因错**。互不替代。
