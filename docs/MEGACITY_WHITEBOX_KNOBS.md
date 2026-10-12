# Megacity 白盒旋钮：可行性与步骤（阶段 3 候选）

> 2026-10-11 · frame agent · 起因：[Megacity 审计](MEGACITY_FRAME_AUDIT.md) 发现每帧约 186 MB（模型值）的 4×MSAA 写回再读回。灰档层改不动，要用「改引擎设置重新出包」量真实收益。

## 结论

**能做，而且一次出包就能把所有旋钮变成启动参数。** 出包机 magicbook 在线、Unity 和工程都在、许可证已激活，可以通过 ssh 零人工出包。harness 已经在运行时用反射改 URP 的 `renderScale`（`--es resscale`），同样的办法能覆盖下面的大多数设置。所以只需要**改一次 harness、出一次包**，之后每个旋钮都是 intent extra，不用再按旋钮出包。**唯一的瓶颈是把 214 MB 的 APK 送到手机节点**（见 §4）。

## 1. 现状（只读核实）

| 项 | 值 | 出处 |
|---|---|---|
| 出包机 | magicbook（办公室 Windows，`ssh magicbook`），Unity 6000.1.0f1 已装，工程 `C:\thirdparty\megacity-metro` 在，Library 缓存热，C: 剩 35.7 GB，Unity 当前没开 | 2026-10-11 22:5x ssh 只读核实 |
| 出包脚本 | `loop_v1/carriers/megacity/build_megacity.ps1`（批处理，零交互，构建后自检 arm64/Vulkan/IL2CPP） | 本仓库 |
| 上次的包 | `C:\megacity-glue\out\megacity.apk`，**214 MB**，2026-09-23 | magicbook |
| 手机上跑的画质档 | `High`（`megacity_out.json` 的 `graphics.quality_level`） | loop_v1/runs/mc_base1 |
| 上游工程 | Unity-Technologies/megacity-metro @ `07652ee`，URP 17.1.0，Unity 6（Render Graph 默认开） | 本机只读克隆（只拉了设置文件） |

`High` 档的 URP 设置（`Assets/Settings/Graphics/URP/2_High_*.asset`）：

| 设置 | 值 | 和审计结果的关系 |
|---|---|---|
| `m_MSAA` | **4** | 主场景 rp19 的 4×MSAA 深度+颜色就是它 |
| `m_SupportsHDR` | 1 | 颜色是 R11G11B10F（每像素 4 字节，和 RGBA8 一样大，关 HDR 省不了带宽） |
| `m_StoreActionsOptimization` | 0（Auto） | 可选 Discard / Store。Auto 下 URP 自己判断要不要写回 |
| `m_MainLightShadowmapResolution` | **4096**，1 级级联，软阴影高质量 | 阴影 pass rp13 占打点时间 41% |
| 渲染器 `m_RenderingMode` | 2（Forward+） | |
| `m_DepthPrimingMode` / `m_CopyDepthMode` | 2（Forced）/ 2（ForcePrepass） | 1 倍深度预渲染（rp17，占 13%）的来源之一 |
| 渲染器特性 | **ScreenSpaceAmbientOcclusion**、**VolumetricFogRendererFeature** | SSAO 需要深度+法线预渲染（rp17）。帧中途把 MSAA 解析出来（rp32）的，**推测**是雾或 SSAO 这类中途要采样场景颜色/深度的特性，**未证实** |
| `m_UseNativeRenderPass` | 0 | Render Graph 模式下不看这个（兼容模式才用），**不确定**是否影响本包 |

## 2. 白盒旋钮候选

全部做成 harness 的启动参数：harness 在运行时通过反射改当前 URP asset 或渲染器特性，并把**实际生效值**写回 `megacity_out.json`。写不进就如实记 `none`，和现有 `resscale` 同一个规矩，不静默降级。

| # | 参数 | 改什么 | 预期效果 | 画质代价 | 运行时可改？ |
|---|---|---|---|---|---|
| W1 | `msaa` = 1/2/4 | `UniversalRenderPipelineAsset.msaaSampleCount` | 关掉后 186 MB 的 MSAA 搬运整块消失。这是**上限对照**，告诉我们这块浪费值多少 GPU 时间和功耗 | 边缘锯齿（有画质代价，只当量尺，不当最终方案） | 是（公开属性） |
| W2 | `store_opt` = auto/discard/store | `storeActionsOptimization` | Discard 让 URP 尽量不写回。MSAA 若之后还要被读，会被迫保留，所以**预期收益有限**，正好用来验证「写回是被谁逼出来的」 | 无（URP 保证正确性） | **不确定**（属性可能只读，要反射私有字段） |
| W3 | `feat_off` = fog,ssao,… | 对渲染器特性 `SetActive(false)` | 逐个关，看 rp32 那次中途解析会不会消失，**找出是谁把主 pass 拆开的** | 关雾/SSAO 有画质代价，同样只当诊断 | 是（`ScriptableRendererFeature.SetActive` 是公开 API） |
| W4 | `fog_event` = before_transparents/after_transparents | 雾特性的注入时机（如果 W3 证实是它） | 拆分点挪到透明物之后，rp19 和 rp41 就能合成一个原生 pass，MSAA 在片上解析，**不出片**。这是「几乎不损画质又去掉搬运」的那个真方案 | 透明物上的雾效可能有差异，要画面比对 | 视特性实现而定，**不确定** |
| W5 | `ssao_src` = depthnormals/depth、`ssao_after_opaque` | SSAO 的输入源和时机 | 改成只用深度、放在不透明物之后，可以省掉深度+法线预渲染 rp17（占 13%） | SSAO 质量略降 | 是（设置对象字段，反射） |
| W6 | `shadowres` = 1024/2048/4096 | `mainLightShadowmapResolution` | 阴影 pass 占 41%。降到 2048，阴影图从 33.5 MB 降到 8.4 MB | 阴影变糊（明确的画质取舍） | 是 |
| W7 | `depth_priming` = off/auto/forced | 渲染器的 `depthPrimingMode` | 看 rp17 有没有一部分是强制深度预渲染带来的 | 无 | **不确定**（渲染器数据字段，反射） |

**排序（按「先找原因再给方案」）：** W1 和 W3 先做，用来量浪费有多大、找出是谁拆的 pass；然后做 W4，这是真方案；W5、W6 是画质取舍，交给阶段 3 的 AI 去搜；W2、W7 是验证项。

## 3. 步骤

1. **改 harness**（本仓库 `loop_v1/carriers/megacity/unity/MegacityRefbenchHarness.cs`，由 frame 做）：解析上表的 extras，静置阶段结束、预热开始前一次性生效，再把实际值写回 json 的 `graphics.*`。契约 `contract/launch.json` 同步加字段。离线能做的是编译检查和契约单测，Unity 编译只能在 magicbook 上做。
2. **出包**（magicbook，通过 ssh）：`setup_on_magicbook.ps1` 同步胶水代码，然后 `build_megacity.ps1`。上次的全量构建记录没写耗时，Library 缓存是热的，**预计几十分钟，不确定**。构建后会自检 arm64/Vulkan/IL2CPP。
3. **送包到手机节点**：见 §4，这是唯一需要 owner/lead 拍板的环节。
4. **验证**：节点上 `adb install -r`，然后对每个旋钮档跑 `frame-audit capture --pkg com.unity.megacity.metro --extras "--es scene city_static … --es msaa 1"`，看结构是否按预期变了（例如 MSAA 写回消失、rp32 消失）。结构对了，再交给 loop_v1 / 判官按 GPU 忙碌时间和功耗做 A/B。
5. **不碰**：上游既有文件一个不改，只改我们注入的 3 个 .cs，和现在的做法一致。

## 4. 卡点：214 MB 的 APK 怎么到手机

开发机的上行只有 10–20 KB/s（lead 已说明），走开发机中转要好几个小时，不可行。可选方案（需要 owner/lead 拍板，frame 不擅自做）：

- **A. 节点直接下载**：magicbook 出包后放到节点能访问的地址（例如 office.liyao.space 上临时开一个只读下载，或者对象存储），节点用 onboard 在用的 `fetch_apk` 拉。
- **B. 人工拷贝**：U 盘或局域网拷到手机节点那台 mini。

## 5. 不确定 / 风险

- rp32 那次中途解析到底是谁引起的：只是推测（雾或 SSAO），W3 会给出答案。
- W2、W4、W7 能否在运行时改：取决于 URP 17 的字段可见性，做 harness 时再核实。改不动就降级成「出包时改 asset」，变成每档一个包。
- Render Graph 是否真的开着：GraphicsSettings 里没有对应条目，按 Unity 6 默认推断是开的，**未核实**。
