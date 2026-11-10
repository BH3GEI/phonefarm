# 原神一帧多余步骤报告（阶段 1）

> 2026-10-11 · frame agent · 设备 红魔 NX809J（Adreno 840，Android 16，root）· 原神 7.1.0（`7.1.0_48052158_48145775`），游戏内 60 帧档
> 工具：灰档层只读 `frameaudit` 档 + `phonefarm frame-audit analyze`（[#85](https://github.com/BH3GEI/phonefarm/pull/85)、[#90](https://github.com/BH3GEI/phonefarm/pull/90)，以及本 PR 的分析修正）
> 证据：[`knobs/evidence/10_frameaudit/`](../knobs/evidence/10_frameaudit/)。本文所有数字都能在那里的 `analysis.md` / `analysis.json` / `fa_1.json` 里找到。

## 一句话结论

**原神这一帧已经很干净，能安全去掉的多余步骤很少。** 确定是「白存」（存回显存后没人读）的只有 3 处，合计约 **2.46 MB/次 present**（按模型估算，是未压缩上限），换算下来约 150 MB/s，估计省电 **6–24 mW（不到整机的 0.5%）**。这低于现有 A/B 能测出来的下限。唯一可能测得出差别的候选是整个 pass rp112（GPU 时间约占 1.7%），但它很可能是遮挡查询，要先验证才能动。**建议阶段 2 只把 storeOp 开关在原神上做成「真机不出错」的样例，主力转到 onboard 接入的 Megacity 去找收益。**

---

## 1. 怎么抓的、数字的口径

- **场景**：璃月港（夜间），角色站定，没有转视角负载（只看结构，不比性能）。冷启动后进入大世界，等 15 秒，抓 3 次，每次间隔约 20 秒（15:16:03 / 15:16:31 / 15:17:00 PDT）。夜间光照稳定，三次之间没跨昼夜。
- **每次抓连续 3 次 present 之间的全部 GPU 提交**（每次 present 对应 2 次 `vkQueueSubmit`）。三次抓取的结构完全一致：每次 present 都是 **33 个 render pass + 20 次 compute dispatch + 2 次 buffer→image 上传 + 12 次 UNDEFINED 丢弃**，pass 外没有一次 clear/copy/blit/resolve。draw 数在 484–522 之间。33 个 pass 的 sig 在三次抓取里逐个相同。
- **口径**（遵守 .github#20 显示指标合同）：
  - 本文的「一帧」都指「**层数到的两次 present 之间**」，不是游戏 FPS。层不测帧率。
  - **模型带宽** = 附件宽 × 高 × 每像素字节 × 采样数：loadOp=LOAD 记一次读，storeOp=STORE 记一次写，深度和模板分开算。这是**未压缩上限**，因为 Adreno 的 UBWC 压缩和驱动的直绘（sysmem）模式都会让真实值更小。纹理采样的流量不在里面。
  - **GPU 打点 ms**：层在每个 pass 前后各写一个 `BOTTOM_OF_PIPE` 时间戳（周期 52.083 ns，528/528 个都拿到了）。打点会让 GPU 串行化，所以**绝对值偏大**：33 个 pass 的打点合计 27.19 ms，两次 present 之间的打点跨度 32.5–32.7 ms，而不打点时测到的 GPU 忙碌时间约 13.3 ms（[06_genshin_world](../knobs/evidence/06_genshin_world/RESULT.md)，不同场次）。所以下文**只用打点的占比**，必要时按占比乘 13.3 ms 推算，并标明「推算」。
- **怎么判断「多余」**：按提交顺序排出时间线，追踪每张图每个 mip/layer/aspect（深度和模板分开）「被谁写、下一次被谁碰」。「读」包括：后续 pass 的 LOAD、描述符里引用了这张图（绑了不一定真采样，是保守的近似）、拷贝/blit 的源、present。所以分析**只会把多余判成有用，不会把有用判成多余**。层看不见的东西：fragment shader 写 storage buffer、查询（query），这两项在下文标为「不确定」。

## 2. 一帧的步骤总览（第 1 次抓取的第 0 帧）

提交顺序：17 个小 dispatch（GPU 驱动的剔除/蒙皮一类，读写的都是 buffer）→ 阴影/小图 → G-buffer → 深度派生 → 光照 → 后处理链 → UI 合成到屏幕。

「占比」= 该 pass 打点 ms ÷ 33 个 pass 打点合计（27.19 ms）。下标为附件下标，C=颜色、D=深度模板。

| # | rp | sig | 尺寸 | 附件（下标:格式 load/store） | draw | 模型读/写 KB | 占比 | 角色（推测） |
|---|---|---|---|---|---|---|---|---|
| 17 | 62 | `0e9c2729bbf28f99` | 128² | C0:R11G11B10F DC/STORE | 1 | 0/66 | 0.1% | 小图 |
| 18 | 22 | `fd73b99ccf776147` | 128² | C0:RGBA8 LOAD/STORE | 1 | 66/66 | 0.1% | 累加 |
| 19 | 71 | `9d0b4db80bf552b8` | 256² | C0:R16F DC/STORE, C1:RGBA8 DC/STORE | 1 | 0/393 | 0.2% | **没人读（见 §3）** |
| 20 | 92 | `986b68b70bfd5a72` | 1070×484 | C0/C1:RGBA8 CLEAR/STORE, D2:D24S8 CLEAR/STORE 模板 CLEAR/STORE | 3 | 0/6215 | 1.8% | 半分辨率预渲染 |
| 26 | 107 | `590ca693387e4e8f` | 2140×968 | C0:RGBA8, C1/C2:RGBA8_SRGB, C3:A2RGB10 全 CLEAR/STORE，D4:D24S8 CLEAR/STORE 模板 CLEAR/STORE | 205 | 0/41430 | **22.5%** | G-buffer（主场景） |
| 30 | 30 | `872ac5561f4e2ae6` | 1070×484 | C0:R32F, C1:RGBA8 CLEAR/STORE, D2:D24S8 CLEAR/STORE 模板 CLEAR/NONE | 1 | 0/5697 | 1.8% | 深度降采样 |
| 36 | 61 | `ae75a3d4bc624906` | 128² | C0:RGBA8 LOAD/STORE | 1 | 66/66 | 0.3% | 疑似没人读（见 §3） |
| 37 | 14 | `0a072fce0059b44d` | 512² | D0:D16 CLEAR/STORE | 8 | 0/524 | 0.9% | 阴影 |
| 38 | 31 | `53582d61f781112e` | 1024² | D0:D16 LOAD/STORE | 38 | 2097/2097 | 1.4% | 阴影（累加） |
| 39 | 65 | `9f1bdf6f576d0886` | 512² | C0:RGBA8 DC/STORE | 3 | 0/1049 | 0.8% | |
| 40 | 66 | `e3d50419e22a3125` | 1070×484 | C0:RGBA8 CLEAR/STORE | 3 | 0/2072 | 4.9% | 半分辨率效果 |
| 44 | 103 | `15a51a879df99c32` | 1070×484 | C0:RGBA16F, C1:R11G11B10F, C2:R32F CLEAR/STORE, D3:D24S8 CLEAR/NONE 模板 CLEAR/STORE | 2 | 0/8804 | 1.4% | **C2 没人读（见 §3）** |
| 45 | 98 | `ae7356e317f7e574` | 1070×484 | C0:RGBA16F CLEAR/STORE, D1:D24S8 DC/NONE 模板 **LOAD**/NONE | 1 | 518/4143 | 1.3% | 读 rp103 的模板 |
| 47 | 55 | `a55683fdf11f70f3` | 1070×484 | C0:RGBA16F CLEAR/STORE, D1:D24S8 LOAD/NONE 模板 CLEAR/NONE | 20 | 1554/4143 | 1.3% | |
| 48–49 | 114 | `42e61e9172f926c2` | 70×5、70×1 | C0:RGBA16F LOAD/STORE | 2+2 | 4/4 | 0.3% | 小累加 |
| 50 | 110 | `4c01dc5736fda7bd` | 2140×968 | C0/C1:R11G11B10F CLEAR/STORE, D2:D24S8 LOAD/NONE 模板 LOAD/NONE | 67 | 8286/16572 | **33.8%** | 光照 |
| 51 | 112 | `309ab726665125ef` | 2140×968 | C0:R8 DC/**NONE**, D1:D24S8 LOAD/NONE 模板 LOAD/NONE | 11 | 8286/0 | 1.7% | **什么都不写回（见 §3）** |
| 52 | 60 | `8b4e9a51c693a466` | 2140×968 | C0:A2RGB10 DC/STORE, D1:D24S8 LOAD/NONE 模板 LOAD/NONE | 21 | 8286/8286 | 4.3% | |
| 53 | 57 | `d6ef9fa3a209e062` | 2140×968 | C0:R11G11B10F, C1:R8 DC/STORE | 1 | 0/10358 | 4.9% | 全屏后处理 |
| 54–55 | 61 | `ae75a3d4bc624906` | 1070×484 | C0:RGBA8 LOAD/STORE | 1+1 | 4144/4144 | 5.0% | 累加（有人读） |
| 56 | 67 | `338c5a2c0e03e642` | 1070×484 | C0:RGBA16F DC/STORE | 1 | 0/4143 | 0.9% | |
| 57 | 44 | `c2b9edb60aa8d203` | 1070×484 | C0:R11G11B10F, C1:R8 DC/STORE | 1 | 0/2589 | 0.8% | |
| 58–60 | 62 | `0e9c2729bbf28f99` | 535×242 ×3 | C0:R11G11B10F DC/STORE | 1×3 | 0/1554 | 1.6% | bloom 降采样链 |
| 61–63 | 48 | `18e11847444a7f3b` | 188×158 ×3 | C0:R11G11B10F CLEAR/STORE | 3×3 | 0/357 | 0.5% | |
| 64 | 20 | `bf9e1161a8dd7758` | 535×242 | C0:R11G11B10F DC/STORE | 1 | 0/518 | 0.2% | |
| 65 | 118 | `a2c3d213c4cb1f01` | 2140×968 | C0:RGBA8 DC/STORE, D1:D24S8 LOAD/NONE 模板 LOAD/NONE | 4 | 8286/8286 | 2.8% | 合成 |
| 66 | 115 | `d81fff0491a7e1c5` | 2141×969 | C0:RGBA8（屏幕）CLEAR/STORE, D1:D24S8 CLEAR/NONE 模板 CLEAR/NONE | 71 | 0/8299 | 4.4% | 放大 + UI |

**合计（每次 present）**：33 个 pass，模型读 41.6 MB，模型写 141.9 MB，共 183.5 MB。按每秒 60 次 present 算约 11 GB/s（未压缩上限，不含纹理采样）。

几点观察：

- 原神**已经在做**移动端该做的事：几乎所有 pass 都用 CLEAR 或 DONT_CARE 开场。主深度（#2913）只在 G-buffer 里 STORE 一次，后面 4 个 pass 都是 LOAD + **storeOp=NONE**（只读不写回）。屏幕 pass 的深度不写回。没有任何 pass 外的 clear/copy/blit/resolve。
- 真正的大头是 **G-buffer（rp107，写 41 MB）和光照（rp110）**，两者合计约占 56% 的打点时间。它们产出的东西都被后面读了，是延迟渲染的正常代价，**不是多余**。想省这部分得合并 pass 或用 subpass 留在 tile 上，这在层里做不到，或者风险极高（见 §5）。

## 3. 疑似多余的步骤（逐条）

「每帧」= 每次 present。三次抓取各给出 3 帧，只有后面还有完整一帧可看的帧才参与判定，每次抓取得到的结论都一样。

| 编号 | 是什么 | 为什么认为多余 | 能省多少（怎么估的） | 灰档层能不能做 / 风险 |
|---|---|---|---|---|
| **A** | rp103（sig `15a51a879df99c32`）附件 **C2**：R32F 1070×484，CLEAR/STORE | 存回显存后，下一次碰这张图（#3014）是下一帧的 UNDEFINED 丢弃，中间没有任何 pass 采样、LOAD 或拷贝它。三次抓取、6 个可判帧全部如此。**确定** | 写回 2.07 MB/帧（1070×484×4 B）。约 124 MB/s，按 40–160 pJ/B（LPDDR5X 系统级每字节能耗，取文献区间，**不是实测**）约 5–20 mW | **写回 storeOp→DONT_CARE**，在 CreateRenderPass 时改，与 LoadOp 改写同一套机制。这个 sig 在本次只对应 1 个 pass，内容确实没人读，所以**画面应逐像素不变**。风险：别的场景里同一个 sig 的 pass 如果有人读 C2 就会画错，所以开关要带「被读即停用」的自检，阶段 2 要换几个场景对比截图 |
| **B** | rp71（sig `9d0b4db80bf552b8`）附件 **C0**（R16F 256²）和 **C1**（RGBA8 256²），DC/STORE，1 笔 draw | 两张图（#2986、#2987）写完后下一次被碰是丢弃，没人读。整个 pass 的产出都没人读。**确定**（两块附件都是） | 写回 393 KB/帧（约 24 MB/s，<5 mW）。如果整个 pass 跳过，再省约 0.2% 的打点时间（推算约 0.02 ms） | 两种做法：storeOp→DONT_CARE（安全，收益极小）；或跳过整个 pass（需要确认它没有 storage buffer 写或查询，**不确定**） |
| **C** | rp112（sig `309ab726665125ef`）整个 pass：R8 颜色 DC/**NONE**，深度和模板 LOAD/NONE，11 笔 draw | 它**一个字节都不写回**：颜色 storeOp=NONE，深度模板只读。按层能看到的信息，它的输出没有去处。**但很可能是遮挡查询**（只测深度、不要颜色的 11 笔 draw，这是遮挡剔除的典型形态），层目前没钩 `vkCmdBeginQuery`，**不确定** | 打点占 1.7%，推算 GPU 忙碌时间约 0.22 ms/帧（1.7% × 13.3 ms），外加深度读入 8.3 MB/帧（模型值）。这是本报告里**唯一可能测得出**的一项 | **整 pass 跳过**。先补一个只读钩子统计这个 pass 里有没有 BeginQuery/EndQuery；有就不能去（去了遮挡剔除会错，物体可能闪烁或消失）。没有的话才进阶段 2，且必须对比画面 |
| D | rp61 的 128² 那一次（sig `ae75a3d4bc624906`），C0 RGBA8 LOAD/STORE | 到抓帧结束（≥1 整帧）都没人读，**疑似**，可能是隔几帧才读的历史缓冲 | 66 KB/帧，可忽略 | **不能按 sig 改**：同一个 sig 还有两个 1070×484 的 pass（#54、#55），它们的输出是有人读的。改了会画错 |
| E | 可以建成 transient（LAZILY_ALLOCATED）的附件：#3014（A 的那张）、#2986/#2987（B 的那两张），以及 #3016（rp112 的 R8）、#124（屏幕 pass 的深度） | 只在 pass 内用，从不被别的步骤读 | 带宽收益**就是 A+B 的写回**，不额外增加。#3016 和 #124 现在已经是 storeOp=NONE，带宽为零，只省显存（约 2.1 MB + 8.3 MB） | 要在 CreateImage 时改 usage 加 TRANSIENT 并选 LAZILY_ALLOCATED 内存。这台机器有没有这种内存类型**不确定**（没查）。改了以后一旦有人读就是未定义内容。收益只是显存，**不推荐优先做** |

lead 点名的另外两类，结论如下：

- **整屏清空又整屏覆盖（clear + 全屏 draw）**：没有找到可去的。一帧里没有一次 pass 外的 `vkCmdClearColorImage`/`ClearDepthStencilImage`，开场清屏全部走 loadOp=CLEAR，这在 tile GPU 上是免费的。pass 内的 `vkCmdClearAttachments` 每帧 4 次：阴影 rp31（1024² D16）2 次局部清（只清一块区域，其余保留，属正常）；屏幕 pass rp115 2 次整屏清，但这个 pass 本身已经是 loadOp=CLEAR。层只记了次数，没记它们在 pass 里的位置，所以多半是「放大画完后、画 UI 前」把深度/模板再清一遍，这是正常用法，**不确定是否多余**。即使多余，tile 内清屏也几乎不花带宽，不值得做开关。
- **整屏 blit/copy 后目标又被覆盖或没人读**：没有。一帧里只有 2 次 128×36 的 buffer→image 上传（数据更新），没有 image 之间的 blit/copy/resolve。
- **「白读」（LOAD 读进来的是未定义内容）**：0 处。12 个颜色/深度 LOAD 和 5 个模板 LOAD 读的都是前面步骤刚产出的有效内容（详见 §4）。

**合计**：确定的白存（A+B）约 **2.46 MB/帧**，约占附件模型流量（183.5 MB/帧）的 **1.3%**，约 150 MB/s，估计 **6–24 mW**。整机功耗约 4.8 W（[STATUS](https://github.com/HGamey/game_opt_loop/blob/main/STATUS.md) 基线 4.76 W），所以不到 0.5%。现有 5v5 A/B 的功耗离散在 ±4% 量级（upop 那轮 +4.07% 都是 p=0.28 的噪声），**测不出来**。

## 4. 上个月 LoadOp 改写为什么没有收益

上次（[06_genshin_world](../knobs/evidence/06_genshin_world/RESULT.md)）把所有 loadOp=LOAD 一律改成 DONT_CARE，改写确实命中了（每轮 31–37 条真被绑），但 GPU 忙碌时间 +0.017%（p=0.944），带宽 +0.83%（p=0.524），没有收益。这次的清单可以解释原因：

1. **那些 LOAD 全部是有用的**。每帧有 12 个附件 LOAD 颜色或深度，另有 5 个 LOAD 模板，它们的上一步都是一个真正写回了内容的 STORE，没有一处是读未定义内容。所以那次改写**不是在去掉多余步骤，而是在破坏正确性**。只是当时没做画面对比，没发现。其中最大的一块是主深度 #2913：被 rp110、rp112、rp60、rp118 四个全分辨率 pass 各 LOAD 一次，模型值 4 × 8.3 MB = 33 MB/帧，占全部 LOAD 的 80%。这四个 pass 都要用主深度做深度测试，改成 DONT_CARE 后，深度测试读到的是垃圾。
2. **为什么改错了 GPU 时间却没变**：最可能的解释是，Adreno 驱动把这类 pass 放在**直绘模式**（sysmem/bypass，不走 tile 内存）下渲染。这几个 pass 的特点是附件少、深度只读、draw 不多，而在直绘模式下 loadOp/storeOp 根本不产生搬运，改成什么都一样。另一种可能是 UBWC 压缩让 LOAD 本来就很便宜。两者现在都**不确定**：驱动对每个 pass 选哪种模式，层看不到，Perfetto 的 GPU 计数器在这台机器上也没注册。
3. **避免再踩坑**：
   - **只改「确定没人用」的那一侧**。改 LOAD 之前要先证明上一步是丢弃，改 STORE 之前要先证明下一步不读。本报告的 analyze 就是做这个判断的，阶段 2 的开关只应该吃它给出的 sig 和附件下标，不要整类一刀切。
   - **每个改写都要配画面对比**：有层/无层逐像素比较，或者至少看截图差异。「统计不显著」不等于「画对了」。
   - **在这台机器上，tile 内存相关的改写，收益天花板可能就很低**，因为驱动会自己选直绘。阶段 2 做 storeOp 开关时，同时用打点时间戳看被改的 pass 是否变快。看不到变化就说明那个 pass 在直绘模式，这一类改写在这台机器上整体无效，应尽早停。

## 5. 大头但不是「多余」的步骤（记一笔，阶段 2 不做）

| 步骤 | 规模 | 为什么不算多余 | 想省要怎么做 |
|---|---|---|---|
| G-buffer rp107 写回 4 张颜色 + 深度模板 | 41.4 MB/帧，打点 22.5% | 后面的光照、后处理都读 | 把 G-buffer 留在 tile 上，后续 pass 改成 subpass 或 input attachment（**白盒**才能做） |
| 主深度 #2913 被 4 个 pass 反复 LOAD | 33 MB/帧（模型） | 每个 pass 都要用 | 合并成一个多 subpass 的 render pass（层里改 pass 结构，风险极高） |
| 光照 rp110 全分辨率两张 R11G11B10F | 16.6 MB/帧，打点 33.8% | 后面读 | 降分辨率属于画质改动，不归「去多余步骤」（超分路线已有 upop） |

## 6. 阶段 2 开关清单（按「省得多且安全」排序）

| 排名 | 开关 | 类型 | 定位 | 预估收益 | 安全性 / 前置 |
|---|---|---|---|---|---|
| 1 | rp103 C2 storeOp STORE→DONT_CARE | 写回 storeOp | sig `15a51a879df99c32`，附件下标 **2** | 2.07 MB/帧写回（约 124 MB/s，约 5–20 mW，推算） | 安全（确定没人读）。要带「被读即停用」自检，并做画面逐像素对比 |
| 2 | rp71 C0、C1 storeOp STORE→DONT_CARE | 写回 storeOp | sig `9d0b4db80bf552b8`，附件下标 **0、1** | 393 KB/帧 | 安全。可以和 1 合成一个开关一起测 |
| 3 | ~~rp112 整个 pass 跳过~~（**已作废**：§9 证实它带查询） | 整 pass（非 storeOp） | sig `309ab726665125ef` | 推算约 0.22 ms GPU 忙碌时间/帧（1.7%）+ 8.3 MB 读入 | **先验证**：补只读钩子看有没有遮挡查询。有就放弃这一条。即使没有，也要做画面对比 + 多场景测试 |
| 4 | rp71 整个 pass 跳过 | 整 pass | sig `9d0b4db80bf552b8` | 约 0.02 ms（推算） | 和 3 同样的前置。收益太小，排在后面 |
| 5 | #3014、#2986、#2987 改 transient | transient | rp103 附件 2；rp71 附件 0、1 | 带宽和 1、2 重叠，额外只省显存 | 先确认有没有 LAZILY_ALLOCATED 内存类型。不推荐优先 |

**不要做的**：按 sig `ae75a3d4bc624906` 改 storeOp（同一个 sig 下有被读的 pass）；整类 LOAD→DONT_CARE（§4）；任何 clear/copy 类开关（原神里没有这类步骤）。

**对阶段 2 的期望要实事求是**：1+2 合起来在功耗上**测不出来**（<0.5%，低于检出下限）。它们的价值是证明「AI 找出多余步骤、改掉、画面不变」这条链路在真游戏上走得通，而不是证明省电。唯一有希望在 GPU 忙碌时间上测出差别的是 3，而且它要先过遮挡查询那一关。按蓝图的判断规则（原神查不出值得去的步骤就换游戏），**建议阶段 2 用原神只做 1+2 的机制验证，同时把收益寻找转到 onboard 正在接入的 Megacity**。Megacity 是 Unity 的移动样例，带 URP 默认配置，常见的深度/MSAA 写回、整屏 copy 一类浪费更可能出现。这是**推测**，要等对它跑一遍 `frame-audit` 才知道。`frame-audit capture --pkg <包名>` 直接支持别的包，只是大世界判定（每帧 pass 数 ≥25）和点门坐标目前写死在原神上，换游戏时要先改这两处。

## 7. 复现

```bash
# 真机（拿 frame 锁, 结束自动摘层 + 关游戏）
pf-node -o ./ev phonefarm frame-audit capture --out @out/fa --ts --captures 3 --gap 20 --lock-label frame
# 离线
phonefarm frame-audit analyze ev/fa/fa_1.json ev/fa/fa_2.json ev/fa/fa_3.json --md analysis.md --json analysis.json
```

本次的还原：`debug.vulkan.layers` 为空，`.so` 和 state 已删（`capture_meta.json` 的 `restore` 字段），游戏已关闭，媒体音量 stream 3 = 0。全局属性的暴露窗口约 4 秒。

## 8. 工具本身的已知局限

- 「读」靠描述符引用来近似：保守，只会少报多余。
- 层看不见 fragment shader 里写的 storage buffer，也看不见查询（query）。所以「整 pass 白画」只标为疑似。
- `vkCmdCopyImage2` 这类 Vulkan 1.3 的拷贝命令没有钩（原神没用：一帧里的拷贝只有 2 次 buffer→image，都是 1.0 接口）。
- 打点时间戳会串行化 GPU，绝对值偏大，只能用占比。
- 本次修正了一个分析器 bug：原来把深度和模板混成一块判断，会把「深度 DONT_CARE 但模板 LOAD」的 pass（rp98）当成覆盖，进而把 rp103 的模板写回误报成白存。修正后深度和模板分开追踪，并加了单测。

## 9. 阶段 2 机制验证：storeOp 开关 1+2 在原神上（2026-10-11 19:3x，[11_storeop_check](../knobs/evidence/11_storeop_check/)）

用 `phonefarm frame-audit storeop-check`（[#96](https://github.com/BH3GEI/phonefarm/pull/96)、[#106](https://github.com/BH3GEI/phonefarm/pull/106)）跑，规则用完整 16 位 sig：`15a51a879df99c32:2,9d0b4db80bf552b8:0,9d0b4db80bf552b8:1`。层用「孪生 pass」现场开关：应用拿原样 pass，开关打开时在 BeginRenderPass 换成只差 storeOp 的孪生 pass。所以同一局、同一位置、同一视角下可以 关→开→关→开→关 交替截图，每段 3 张，每张间隔约 1 秒。

| 验收项 | 结果 |
|---|---|
| 改写命中（effective 的 begins>0） | **3 个目标附件全部命中**：sig `9d0b4db8…` 的附件 0、1 各 begins=716，sig `15a51a87…` 的附件 2 begins=748（只在开关开着的那几段计数）。effective 共 6 条，因为 #91 对颜色附件也多登记了一条无实际作用的 `stencilStoreOp` |
| 改写真的生效 | 开关开着时同局抓的步骤清单（`fa_on_analysis.md`）显示：sig `9d0b4db8…` 两个附件变成 DONT_CARE/DONT_CARE，模型写回 393→0 KB；sig `15a51a87…` 附件 2 变成 CLEAR/DONT_CARE，该 pass 模型写回 8804→6732 KB。确定白存从 3 处降到 0 处 |
| 不崩不报错 | 全程游戏存活，crash 日志 0 行，结束时自动摘层、关游戏 |
| 画面一致 | **在噪声内，但做不到逐像素相同**（数字见下表） |

画面比对（全屏 2688×1216，排除小地图、顶部图标、队伍、技能区、UID、左下聊天等 UI 区域；「差异像素」= 任一通道差 >8）：

| 比较 | 对数 | 差异像素 % | 平均每通道绝对差 |
|---|---|---|---|
| 同一段内相隔 1 秒（纯动画噪声） | 5 | 3.3–12.9（均 6.4） | 1.6–4.3（均 2.5） |
| 隔一段的同状态（关~关、开~开，间隔约 12 秒） | 3 | 15.4–22.0（均 18.2） | 4.6–8.2（均 6.0） |
| **相邻段的关~开（间隔约 6 秒）** | 4 | 11.4–20.2（均 15.0） | 3.6–7.4（均 5.3） |

关~开的差异落在同状态噪声的范围内，没有比噪声更大。但夜景里人物待机动作、NPC、水面和灯光一直在动，噪声底很高（1 秒内就有 3–13% 像素不同）。所以这一项只能说**没看到改坏，不能说逐像素相同**。能保证画面不变的，是结构上的证据：这三块内容在抓到的帧里没有任何步骤读它们（§3），改写后它们也确实不再写回。要做严格的逐像素证明，得换一个能完全静止的画面（例如 Megacity 的 `city_static`），或者让层直接比较被改附件下游 pass 的输出。

**顺带结论：rp112 确实带查询，不能跳过。** 这次的步骤清单来自加了查询钩子（[#94](https://github.com/BH3GEI/phonefarm/pull/94)）的层：每帧有 1 个 pass 带查询，就是 sig `309ab726665125ef` 那个（11 笔 draw、不写回任何附件），它也因此不再被判为「整个 pass 白画」。按它的形态看是遮挡剔除，所以候选 3、4（整 pass 跳过）**作废**。原神在灰档层能安全拿到的收益，就只剩上面这 2.46 MB/帧的写回，和 §3 的结论一致。

**局限**：本轮截图 PNG（约 7 MB）从节点回传时多次断流没拉回来，比对数字是在节点上算完带回的（`storeop_check.json`），原始截图留在节点任务目录 `55aa56ff8290`。
