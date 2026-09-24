# knobs — 可改动面：黑档系统参数 + 灰档 Vulkan 层

回答「能改什么」这一侧的问题。黑档是系统级开关（DVFS / 限帧 / 热 / 调度 / 刷新率），
灰档是 Vulkan layer 注入（render pass 观测，以及 LoadOp / 精度 / 分辨率 / shader 改写）。
改动由 [`../loop_v1`](../loop_v1) 驱动施加，本目录不碰编排、采集与判定——
能改的一方不参与判定，否则证据不成立。

## 在整体里的位置

整套东西是一个 harness：用户只提一种任务「把某个游戏在某台手机上优化一下」，
内部决定开哪几层。phonefarm 是底座，本目录是底座里「应用改动」那一块。

优化分三个层级：① 游戏代码（需要白盒，本目录管不到）② 图形接口 Vulkan 层（灰档）
③ 系统参数（黑档）。闭源商用游戏上只剩 ②③，这两档就是本目录的全部内容。
鸿蒙商用机在没有 root 的情况下两档都动不了，只能用 HiSmartPerf 测。

本目录原先是独立仓库 `HGamey/knobs`，2026-09-23 并入 `phonefarm/knobs/`（保留提交历史），
原仓库已归档。引用路径请写 `phonefarm/knobs/…`。shell 脚本正在往 Rust 内核里收，尚未完成。

- 为什么、边界、判据、灰档可行性三问 → [`DESIGN.md`](DESIGN.md)
- 旋钮接口（`apply`/`restore`/`status` + 自报）→ [`contract/knob.md`](contract/knob.md)
- 灰档可行性状态 → [`gray/FEASIBILITY.md`](gray/FEASIBILITY.md)

## 现状（2026-09-23 已上机）

**灰档只读层在真机上跑通了**。证据在 [`evidence/`](evidence/)，结论在
[`gray/FEASIBILITY.md`](gray/FEASIBILITY.md)。

```bash
bash gray/build/build_layer.sh              # NDK 直编 + DT_NEEDED 自检
bash gray/enable_layer.sh probe             # 对 refbench 挂只读层 (问题 1) ✅ 成立
bash gray/enable_layer.sh loadop            # 同上 + 打开 LoadOp 改写 (判据 5) ✅ 成立
bash gray/test_loadop.sh                    # 四臂对照, 证据落 evidence/05_loadop/
bash gray/enable_layer.sh passdump <pkg>    # 只读观测: 导出 pass 形状表 + 描述符溯源
bash gray/enable_layer.sh target <目标包名>  # 对目标游戏挂空层 (问题 2) ✅ 成立
bash gray/enable_layer.sh status            # 只读看当前挂载态
bash gray/enable_layer.sh off               # 还原 (state 回滚 + 残留清扫)
```

| 问题 | 结论 | 判据 |
|---|---|---|
| 1 能给非 debuggable 应用挂层 | ✅ 成立 | refbench 外部 files 出现 `layer_loaded=true`；层数到 3600 帧 = 应用自报 3600 帧 |
| 2 目标游戏反作弊放行 | ✅ 成立 | 更新到 7.1.0 后补齐：正常登录、进大世界、跑满一轮负载，无崩溃无反作弊告警 |
| 3 鸿蒙等价机制 | ⏸ 无设备 | |
| 灰档改写 #1 LoadOp→DONT_CARE（refbench） | ✅ 成立 | 四臂对照；改写被绑 14400 次、应用仍自报 LOAD、四臂 `clean_exit=true` |
| 灰档改写 #2 前置：copyprobe 1:1 拷贝探针（原神） | ✅ 通过 | 5 臂跑满一轮无反作弊拦截；copies=subs≈帧数；p50 与无层差 <0.05ms；画面目检无异常（跨 run 逐像素受游戏内时辰影响不可比） |
| 同上，在原神大世界的效果 | ⚠ 命中但无效果 | 5 对 A/B：改写真被绑 4.4–5.2 万次，但 p95 / GPU / 带宽 p 值全 > 0.05 |
| 超分算子可行性（只读观测） | ✅ 可行 | 放大 = 帧内最后一个 pass 的第一笔 draw；**UI 在同一 pass 内紧跟其后**；该 draw 采的确认就是上一个渲染分辨率 pass 的输出（`usage=151` 含 `SAMPLED`，可读） |
| 灰档超分：真算子 gen1_loc3 投原神（渲染精度低） | ✅ 零成本通过 | 5 对 A/B（loop_v1 冻结口径 252 枚举）：p95 −0.19%、fps +0.15%、功耗 +4.1%（p_lose=0.28，噪声内；A 臂自身极差 0.85W）、37°C 平直；画质 MAD 比游戏自带放大接近原生 3–4 倍（8.3–18.7 vs 26.9–42.8）。`evidence/09_upop/` |

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

## 换了游戏版本之后

客户端更新到 7.1.0 才通的大世界，代价是两条：

- **loop_v1 已采的历史基线作废**，7.0.0 与 7.1.0 的数不能混在一起比。
- **手柄注入在 7.1.0 上失效**。原来的手柄脚本跑出来是静止画面，而静止画面照样采得到
  帧率、功耗、温度，报告看着一切正常——这种失败不会自己报错。现在负载改用触控版
  [`gray/workload_spin_touch_v1.json`](gray/workload_spin_touch_v1.json)，并且用
  `phonefarm frames-moving` 抓窗口内两张裸帧算逐像素差，确认画面真的在动
  （交叉核对那一轮量到 10.458%，静止画面实测约 1%）。

顺带更正一条旧说法：过去把原神封在 30fps 归因成「厂商限帧」，那是 7.0.0、当时那套
游戏设置下的观测。7.1.0、游戏内 60 帧设置下同一台机器实测就是 60fps，
`gpu_active_mean` 约 13.3ms/帧（`evidence/06_genshin_world/ab_report.json`）。
当时为什么封在 30fps 还没核实。

## 下一步

1. **换一条更直接削工作量的灰档改写**。LoadOp→DONT_CARE 在 refbench 上判据成立，
   到了原神大世界是「命中但无效果」（p=0.325）——被改的 pass 本就不是带宽瓶颈，
   或者 Adreno 这类 tile-based GPU 上 `LOAD` 的代价本来就小。这条不值得再投入，
   下一条候选挑 RT 精度降级或格式替换（「低分辨率渲染 + 超分」那条已投真算子，见 `evidence/09_upop/`）。
2. **黑档补上限帧解除**（判据 6 的关键）。卡在控制点还没真机确认，见上一节。
3. **鸿蒙机制调研**（问题 3），等设备。商用机没有 root 时灰黑两档都动不了，
   可能的出路有工程机、官方性能接口、开发者模式下自签名应用三条，都还没核实。

任何改写都要配有无层的对照——只读层曾经把 refbench 的 swapchain 弄成 0×0 而全程不报错，
「没报错」不等于「没弄坏」。
