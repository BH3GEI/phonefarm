# knobs — 可改动面：黑盒系统旋钮 + 灰盒 API 注入层

给 [phonefarm](https://github.com/BH3GEI/phonefarm) 的 loop_v1 harness 提供**能改什么**：
黑档是系统级开关（DVFS / 限帧 / 热 / 调度 / 刷新率），灰档是 Vulkan layer 注入
（render pass 观测 + LoadOp/精度/分辨率/shader 改写）。跑测分离——本仓库不碰编排/采集/判定。

- 为什么、边界、判据、灰档可行性三问 → [`DESIGN.md`](DESIGN.md)
- 旋钮接口（`apply`/`restore`/`status` + 自报）→ [`contract/knob.md`](contract/knob.md)
- 灰档可行性状态 → [`gray/FEASIBILITY.md`](gray/FEASIBILITY.md)

## 现状（2026-09-21 立项当天）

**灰档只读层已就绪、待上机**（测试设备正被 refbench 的评测流水线占用，不抢设备）：

```bash
bash gray/build/build_layer.sh          # NDK 直编 libVkLayer_refknobs.so ✅ 已验证可编译
bash gray/enable_layer.sh probe         # 设备空出后: 对 refbench 挂层, 验注入机制 (问题1)
bash gray/enable_layer.sh target <pkg>  # 对目标游戏挂空层, 验反作弊放行 (问题2, 几分钟出结果)
bash gray/enable_layer.sh off           # 还原
```

层是**纯只读 pass-through + render pass 计数**，因此同时是探反作弊的低风险空层。
证据走文件系统（本机 logcat 会哑掉）：应用外部 files 目录下 `knobs_layer_out.json`。

**黑档**：`black/knob_framecap.sh` 是接口骨架（限帧解除，判据 6 的关键），
控制点待设备可用后在真机确认再落实现——未确认前安全空转，不乱写系统状态。

## 下一步（设备空出后，按此顺序）

1. `enable_layer.sh probe` → refbench 外部 files 出现 `layer_loaded=true` = 问题 1 成立
2. `enable_layer.sh target <目标游戏>` → 能进大世界 = 问题 2 反作弊放行
3. 两者成立 → 灰档从只读进入低风险改写（LoadOp→DONT_CARE）；否则按 FEASIBILITY 退路走
