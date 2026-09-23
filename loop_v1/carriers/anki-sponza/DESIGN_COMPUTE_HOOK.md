# 在 AnKi 渲染管线尾部挂外部 Compute 算子 —— 要改哪几处

目标：复刻 refbench `sr_pipeline` 场景的 A/B 两臂契约，但载体换成真实游戏引擎的
完整延迟渲染管线。B 臂在管线尾部动态装入设备侧 `candidate.spv`，让 `game_opt_loop`
产出的候选算子可以直接投进来跑。

结论先说：**AnKi 的形状非常合适，不需要改渲染架构，四处改动 + 一个新 pass 文件。**
下面每一处都给了 commit `7ea7005` 下的确切锚点。

---

## 0. 为什么 AnKi 天然对得上 sr_pipeline

refbench `sr_pipeline` 的契约是：低分辨率场景 pass → (A) 送显 blit 直接采样低分辨率
RT，驱动双线性放大；(B) 尾部插入外部 compute 算子，低分辨率 RT → 送显分辨率 RT。

AnKi **已经有这个形状**，而且是生产代码路径，不是我们造的：

| refbench 概念 | AnKi 对应物 | 锚点 |
|---|---|---|
| 低分辨率 RT | `m_postProcessResolution` | `Renderer/Renderer.h:248` |
| 送显分辨率 | `m_swapchainResolution` | `Renderer/Renderer.h:249` |
| 分辨率比例旋钮 | cvar `Render.RenderScaling` | `Renderer/Renderer.h:23`，用在 `Renderer.cpp:177` |
| A 臂（驱动放大） | `"Final Blit"` pass，`m_blitGrProg` 全屏三角形 + `m_trilinearClamp` | `Renderer/Renderer.cpp:952-978` |
| B 臂（外部算子） | **要加的东西** | 下面 §2 |

关键点在 `Renderer.cpp:952`：

```cpp
const Bool bNeedsBlit = m_postProcessResolution != m_swapchainResolution;
```

也就是说，**只要把 cvar `Render.RenderScaling` 设成 < 1.0，A 臂就自动成立了**，
一行引擎代码都不用改。`FinalComposite` 会渲进一张 post-process 分辨率的 RT
(`FinalComposite.cpp:57-64` 的 `m_rtDesc`)，然后 `Renderer.cpp:955` 的 "Final Blit"
pass 用三线性采样把它拉到 swapchain。这就是干净的 A 臂对照。

B 臂要做的就是：把这个 blit pass 换成一次 compute dispatch，shader 来自外部文件。

---

## 1. 动态装入 SPIR-V —— 这块比想象的容易

AnKi 平时走 `loadShaderProgram("ShaderBinaries/X.ankiprogbin", ...)`
(见 `FinalComposite.cpp:44`)，那是资源系统的路径，吃的是 AnKi 自己的
`.ankiprogbin` 容器（带 mutator、反射信息）。外部 `candidate.spv` 是裸 SPIR-V，
走不了这条路。

但 Gr 层的底层入口是公开的，可以绕过资源系统直接建程序：

```
GrManager::newShader(const ShaderInitInfo&)          AnKi/Gr/GrManager.h:71
GrManager::newShaderProgram(const ShaderProgramInitInfo&)  AnKi/Gr/GrManager.h:72
```

`ShaderInitInfo` (`AnKi/Gr/Shader.h:14-43`) 要三样东西：

```cpp
ShaderType       m_shaderType;   // = ShaderType::kCompute
ConstWeakArray<U8> m_binary;     // = candidate.spv 的字节
ShaderReflection m_reflection;   // ← 唯一的坑
```

`m_reflection` 是必填的（`validate()` 会断言它）。**不用手写**——引擎里已经有从
SPIR-V 反推反射信息的函数：

```cpp
Error doReflectionSpirv(ConstWeakArray<U8> spirv, ShaderType, ShaderReflection&, ShaderCompilerString&);
```
`AnKi/ShaderCompiler/Spirv.h:16`

这个函数在设备上**是可用的**，两个理由：

1. `AnKiResource` 无条件链接 `AnKiShaderCompiler`（`AnKi/Resource/CMakeLists.txt:5`），
   所以它在 Android 包里。
2. `AnKiShaderCompiler` 本身**不链接 DXC** —— `AnKi/ShaderCompiler/CMakeLists.txt` 里
   那行 `set(libs ...dxcompiler)` 是注释掉的，DXC 是运行期 `dlopen` 的
   (`ShaderCompiler/Dxc.cpp:75`)，而且只在 HLSL→SPIR-V 那条路上才用。
   `doReflectionSpirv` 走的是 SPIRV-Cross / SPIRV-Tools，跟 DXC 无关。

所以装载逻辑大概是：

```cpp
// 读 candidate.spv
DynamicArray<U8> spv; readFileToArray(path, spv);

ShaderReflection refl; ShaderCompilerString err;
ANKI_CHECK(doReflectionSpirv(spv, ShaderType::kCompute, refl, err));

ShaderInitInfo sinf(ShaderType::kCompute, spv, "PostfxCandidate");
sinf.m_reflection = refl;
ShaderPtr sh = GrManager::getSingleton().newShader(sinf);

ShaderProgramInitInfo pinf("PostfxCandidate");
pinf.m_computeShader = sh.get();          // AnKi/Gr/ShaderProgram.h:40
m_postfxProg = GrManager::getSingleton().newShaderProgram(pinf);
```

**失败必须硬失败**（退出码 2、`clean_exit=false`），绝不静默退回 A 臂 ——
否则 A/B 两臂变成 A 和 A，而上层还以为在对照算子。这条纪律直接抄
refbench `contract/launch.json` 里 `postfx.shader` 的 `failure` 条款。

---

## 2. 新增 pass：`ExternalPostfx`

放在 `AnKi/Renderer/ExternalPostfx.{h,cpp}`，照抄 `FinalComposite` 的骨架
（`RendererObject` 子类 + `init()` + `populateRenderGraph()`）。

它要做的事，对照 `Renderer.cpp:955-978` 那个 "Final Blit" graphics pass，换成
non-graphics（compute）pass：

```cpp
NonGraphicsRenderPass& pass = ctx.m_renderGraphDescr.newNonGraphicsRenderPass("External Postfx");

pass.newTextureDependency(m_finalComposite->getRenderTarget(), TextureUsageBit::kSrvCompute);
pass.newTextureDependency(ctx.m_swapchainRenderTarget,         TextureUsageBit::kUavCompute);

pass.setWork([this](RenderPassWorkContext& rgraphCtx) {
    CommandBuffer& cmdb = *rgraphCtx.m_commandBuffer;
    cmdb.bindShaderProgram(m_postfxProg.get());
    rgraphCtx.bindSrv(0, 0, m_lowResRt);          // binding 0 = 只读低分辨率图
    rgraphCtx.bindUav(0, 0, m_swapchainRt);       // binding 1 = 只写送显分辨率图
    cmdb.dispatchCompute((w + 15) / 16, (h + 15) / 16, 1);   // 工作组 16x16
});
```

binding 布局（binding 0 只读低分辨率、binding 1 只写送显分辨率、工作组 16×16）
是 `game_opt_loop` 赛道的既有契约，和 refbench 一致，不要改。

### 两个真实的坑

**(a) swapchain 能不能当 UAV 写。** AnKi 目前把 swapchain 当 RTV 用
（`Renderer.cpp:957` 的 `setWritesToSwapchain()` + `kRtvDsvWrite`）。compute 直接写
presentable image 需要该 image 有 `VK_IMAGE_USAGE_STORAGE_BIT`，Adreno 上对
swapchain 不一定给。**稳妥做法**：算子写进一张我们自己建的送显分辨率 RT，再用
现成的 blit pass 1:1 拷到 swapchain。多一次全屏拷贝，但两臂都有这次拷贝的话
对照仍然干净；或者干脆让 A 臂也走「blit 到中间 RT 再拷」以保持对称。
第一版建议直接用中间 RT，别去碰 swapchain 的 usage flags。

**(b) UI。** "Final Blit" 里顺手画了 UI（`Renderer.cpp:974` 的 `m_uiStage->drawUi`）。
compute pass 画不了 UI。确定性跑法里 UI 本来就该关掉（见 §4），所以这条在我们的
用法下不是问题，但改代码时别把 UI 调用弄丢了导致别的模式崩。

---

## 3. 接线：Renderer.cpp

`Renderer.cpp:952` 那个 `if(bNeedsBlit)` 改成三岔：

```cpp
if(bNeedsBlit && m_externalPostfx->isEnabled())   // B 臂
{
    m_externalPostfx->populateRenderGraph();
}
else if(bNeedsBlit)                               // A 臂: 保持原样
{
    ... 原来的 Final Blit pass ...
}
```

外加：
- `Renderer.h` 里加成员 `ExternalPostfxPtr m_externalPostfx;`（照 `m_finalComposite` 的样子）
- `Renderer::initInternal()` 里 new + init 它
- `AnKi/Renderer/CMakeLists.txt` 是 `file(GLOB)`，新增 .cpp **不用改 CMake**

## 4. 开关从哪来

AnKi 有现成的 cvar 系统（`ANKI_CVAR` 宏，见 `FinalComposite.h:15-16`），而且
cvar 可以从命令行/配置覆盖。加两个：

```cpp
ANKI_CVAR(StringCVar,  Render, PostfxShader, "",  "外部 compute 算子 candidate.spv 的设备侧绝对路径")
ANKI_CVAR(NumericCVar<U8>, Render, Postfx,    0, 0, 1, "0=A臂(驱动放大) 1=B臂(外部算子)")
```

Android 侧怎么把 intent extras 喂进 cvar，见 `contract/launch.json` 和 README §启动契约。

---

## 改动清单（总结）

| # | 文件 | 改什么 | 量级 |
|---|---|---|---|
| 1 | `AnKi/Renderer/ExternalPostfx.{h,cpp}` | **新增**。compute pass + 动态 SPIR-V 装载 | ~200 行 |
| 2 | `AnKi/Renderer/Renderer.cpp:952` | `bNeedsBlit` 分支加 B 臂岔路 | ~6 行 |
| 3 | `AnKi/Renderer/Renderer.{h,cpp}` | 加成员 + init | ~5 行 |
| 4 | `AnKi/Renderer/ExternalPostfx.h` | 两个 cvar（开关 + spv 路径） | ~2 行 |

渲染架构、RenderGraph、资源系统**都不用动**。`AnKi/Renderer/CMakeLists.txt` 用
`file(GLOB)`，新文件自动进构建。

**不需要 DXC** —— 这条很重要：整条 B 臂链路（读 spv → `doReflectionSpirv` →
`newShader` → dispatch）只用 SPIRV-Tools，所以 candidate.spv 可以在设备上热装，
不需要把宿主着色器编译器带到手机上，也不受 §README 里那个 macOS/DXC 限制影响。
