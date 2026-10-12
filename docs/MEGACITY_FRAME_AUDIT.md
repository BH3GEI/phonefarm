# Megacity 一帧多余步骤报告（阶段 2-b，与原神对比）

> 2026-10-11 · frame agent · 设备 红魔 NX809J（Adreno 840）· Megacity Metro（Unity 6，Vulkan，我们自己打包的白盒载体，`com.unity.megacity.metro`）
> 场景：`city_static`（相机钉在路线起点不动，场景块已加载完），启动参数 `--es scene city_static --es frames 100000`
> 工具与口径同 [原神报告](GENSHIN_FRAME_AUDIT.md) §1：模型带宽是未压缩上限，GPU 打点只用占比；「一帧」= 层数到的两次 present 之间，不是游戏 FPS。
> 证据：[`knobs/evidence/12_megacity/`](../knobs/evidence/12_megacity/)（节点任务 f464bea1724f）

## 一句话结论

**Megacity 的浪费比原神多、也更典型。** 确定白存每帧 7.7 MB，是原神的 3 倍。更大的一块是 **4×MSAA 缓冲被写回显存又读回来**：每帧约 186 MB（模型值），占全部附件流量的 66%，这正是分块 GPU 上最典型的浪费。原神那边的大头是延迟渲染的正常代价，Megacity 这边的大头是「本该留在片上的东西被搬出去了」。**建议阶段 3 的主战场选 Megacity**（PUBG 接入后再补对比）。

---

## 0. 先作废的一轮（记下来避免再踩）

第一轮抓取（任务 316e5fd8881d）里，所有 pass 的描述符引用都是 0，分析器因此报了「每帧 65 MB 白存、4 个 pass 白画」，连 4096² 阴影图都被判成没人读。**全是假阳性**。原因是 Unity 6 用 `vkUpdateDescriptorSetWithTemplate` 更新描述符，而层当时只钩了 `vkUpdateDescriptorSets`，「读」全部看不见。修复在 [#113](https://github.com/BH3GEI/phonefarm/pull/113)：钩了描述符模板和 push 描述符，分析器也加了自检，看不见读时一律不报。本轮用新层重抓：`tmpl_updates`=64509，每帧能看到 87–88 处描述符引用，阴影图被主 pass 的 74 处引用读到。

另外，那一轮落在 open 中止 optimize 的可疑窗口里，系统参数可能被动过。本轮在那之后跑，跑之前核对过没有残留进程，三个白名单参数（DDR/LLCC boost、policy0）都是原值，只有 policy6 max 是厂商温控压出来的 1382400。

## 1. 一帧的步骤总览

三次抓取共 9 帧，结构完全一致：**2 次 buffer 拷贝 + 1 次 dispatch + 8 个 render pass**，draw 592–601 笔，模型读 108.8 MB、写 173.7 MB，合计 282.5 MB/帧。`ev_overflow`=1，有一条命令缓冲超过 4096 个事件被截断。它不在抓到的帧里（每帧 11 个事件，三次一致），但**不能完全排除**这个截断漏掉了别的步骤。

「占比」= 打点 ms ÷ 本帧 8 个 pass 的打点合计（16.2 ms）。x4 = 4 倍多重采样。

| # | rp | sig | 尺寸 | 附件（角色:格式 load/store） | draw | 模型读/写 KB | 占比 | 角色（推测） |
|---|---|---|---|---|---|---|---|---|
| 3 | 13 | `2b01c0d5a1d5e34e` | 4096² | D:D16 CLEAR/STORE | 314 | 0/33554 | **41.4%** | 阴影图（被主 pass 采样） |
| 4 | 17 | `e51e2a0b50cc0287` | 2068×935 | D:D24S8 CLEAR/STORE 模板 CLEAR/STORE, C:RGBA8_SNORM CLEAR/STORE | 138 | 0/15469 | 13.0% | 深度+法线预渲染（URP DepthNormals，给 SSAO） |
| 5 | 19 | `c3a64d53ba8837bb` | 2068×935 | **D:D24S8 x4 CLEAR/STORE** 模板 CLEAR/STORE, **C:R11G11B10F x4 CLEAR/STORE** | 139 | 0/61875 | **32.1%** | 主场景（4×MSAA） |
| 6 | 21 | `b86d017ba7deaeeb` | 1034×467 | C:R8 DC/STORE | 1 | 0/483 | 2.7% | 半分辨率 SSAO |
| 7 | 38 | `67c02862fc88532b` | 1034×467 | C:R8 LOAD/STORE | 1 | 483/483 | 0.3% | SSAO 模糊 |
| 8 | 32 | `fb2e9bc6efb185c5` | 2068×935 | **C:R11G11B10F x4 LOAD/STORE**, R:R11G11B10F DC/STORE | 1 | 30937/38672 | 5.1% | 读回 MSAA 颜色 + 解析 |
| 9 | 41 | `d88abfb3b0566231` | 2068×935 | **D:D24S8 x4 LOAD/DC** 模板 LOAD/DC, **C:R11G11B10F x4 LOAD/DC**, R:R11G11B10F DC/STORE | 6 | 61875/7734 | 4.4% | 再读回 MSAA 深度+颜色，画透明物，再解析一次 |
| 10 | 40 | `ae9f514c59165b6a` | 2068×935 | D:D24S8 LOAD/STORE 模板 LOAD/STORE, C:RGBA8_SRGB（屏幕）LOAD/STORE | 1 | 15469/15469 | 1.1% | 最终全屏拷贝到屏幕 |

## 2. 疑似多余的步骤

| 编号 | 是什么 | 为什么认为多余 | 能省多少（怎么估） | 灰档层可行性 / 风险 |
|---|---|---|---|---|
| **M1** | rp32（sig `fb2e9bc6efb185c5`）附件 **1**：解析目标 #366（R11G11B10F 2068×935）DC/STORE | rp32 把 MSAA 颜色解析进 #366 并写回，紧接着 rp41 又解析一次覆盖 #366，中间没人读。三次抓取 6 个可判帧全部如此。**确定** | 7.73 MB/帧写回 | **写回 storeOp→DONT_CARE**，和原神同一个开关。严格的逐像素证明见 §4 |
| **M2** | rp19 的 **4×MSAA 深度 + 颜色被 STORE 回显存**，rp32、rp41 再 LOAD 回来（rp32 还把 MSAA 颜色再 STORE 一次） | 分块 GPU 上 MSAA 应该在片上解析、不写回。这里是三个 pass 接力，所以必须写回。这些内容**有人读**，不算「白存」，属于 pass 拆分造成的搬运 | 模型值：rp19 写 61.9 + rp32 读 30.9、写 30.9 + rp41 读 61.9 ≈ **186 MB/帧**（占全部附件流量 66%）。实际受 UBWC 压缩影响会小很多，**不确定** | 层里把三个 pass 合成一个（subpass + 片上 resolve）**风险很高**。透明物在 rp41 里还要读深度，得改成 subpass input，属于结构改写。更现实的做法是阶段 3 把它当「已知最大浪费」交给白盒（Unity 设置里关 MSAA 或改成片上解析）做对照，或者在层里先只验证「rp32 + rp41 合并」这一小步 |
| M3 | rp40（sig `ae9f514c59165b6a`）：屏幕图 LOAD、深度 #201 LOAD/STORE，1 笔 draw | 屏幕图在本帧之前唯一的访问是上一次 present；深度 #201 除了 rp40 自己，没有别的步骤碰。如果这 1 笔 draw 是盖满全屏的拷贝，两次 LOAD 和深度写回都没用。**疑似**：分析器没判它（前一个访问是 present 或自己上一帧的写，按规则算「读」），这是人工看出来的 | 屏幕 LOAD 7.7 MB + 深度 LOAD/STORE 15.5 MB，约 23 MB/帧（模型值） | 屏幕 loadOp→DONT_CARE 和深度 storeOp→DONT_CARE 都是同一类开关，但**必须先做画面比对**：只要那笔 draw 没盖满全屏或用了深度测试，就会画错 |
| — | rp17 深度+法线预渲染又画了一遍几乎同样的几何（138 vs 139 笔） | 输出被 SSAO 读了，不是多余，是开 SSAO 的代价 | 打点 13% | 关 SSAO 属于画质取舍，不归「去多余步骤」 |

没有发现的类别：pass 外的 clear/blit/resolve/copy image（一帧里只有 2 次 buffer 拷贝），白读（LOAD 未定义内容）0 处，带查询的 pass 0 个。

## 3. 和原神对比

| | 原神（[报告](GENSHIN_FRAME_AUDIT.md)） | Megacity |
|---|---|---|
| 每帧 pass / dispatch | 33 / 20 | 8 / 1 |
| 模型读+写 | 41.6 + 141.9 = 183.5 MB | 108.8 + 173.7 = 282.5 MB |
| 确定白存 | 2.46 MB（3 处，1.3%） | **7.73 MB（1 处，2.7%）** |
| 结构性搬运（有人读、但本可留在片上） | 主深度被 4 个 pass 各 LOAD 一次，约 33 MB | **MSAA 写回再读回，约 186 MB** |
| 疑似但要先验画面 | rp112（已证实带遮挡查询，作废） | rp40 屏幕/深度 LOAD，约 23 MB |
| 灰档层能安全拿到的 | 2.46 MB | 7.73 MB（M1），若 M3 验证通过再加约 23 MB |

**选择：阶段 3 主战场 = Megacity。** 理由：
1. 能安全拿到的确定收益是原神的 3 倍；
2. 浪费的类型正好是分块 GPU 的教科书问题（MSAA 出片），有明确的「该怎么改」可以给 AI 去做；
3. Megacity 是我们自己打包的白盒，可以拿「改 Unity 设置」的结果当对照，判断灰档改写离理想值还有多远。
4. 代价：它不是商业游戏。PUBG 接入后用同样流程补一份，若 PUBG 浪费更多就换过去。

## 4. 严格逐像素证明（`city_static` 上跑 storeop-check）

**待补**：真机任务 cb0e2a4cb462（规则 `fb2e9bc6efb185c5:1`，`city_static` 上 关/开 交替截图）已提交。节点 21:xx 起失联，结果还没取回，节点恢复后补到这里。

证据说明：`analysis_INVALID_no_template_hooks.md` 是作废的第一轮，留着给 §0 作对照，不要引用里面的数字。

## 5. 阶段 3 开关清单（Megacity）

| 排名 | 开关 | 类型 | 定位 | 预估收益 | 安全性 |
|---|---|---|---|---|---|
| 1 | rp32 解析目标写回 →DONT_CARE | 写回 storeOp | sig `fb2e9bc6efb185c5` 附件 **1** | 7.73 MB/帧 | 确定没人读，见 §4 |
| 2 | rp40 屏幕 loadOp LOAD→DONT_CARE | 读入 loadOp | sig `ae9f514c59165b6a` 附件 **1** | 7.7 MB/帧 | **疑似**，先画面比对（全屏盖满才安全） |
| 3 | rp40 深度 #201 storeOp/stencilStoreOp →DONT_CARE（及 loadOp） | 写回 / 读入 | sig `ae9f514c59165b6a` 附件 **0** | 最多 15.5 MB/帧 | **疑似**，同上 |
| 4 | MSAA 不出片（rp19/rp32/rp41 合并或片上解析） | 结构改写 | sig `c3a64d53ba8837bb` / `fb2e9bc6efb185c5` / `d88abfb3b0566231` | 最多约 186 MB/帧（模型上限） | 层里风险高。建议先用白盒设置做对照，量出真实收益，再决定值不值得在层里做 |
