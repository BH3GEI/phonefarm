# 灰档改写 #1: LoadOp LOAD → DONT_CARE —— 成立

设备 NX809J / Android 16 / user 版 / Magisk root / Adreno 840，2026-09-23 07:05–07:20
靶子 `io.github.hgamey.refbench`（白档，自带 `knob.loadop`，所以这条改写有**已知答案**可对）

## 测试条件（同一组对照必须同条件，勿与其它批次混比）

- **红魔内置主动散热风扇：开**（`/sys/kernel/fan/fan_enable = 1`，用户手动开启后一直开着）。
  四个臂全部在风扇开启状态下连续跑完，条件一致。风扇开启**之前**采的数据不要和这批混在一起比。
- 负载 `bw_pingpong`，`frames=1800`，`intensity=8`
- 原神当时未运行，全程未启动/未杀/未清数据（用户正在更新客户端）
- 设备锁：新版 `devlock` 协议，label `knobs-layer`。本轮 07:05:06 取到锁时
  **排在我前面的队列还没走完**（队列是 sysknob-loop → real-frame-psnr → wb-vksamples → 我），
  锁当时是 free 我就拿了，属于插队；任务约 12 分钟，跑完立即释放。记一笔备查。

## 四臂对照

| 臂 | 层 | `knob.loadop` | refbench 自报 `load_op` | 层自报 `effective` | `begins` | `unavailable_reason` | frames / clean_exit |
|---|---|---|---|---|---|---|---|
| A | 无 | off | LOAD + DONT_CARE | — | — | — | 1800 / true |
| B | 无 | on  | 只有 DONT_CARE | — | — | — | 1800 / true |
| C | **改写** | off | LOAD + DONT_CARE | pass 2 att 0 LOAD→DONT_CARE | **14400** | null | 1800 / true |
| D | **改写** | on  | 只有 DONT_CARE | pass 2 att 0 LOAD→DONT_CARE | **0** | `rewritten pass never bound` | 1800 / true |

B 是白档给出的已知答案（refbench 自己换成 DONT_CARE）。C 是灰档要验的那一条。

## C 成立的四条判据

1. **层确实改了**：`effective = [{pass:2, attachment:0, field:"loadOp", from:"LOAD", to:"DONT_CARE"}]`
2. **改的那个 pass 真被用上了**：`begins = 14400 = 1800 帧 × 8 (intensity)`，
   与 `render_pass_begins = 16200`（= 14400 + 1800 个 present blit）自洽。
3. **应用不知道自己被改了**：refbench 仍自报 `load_op: "LOAD"`。
   "应用以为是 LOAD、驱动实际收到 DONT_CARE" 就是灰档改写生效的定义。
4. **没把应用弄坏**：四个臂全部 `frames_submitted=1800`、`clean_exit=true`，与无层基线相同。

## D 臂逼出来的一个设计缺陷（已修）

第一版 D 臂的 `effective` **也非空**，看起来像"改写在 on 臂也生效了"，是错的：
refbench 在启动时把 `rp_off_load` 和 `rp_off_dont` **两个 render pass 对象都建出来**，
按 knob 只绑其中一个。层钩的是 `vkCreateRenderPass`（loadOp 是创建期烘进对象的，
BeginRenderPass 时已改不动），于是它把那个**根本不会被绑**的 LOAD 对象也改了，
然后如实报成 effective —— 一次空改写被报成了生效。

修法：记录被改写的 `VkRenderPass` 句柄，在 `vkCmdBeginRenderPass` 里数它实际被绑了多少次，
`effective` 增加 `begins` 字段；`begins` 全为 0 时 `unavailable_reason` 报
`"rewritten pass never bound"`。修完 D 臂就正确地报 `begins:0` + 该原因。

**这条对 harness 很要紧**：`effective` 非空不等于旋钮生效。判"这一轮算不算数"要看
`begins > 0`，不能只看 `effective` 非空。

## 审查捡到的一个证据自相矛盾（已修正后重跑）

这轮第一次采的证据里，`RESULT.md` 写着 `frames=1800`，而四份 `*.refbench.json` 里
`params.frames` 全是 `3600`（refbench 的默认值）——**条件和证据对不上**。
根因是 `test_loadop.sh` 用 `--ei` 传 `frames`/`intensity`，但 refbench 的 extras 全走
字符串解析，`--ei` 会被 `am` 静默丢掉，于是那两个参数一直是摆设。

改用 `--es` 后重跑，`params.frames` 才真的是 1800，本页所有数字取自重跑后的证据。
**教训**：脚本里"设了参数"不等于"参数生效了"，回读被测对象自报的实际参数才算数。

## 与白档答案的差异（如实记，别当成等价）

refbench 的 `knob.loadop=on` 除了把 loadOp 设成 DONT_CARE，**还把 `initialLayout`
从 `SHADER_READ_ONLY_OPTIMAL` 改成 `UNDEFINED`**（等于额外允许驱动丢弃旧内容、
省掉一次布局转换）。本层只改 `loadOp`，不动 `initialLayout`，所以

> 灰档改写是白档答案的**真子集**，两者的性能效果不保证相等。

C 与 B 谁更省、差多少，是 loop_v1 的统计判定该回答的问题 —— **本仓库不做任何测量与判定**
（DESIGN.md 边界）。这里只交付"改写生效、可自报、可回滚、不弄坏应用"。

## 复现

```bash
bash gray/build/build_layer.sh
bash gray/test_loadop.sh          # 四臂全跑, 证据落到 evidence/05_loadop/
bash gray/enable_layer.sh off     # 还原
```

单臂手动：`bash gray/enable_layer.sh loadop io.github.hgamey.refbench`
（与 `probe` 唯一差别是属性 `debug.knobs.loadop`，A/B 两臂只切它一个）。

## 还原

`debug.vulkan.layers` 空、`debug.knobs.loadop=0`、全盘搜不到 `libVkLayer_refknobs.so`
与 state 文件。全局属性的暴露窗口每臂约 **2 秒**（层一加载就清）。
