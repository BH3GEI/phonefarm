# 超分算子可行性（第 1 步，只读层）：能做，但要动描述符，不是换个 pass 那么简单

设备 NX809J / 原神 7.1.0 / 游戏内 60 帧 / 风扇开启，2026-09-23 10:49–10:56

方法：给只读层加了 `passdump` 档，导出每个 render pass 的**宽高 / 格式 / 附件数 / draw 数**、
swapchain 尺寸，以及稳态里**一整帧的有序 pass 序列**。
然后把游戏内「渲染精度」从 `极高` 调到 `低`，对比两份 dump —— 谁跟着缩，谁不跟着缩，
一眼就分出来了。（测完已把设置**改回 `极高`**。）

## 结论：放大发生在最后一个 pass 的第一笔 draw，UI 就画在同一个 pass 里

swapchain：**2141×969，format 37（`R8G8B8A8_UNORM`）**

| | 渲染精度 `极高` | 渲染精度 `低` | 跟着缩？ |
|---|---|---|---|
| 主场景 pass（att=4，draws≈170/帧） | 2140×968 | **1134×514** | ✅ |
| 半分辨率后处理（att=2/3） | 1070×484 | 567×257 | ✅ |
| 各类小 RT（阴影/AO 等） | 128² / 256² / 1024² | 同左 | ❌ 与分辨率无关 |
| **帧内最后一个 pass**（pass 50，att=2） | **2141×969** | **2141×969** | ❌ **恒等于 swapchain** |
| 它前面那个 pass（pass 49，att=1） | 2140×968 | **1134×514** | ✅ |

一帧的收尾序列（`低`）：

```
... [32] pass 49  1134x514  att=1   ← 场景最终合成, 在渲染精度下
    [33] pass 50  2141x969  att=2   ← swapchain 分辨率, 56 draws/帧
```

pass 50 每帧约 **56 笔 draw**（`极高` 时约 50 笔），而它的尺寸**不随渲染精度变化**。
所以它 = **一笔全屏放大 draw（把 pass 49 的低分辨率结果采样放大）+ 约 50 笔 UI draw**。

> **直接回答"UI 是否在它之后画"：UI 不在它之后，UI 就在同一个 pass 里、紧跟在放大那笔 draw 后面。**

这条决定了实现路径：**不能把 pass 50 整个换成一次 compute dispatch** —— 那样 UI 就没了。

## 能不能在层里换掉这一步：能，但要动描述符

可行路径（vkBasalt 那类后处理注入层的标准做法，机制上已被证明可行）：

1. 在 `vkCmdBeginRenderPass(pass 50)` **之前**插入一次 compute dispatch：
   读 pass 49 的输出（1134×514），写进我们自己建的 2141×969 storage image；
   前后各加一道 image barrier。
2. 让 pass 50 的**第一笔 draw** 改为采样我们的 image —— 这是**唯一的难点**：
   那笔 draw 的 descriptor set 里绑的是 pass 49 的 image view，要在
   `vkCmdBindDescriptorSets` / `vkUpdateDescriptorSets` 上把它换成我们的。
3. 算子本身按既有契约动态装 `candidate.spv`：
   **binding 0 = 只读低分辨率、binding 1 = 只写送显分辨率、工作组 16×16**
   （与 refbench `sr_pipeline` 和 `carriers/anki-sponza/DESIGN_COMPUTE_HOOK.md` 完全一致，不要改）。
4. **装不上必须硬失败**，绝不静默退回游戏自带放大 —— 否则 A/B 会悄悄变成 A 和 A。

层这边要新建的东西：VkImage + VkImageView + VkDeviceMemory（送显分辨率）、
descriptor set layout / pool、compute pipeline、以及 SPIR-V 直接 `vkCreateShaderModule`
（裸 spv，不需要任何 shader 编译器上设备，比 AnKi 那条路还简单 —— 它还得先做反射）。

### 风险 1 已排掉：确认了，那笔 draw 采的就是上一个 pass 的输出

2026-09-23 12:25 纯观测复核（`composite_draw_probe.json`）：

```json
{"sets_bound_in_pass": 1, "confirmed": true,
 "hit":                   {"w":2140,"h":968,"fmt":37,"usage":151},
 "prev_pass_attachments":[{"w":2140,"h":968,"fmt":37,"usage":151}],
 "sampled_by_first_draw":[{"w":2140,"h":968,"fmt":37,"usage":151,"is_prev_pass_output":true}]}
```

证据很干净：送显分辨率 pass 里**只绑了 1 个 descriptor set**，它的第一笔 draw
**只采样 1 张图**，而这张图正是上一个（渲染分辨率）pass 的颜色附件。
不是"有一张碰巧对上"，是"只有这一张，而且就是它"。

**`usage = 151` 拆开**（`VkImageUsageFlagBits`）：

| 位 | 含义 | 有无 |
|---|---|---|
| `0x01` | TRANSFER_SRC | ✅ |
| `0x02` | TRANSFER_DST | ✅ |
| `0x04` | **SAMPLED** | ✅ |
| `0x08` | STORAGE | ❌ |
| `0x10` | COLOR_ATTACHMENT | ✅ |
| `0x80` | INPUT_ATTACHMENT | ✅ |

→ **我们能读它**：`SAMPLED` 在，compute 里当 `sampler2D` 采样即可（超分本来就要做双线性/多抽头采样，
采样器正是想要的形式）；`TRANSFER_SRC` 也在，必要时还能直接 copy 出来。
`STORAGE` 不在，所以**不能把它当 storage image 直接写** —— 但我们本来也不写它，
写的是我们自己建的送显分辨率图，usage 自己定。**这条不构成障碍。**

### 剩下两个还没验的风险
1. **反作弊**：目前只验过只读层与 loadOp 改写放行。建 pipeline / 改描述符比那两者侵入性大得多，
   得单独探。
2. **描述符替换怎么做得准**：`sets_bound_in_pass = 1` 是个好消息 —— 目标很集中。
   做法是拦 `vkCmdBindDescriptorSets`（在送显 pass 内、第一笔 draw 之前那次），
   用同一个 `VkDescriptorSetLayout` 另建一个 set，把原 set 的内容拷过来
   （`vkUpdateDescriptorSets` 的 copy 形式），只把那张图的 binding 换成我们的 view，
   然后绑我们的 set。**不要直接改原 set** —— 同一个 set 可能别处还在用。
   还差一个小信息：那张图具体在第几个 binding（层里已经按 `(set, binding)` 存了，
   下一轮把 binding 号一起打出来即可，不用再上机专门跑一次）。

## 画质真值怎么拿

- **参考帧**：同一站位、同一镜头角度，渲染精度 `极高`（原生 2140×968）跑一遍截帧。
- **对照帧**：渲染精度 `低`（1134×514）分别走 ① 游戏自带双线性放大 ② 我们的算子。
- **对齐**：人物位置由构造不变（负载只转视角不走位）；镜头角度用固定拖拽序列复现。
- **比较区域**：**只比场景、不比 UI**。UI 是在放大之后画的、两臂逐像素相同，
  算进去会把 PSNR 冲高、掩盖场景差异。两种做法：截屏后按固定矩形抠掉 UI 区域；
  或者更干净 —— 让层直接把 **pass 49 的输出**和**我们算子的输出**dump 成 raw 图，
  完全绕开 UI。建议用后者。
- **时间窗**：原神昼夜循环是最大的不可重复来源（loop_v1 README 实测跨黄昏离散度 5.35%
  vs 深夜 1.37%），三组截帧必须在同一个光照窗口内连续跑完。

## 工作量估计

| 项 | 量级 |
|---|---|
| ~~确认那笔 draw 的 descriptor（风险 1）~~ | ✅ **已完成**，结论见上 |
| 层里的 compute 注入（image/pipeline/barrier/spv 装载） | ~500–700 行，是这个仓库目前最大的一块 |
| 描述符替换 | 小但最容易出错，得能精确命中"那一笔" |
| 画质真值链路（dump + 对齐 + 指标） | 中等，可复用 real-frame-psnr 那条线 |
| A/B（帧时 + 停充功耗 + 画质） | 与 LoadOp 那轮同构，driver 现成 |

合计比 LoadOp 那条大一个量级。风险 1 已排掉且结果比预期好（只有 1 个 set、1 张采样图），
**下一步建议先探反作弊**：做一个"建 pipeline + 建 image + 替换描述符但算子是 1:1 拷贝"
的最小改写，画面应当与原来逐像素相同 —— 这样能把"反作弊放不放行"和"算子好不好"
两件事分开验，不至于一上来就投 500+ 行然后卡在反作弊上。

## 证据

| 文件 | 内容 |
|---|---|
| `passdump_render_extreme.json` | 渲染精度 `极高` 的 pass 形状表 + 一帧有序序列 |
| `passdump_render_low.json` | 渲染精度 `低` 的同上，对比用 |
| `composite_draw_probe.json` | 送显那一笔 draw 的描述符溯源（含 `usage`），风险 1 的判据 |

两份都含 `swapchain` 尺寸、`pass_table`（每帧都在跑的 pass）、`frame_seq`（稳态一帧的有序序列）。
