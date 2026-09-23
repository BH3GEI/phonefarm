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

### 三个还没验的风险（step 2 第一件事就是逐个排掉）

1. **那笔 draw 到底采的是哪个 image**：我是从「尺寸随渲染精度缩 + draw 数」推断 pass 49→50
   是放大，**还没确认**第一笔 draw 的 descriptor 里绑的就是 pass 49 的输出。得先用层把
   `vkCmdBindDescriptorSets` 的内容打出来对上号。
2. **反作弊**：目前只验过只读层与 loadOp 改写放行。建 pipeline / 改描述符比那两者侵入性大得多，
   得单独探。
3. **pass 49 的输出是否带 `SAMPLED` 以外的 usage**：我们要用 compute 读它，需要
   `VK_IMAGE_USAGE_SAMPLED_BIT`（采样读即可，够用）；写目标是我们自己的 image，
   usage 自己定，不碰 swapchain 的 usage flags（`DESIGN_COMPUTE_HOOK.md` §2(a) 踩过这个坑）。

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
| 确认那笔 draw 的 descriptor（风险 1） | 小，半天，纯观测 |
| 层里的 compute 注入（image/pipeline/barrier/spv 装载） | ~500–700 行，是这个仓库目前最大的一块 |
| 描述符替换 | 小但最容易出错，得能精确命中"那一笔" |
| 画质真值链路（dump + 对齐 + 指标） | 中等，可复用 real-frame-psnr 那条线 |
| A/B（帧时 + 停充功耗 + 画质） | 与 LoadOp 那轮同构，driver 现成 |

合计比 LoadOp 那条大一个量级。**建议 step 2 先只做风险 1**（确认 descriptor），
确认了再投 compute 注入；确认不了就说明这条路要换方式（例如改走 pass 49 的 framebuffer
尺寸 + 让游戏自己的放大 draw 去采我们的图）。

## 证据

| 文件 | 内容 |
|---|---|
| `passdump_render_extreme.json` | 渲染精度 `极高` 的 pass 形状表 + 一帧有序序列 |
| `passdump_render_low.json` | 渲染精度 `低` 的同上，对比用 |

两份都含 `swapchain` 尺寸、`pass_table`（每帧都在跑的 pass）、`frame_seq`（稳态一帧的有序序列）。
