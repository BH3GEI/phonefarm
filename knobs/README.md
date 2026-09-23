# knobs — 可改动面：黑盒系统旋钮 + 灰盒 API 注入层

给本仓库的 [`../loop_v1`](../loop_v1) harness 提供**能改什么**：
黑档是系统级开关（DVFS / 限帧 / 热 / 调度 / 刷新率），灰档是 Vulkan layer 注入
（render pass 观测 + LoadOp/精度/分辨率/shader 改写）。跑测分离——本仓库不碰编排/采集/判定。

- 为什么、边界、判据、灰档可行性三问 → [`DESIGN.md`](DESIGN.md)
- 旋钮接口（`apply`/`restore`/`status` + 自报）→ [`contract/knob.md`](contract/knob.md)
- 灰档可行性状态 → [`gray/FEASIBILITY.md`](gray/FEASIBILITY.md)

## 现状（2026-09-23 已上机）

**灰档只读层在真机上跑通了**。证据在 [`evidence/`](evidence/)，结论在
[`gray/FEASIBILITY.md`](gray/FEASIBILITY.md)。

```bash
bash gray/build/build_layer.sh              # NDK 直编 + DT_NEEDED 自检
bash gray/enable_layer.sh probe             # 对 refbench 挂层 (问题 1) ✅ 成立
bash gray/enable_layer.sh target <目标包名>  # 对目标游戏挂空层 (问题 2) ⚠ 部分成立
bash gray/enable_layer.sh status            # 只读看当前挂载态
bash gray/enable_layer.sh off               # 还原 (state 回滚 + 残留清扫)
```

| 问题 | 结论 | 判据 |
|---|---|---|
| 1 能给非 debuggable 应用挂层 | ✅ 成立 | refbench 外部 files 出现 `layer_loaded=true`；层数到 3600 帧 = 应用自报 3600 帧 |
| 2 目标游戏反作弊放行 | ⚠ 部分 | 层挂进去了、游戏跑 5 分钟不崩不被杀；但被强制更新公告挡在登录界面，**没进大世界** |
| 3 鸿蒙等价机制 | ⏸ 无设备 | |

**层对宿主是透明的**（有无层对照：swapchain、`frames_submitted=3600`、`clean_exit=true` 全相等）。

⚠ **但这个机制会误伤别的应用**：`debug.vulkan.layers` 是全局属性，挂层期间启动的、
自己 lib 目录里没有这个 .so 的 Vulkan 应用会**启动失败**——实测厂商应用
`cn.nubia.gameassist` 在挂层窗口里每 ~2 秒崩溃重启一次。`enable_layer.sh` 已改成
层一加载成功就自动清掉属性（窗口压到几秒），但**共享设备上用完务必 `off`**。

两处与立项时的假设不符，已改掉：

- **注入机制**：生效的是 `setprop debug.vulkan.layers <层名>` + 把 .so 放进**目标应用自己的
  nativeLibraryDir**。README 原先写的 `settings put global gpu_debug_*` 那一套在本机
  **一条都不生效**（四格对照见 `evidence/01_probe_refbench/matrix.txt`）。
- **logcat 没哑**：只是对 shell uid 不可读，`adb shell su -c 'logcat -b main'` 一切正常。
  所以证据走**双通道**：`knobs_layer_out.json` + logcat 的 `refknobs` 标签。

**黑档**：`black/knob_framecap.sh` 仍是接口骨架（限帧解除，判据 6 的关键），
控制点待真机确认再落实现——未确认前安全空转，不乱写系统状态。

## 下一步

1. **补上大世界这一环**：原神装机版本落后，登录要求先更新客户端。要不要更新得人来定——
   换版本会作废 loop_v1 已采的历史基线。在那之前问题 2 只能停在「部分成立」。
2. **先在 refbench 上做 LoadOp→DONT_CARE 改写**：白档靶子有已知答案，不依赖原神能否登录，
   可独立推进判据 5。任何改写都要配有/无层对照——只读层曾经把 refbench 的 swapchain
   弄成 0×0 而**全程不报错**，"没报错"不等于"没弄坏"。
3. 鸿蒙机制调研（问题 3），等设备。
