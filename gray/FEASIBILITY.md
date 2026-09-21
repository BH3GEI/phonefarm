# 灰档可行性：三个必须先回答的问题

判据 3 允许以否定结论结案。**"验证不可行"是合格交付，"没去验"不是。**
每个问题的结论要带证据（命令 + 现象）。当前状态：机制与探针已就绪，**实测待设备空出**
（refbench 的 M0 判定电池正占用 NX809J；不与它抢设备，避免污染采集）。

## 问题 1 — 能给非 debuggable 应用挂 Vulkan layer 吗？

**机制**（Android GPU debug layers，root 下对任意应用生效，无需应用 debuggable）：

```
settings put global enable_gpu_debug_layers 1
settings put global gpu_debug_app <pkg>
settings put global gpu_debug_layers VK_LAYER_refknobs_readonly
settings put global gpu_debug_layer_app io.github.hgamey.knobslayer   # 提供 .so 的包
```

层 .so 需在加载器搜索得到的位置。两条路径，`enable_layer.sh` 都编码了：
1. **借宿一个自带 .so 的宿主包**（把 libVkLayer_refknobs.so 塞进一个我们自己的占位 apk 的
   jniLibs，用 `gpu_debug_layer_app` 指过去）——第三方游戏最稳；
2. **root 直接放进目标应用私有 lib 目录**——省一个 apk，但每个目标都要放一次。

**验证步骤**（设备空出后跑 `enable_layer.sh probe`）：
- 先对 **refbench**（自己的 Vulkan demo，release 非 debuggable）挂只读层 → 看应用外部
  files 目录是否出现 `knobs_layer_out.json` 且 `layer_loaded=true`。这单验"机制能挂上"。

**结论**：`PENDING`（机制脚本就绪，未上机）

## 问题 2 — 目标游戏反作弊会拦吗？【第一天就做，几分钟出结果】

**方法**：挂**只读、零改写**的层（本仓库的 `vk_layer_refknobs.cpp` 起步态就是纯只读），
对目标游戏设 `gpu_debug_app`，正常启动 → 看能否进大世界、有没有闪退/封号提示。

**为什么先做**：挂空层试探是唯一低风险的验证方式。改写逻辑一行都还没写就能拿到
"这条路走不走得通"的结论，比写完一堆拦截再撞墙划算得多。

**结论**：`PENDING`

## 问题 3 — 鸿蒙有等价机制吗？

待鸿蒙设备到位。OpenHarmony 的图形栈是否暴露等价的 layer 注入点未知；
不成立则灰档只在 Android 成立，鸿蒙退回黑档。

**结论**：`PENDING`（无设备）

---

## 就绪清单（本轮已备，未上机）

- `layer/vk_layer_refknobs.cpp` — 只读 pass-through + render pass 计数，写 JSON 到应用外部 files 目录
- `build/build_layer.sh` — NDK 直编 `libVkLayer_refknobs.so`（arm64）+ manifest
- `enable_layer.sh {probe|target|off}` — 挂/摘层，编码上面两条 .so 供给路径
- 首个实测目标顺序：refbench（问题 1 机制）→ 目标游戏空层（问题 2 反作弊）
