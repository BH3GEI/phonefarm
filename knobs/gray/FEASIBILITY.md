# 灰档可行性：三个必须先回答的问题

判据 3 允许以否定结论结案。**"验证不可行"是合格交付，"没去验"不是。**
每个问题的结论要带证据（命令 + 现象）。

**2026-09-23 已上机实测**（NX809J / Android 16 / SDK 36 / user 版 / Magisk 30.7 root /
Adreno 840）。原始证据在 [`../evidence/`](../evidence/)。

## 问题 1 — 能给非 debuggable 应用挂 Vulkan layer 吗？

**结论**：`YES`，成立。证据 [`evidence/01_probe_refbench/RESULT.md`](../evidence/01_probe_refbench/RESULT.md)

对 `io.github.hgamey.refbench`（release 装机，`dumpsys package` 的 flags 里没有
`DEBUGGABLE`）挂上了只读层，应用外部 files 目录出现：

```json
{"knob":"gray_readonly_probe","layer_loaded":true,"pkg":"io.github.hgamey.refbench",
 "readonly_stats":{"frames":3600,"render_pass_begins":32400}}
```

层数到的 `frames=3600` 与 refbench 自报的 `frames_submitted=3600` **逐帧相等**，
`render_pass_begins=32400` = 3600 × 9 pass/帧。这是独立交叉校验：
层的计数与被测应用自己的账对得上，不是自说自话。

### 生效条件（与立项时设想的**完全不同**）

只需要两件事，缺一不可：

1. `libVkLayer_refknobs.so` 放进**目标应用自己的 nativeLibraryDir**
   （`/data/app/~~<x>/<pkg>-<y>/lib/arm64/`，root 直接 cp，属主 `system:system`，
   SELinux 上下文 `u:object_r:apk_data_file:s0`）
2. `setprop debug.vulkan.layers VK_LAYER_refknobs_readonly`

加载器对**每个**应用都默认搜它自己的 lib 目录（实测：无关的系统应用 `cn.nubia.gamelab`
启动时同样在搜它自己的 lib 目录），所以不需要应用 debuggable。

### 立项时写的那套 `settings put global gpu_debug_*` 在本机**完全不生效**

四格对照见 [`evidence/01_probe_refbench/matrix.txt`](../evidence/01_probe_refbench/matrix.txt)：

| | `debug.vulkan.layers` 置层名 | 置空 |
|---|---|---|
| `ro.debuggable=1` | 挂上了 | 没挂上 |
| `ro.debuggable=0` | 挂上了 | 没挂上 |

外加一格：把 `settings global gpu_debug_*` 全删、只留属性 → **仍然挂上了**。

所以：`debug.vulkan.layers` 是充分必要条件；`ro.debuggable` 无关（测完已 resetprop 还原为 0）；
`enable_gpu_debug_layers` / `gpu_debug_app` / `gpu_debug_layers` / `gpu_debug_layer_app`
这一套一条都不生效 —— 框架侧 `GraphicsEnvironment` 因为应用非 debuggable 且
`ro.debuggable=0` 直接跳过，logcat 里连 `GPU debug layers enabled` / `Debug layer list`
都不打。`gpu_debug_layer_app` 指向的"宿主包供 .so"那条路也随之不成立，
**.so 必须放进目标应用自己的 lib 目录**。

### ⚠ 这个机制有会伤到别人的副作用（实测，不是理论风险）

`debug.vulkan.layers` 是**全局**属性。没有 .so 的应用确实不会加载本层，但它们会因为
**找不到被指名的层而启动失败**：

挂层期间厂商应用 `cn.nubia.gameassist`（此前已稳定运行 63 小时）
`RenderThread` 报 `HWUI: Assertion failed: err < 0` → SIGABRT，之后在整个挂层窗口里
**每 ~2 秒崩溃重启一次**，直到属性被清掉。证据
[`evidence/02_target_genshin/RESULT.md`](../evidence/02_target_genshin/RESULT.md)。

缓解：`enable_layer.sh` 现在挂完会自动启动目标、一旦轮询到层加载成功就**立刻清掉属性**，
把暴露窗口压到几秒（层已在目标进程里，清属性不影响它）。要保留属性得显式 `--keep-prop`。
用完仍要 `enable_layer.sh off`。**在共享设备上尤其注意这一条。**

### 上机才暴露的四个真问题（立项时的代码全错，全已修）

1. **.so 落点**：早先脚本放 `/data/data/<pkg>/`，加载器根本不搜那里。
2. **动态链 STL**：`dlopen failed: library "libc++_shared.so" not found`。
   宿主应用 lib 目录里不一定有它（refbench 没有，原神有）→ 必须 `-static-libstdc++`。
   `build_layer.sh` 已加该选项并自检 `DT_NEEDED`，残留依赖就当场失败。
3. **枚举符号**：`missing some instance enumeration functions`。
   Android 的加载器用 **dlsym 发现层，完全不读 JSON manifest**（那是桌面 loader 的约定），
   层必须导出 `vkEnumerateInstance{Layer,Extension}Properties`。
4. **sType 写错**：`VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO` 是**枚举量不是宏**，
   早先用 `#ifndef` 兜底 define 成 `1000000000` —— `#ifndef` 对枚举恒为真，于是无条件
   覆盖成错值，层挂上了但 `CreateInstance` 永远找不到 layer link
   （实测 chain 里是 `sType=47`）。正确值是核心枚举的 **47 / 48**，已用 `static_assert` 钉死。

### 第 5 个坑：只读层把应用弄坏了却不报错

早先版本的 `vkEnumerateDeviceExtensionProperties` 无条件回答"0 个扩展"，
于是 refbench 找不到 `VK_KHR_swapchain`，swapchain 建成 0×0，`frames_submitted=0`、
`clean_exit=false`，**全程零报错**。有无层对照：

| | swapchain | frames_submitted | clean_exit |
|---|---|---|---|
| 无层 | 2688×1216 MAILBOX | 3600 | true |
| 挂层（修好后） | 2688×1216 MAILBOX | 3600 | true |

教训：**"只读"指不改渲染行为，不等于可以乱答枚举**。`pLayerName` 不是本层时一律转发下层。
任何后续改写都要配同样的有/无层对照，否则"没报错"不等于"没弄坏"。

## 问题 2 — 目标游戏反作弊会拦吗？

**结论**：`YES` —— 客户端侧、登录态、大世界三段都放行。
证据 [`evidence/02_target_genshin/RESULT.md`](../evidence/02_target_genshin/RESULT.md)（首轮，卡在强制更新）
与 [`evidence/06_genshin_world/RESULT.md`](../evidence/06_genshin_world/RESULT.md)（更新到 7.1.0 后补齐）

成立的部分：层挂进了 `com.miHoYo.Yuanshen`（7.0.0），游戏**没崩、没被杀**，
连续跑约 5 分钟：

```json
{"layer_loaded":true,"pkg":"com.miHoYo.Yuanshen",
 "readonly_stats":{"frames":5700,"render_pass_begins":152196}}
```

按 logcat 优先级字段精确过滤：原神进程无 F 级日志、无 tombstone、无 SIGSEGV/SIGABRT、
无反作弊模块告警。

没成立的部分：**进不了大世界**。装机版本落后于服务端要求，登录界面弹强制更新公告
（"发现新版本，请点击下方按钮下载最新客户端"），不更新客户端登录不了。
所以最硬的那一环 —— 登录态 + 大世界里反作弊是否放行 —— 仍是未知。

没擅自开始下载客户端：数 GB、占共享设备，而且会把游戏版本改掉，
`../../loop_v1` 的基线是在当前版本上采的，换版本等于作废历史基线。这个决定该由人拍。

## 问题 3 — 鸿蒙有等价机制吗？

**结论**：`PENDING`（无设备）。待鸿蒙设备到位。OpenHarmony 的图形栈是否暴露等价的
layer 注入点未知；不成立则灰档只在 Android 成立，鸿蒙退回黑档。

---

## 当前可用能力

- `layer/vk_layer_refknobs.cpp` — 只读 pass-through + render pass 计数，**已在真机验证**：
  对非 debuggable 应用可注入、对宿主透明（有无层对照逐帧相等）、计数与应用自报交叉一致
- 证据双通道：应用外部 files 目录的 `knobs_layer_out.json`，
  以及 `adb shell su -c 'logcat -b main' | grep refknobs`。
  **立项时记的"本机 logcat 哑掉"不准确** —— logcat 只是对 shell uid 不可读，root 读一切正常
- `build/build_layer.sh` — NDK 直编 + `DT_NEEDED` 自检
- `enable_layer.sh {probe|target <pkg>|status|off}` — `off` 带 state 回滚 + 全盘残留清扫

## 灰档改写 #1：LoadOp LOAD → DONT_CARE —— 已成立

2026-09-23 在 refbench 上跑通四臂对照，证据
[`evidence/05_loadop/RESULT.md`](../evidence/05_loadop/RESULT.md)。

层钩 `vkCreateRenderPass`（loadOp 是**创建期**烘进 VkRenderPass 对象的，
BeginRenderPass 时已改不动），把 `LOAD` 的 attachment 改成 `DONT_CARE`，并自报改了什么。
开关是属性 `debug.knobs.loadop`，与只读档共用同一个 .so，A/B 两臂只切它一个。

| 臂 | 层 | `knob.loadop` | 层自报 | `begins` | frames / clean_exit |
|---|---|---|---|---|---|
| A | 无 | off | — | — | 1800 / true |
| B | 无 | on（白档答案） | — | — | 1800 / true |
| C | 改写 | off | pass 2 att 0 LOAD→DONT_CARE | 14400 | 1800 / true |
| D | 改写 | on | 同上 | 0 → `rewritten pass never bound` | 1800 / true |

C 成立：改了、改的那个 pass 真被绑了 14400 次（= 1800 帧 × 8）、refbench 仍自报
`load_op: LOAD`（它不知道自己被改了，这正是灰档改写的定义）、四臂全部 `clean_exit=true`。

**D 臂逼出的一条硬经验**：`effective` 非空 ≠ 旋钮生效。refbench 会把 LOAD 和 DONT_CARE
两个 render pass 对象都建出来、只绑一个，层把那个**永远不会被绑**的也改了并报成 effective。
已加 `begins` 计数与 `"rewritten pass never bound"`。**harness 判该轮算不算数要看 `begins > 0`。**

**与白档答案不等价**：refbench 的 `knob.loadop=on` 还把 `initialLayout` 改成 `UNDEFINED`，
本层只改 `loadOp` → 灰档改写是白档答案的**真子集**，性能效果不保证相等。
差多少由 loop_v1 判定，本仓库不做测量。

## 灰档改写 #1 在原神上的判定：命中，但无可测效果

5 对交替 A/B（两臂都挂层，只切 `debug.knobs.loadop`），原神 7.1.0 / 游戏内 60 帧：

| 指标 | 差值 | 置换检验 p | 显著 |
|---|---|---|---|
| frame_p95（主指标） | −0.51% | 0.325 | ✗ |
| gpu_active_mean | +0.017% | 0.944 | ✗ |
| bw_median | +0.83% | 0.524 | ✗ |

改写**确实命中**：每个 knob 臂有 31–37 条 attachment 真被绑定，单轮总绑定 4.4–5.2 万次，
单条最高被绑 1.7 万次；5 个 ctrl 臂 `effective` 全空。所以是
**「改了、改准了、但这条改动在这个负载上换不出性能」**，不是「没改到」。

判据 1 可重复 ✅、2 归因 ✅、4 不留痕 ✅、**3 统计显著 ❌**。
功耗只采到 3 个可用样本（离散 2.87–4.62 W）就让出设备了，不下结论。

合理解释：被改的 pass 本就不是带宽瓶颈，或 Adreno 这类 tile-based GPU 上 `LOAD`
的代价本来就小。**这条不值得再投入**，下一条候选应挑更直接削减工作量的。

## 灰档改写 #2 候选：低分辨率渲染 + 我们的超分算子（可行性已验，未实现）

只读观测两轮，结论：**可行，且放大那一步已精确定位**。
证据 [`evidence/07_sr_feasibility/RESULT.md`](../evidence/07_sr_feasibility/RESULT.md)。

- swapchain **2141×969 / `R8G8B8A8_UNORM`**；把游戏内「渲染精度」从 `极高` 调到 `低`，
  整条 3D 管线跟着缩（主场景 2140×968 → 1134×514），**唯独帧内最后一个 pass 恒等于 swapchain**。
- 那个 pass 每帧约 56 笔 draw = **一笔全屏放大 draw + 约 50 笔 UI draw**，
  即 **UI 与放大在同一个 pass 内**，UI 紧跟在放大之后 →
  **不能把整个 pass 换成 compute dispatch**。
- 描述符溯源确认：该 pass 内**只绑 1 个 descriptor set**，第一笔 draw **只采样 1 张图**，
  且这张图**就是**上一个渲染分辨率 pass 的颜色附件。
- 这张图 `usage = 151` = `TRANSFER_SRC | TRANSFER_DST | SAMPLED | COLOR_ATTACHMENT | INPUT_ATTACHMENT`。
  **`SAMPLED` 在 → 我们能读**（compute 里当 `sampler2D`，超分本来就要多抽头采样）；
  `STORAGE` 不在，但我们不写它，写的是自己建的图，**不构成障碍**。

实现路径：`BeginRenderPass` 前插 compute dispatch → 写我们自己的送显分辨率图 →
拦那一次 `vkCmdBindDescriptorSets`，用同一个 layout 另建 set（拷贝原内容、只换那张图的 binding）
→ 绑我们的 set。算子按既有契约装 `candidate.spv`（binding 0 低分辨率只读 / binding 1
送显分辨率只写 / 16×16），装不上硬失败。

**下一步建议先探反作弊**：做一个「建 pipeline + 建 image + 替换描述符，但算子是 1:1 拷贝」
的最小改写，画面应逐像素不变 —— 把「反作弊放不放行」与「算子好不好」分开验，
免得一上来投 500+ 行再卡在反作弊上。

passdump 观测工具已按评审加固（rust-port 评审 11 条中余下的 8 条全部修掉）：
RP2 创建的 pass 现在也进形状表；pass_table 过滤改用「按提交折算的 frames_seen」，
预录命令缓冲的 pass 不再被误滤，且每行多一个 `frames` 字段；采样窗挪到第 1750 帧
（不再是 300 的倍数，落盘帧不再必为空序列），marker 新增 `frame_seq_ready` 布尔，
「空序列」与「还没采」可分；描述符采样图条目带 `binding` 号；二级命令缓冲的 draw
计入（继承信息取 pass，CmdExecuteCommands 并入主缓冲录制清单）；image/view/fb/cb
销毁即摘表，长跑不再只增不减；swapchain 尺寸读写同锁；passdump 模式 logcat 只打
摘要（全文超单条 4KB 上限必被截），全文只走文件通道。**消费方兼容**：harness 只读
`readonly_stats` 与文件全文，本次全部是新增字段，无破坏。

## 下一步（按性价比排）

1. 超分算子：先做上面那个最小改写探反作弊，过了再投真算子。
2. 备选改写候选：**RT 精度降级 / 格式替换**（A 类）—— 比 LoadOp 更直接地削工作量。
   每条仍走 A/B + `begins` 自洽检查。
2. 功耗那一栏补齐（要跟帧数据一样跑满 5 对）。
3. 鸿蒙机制调研（问题 3），等设备。
