# 问题 1 实测结果: 能给非 debuggable 第三方应用挂 Vulkan layer —— 成立

设备 NX809J / Android 16 / SDK 36 / user 版 / Magisk 30.7 root / Adreno 840
目标 io.github.hgamey.refbench (release 装机, flags 里**没有** DEBUGGABLE), 2026-09-23

## 判据与证据

层写出的 marker (refbench 外部 files 目录):

    {"knob":"gray_readonly_probe","layer_loaded":true,"pkg":"io.github.hgamey.refbench",
     "effective":[],"failed":[],"unavailable_reason":null,
     "readonly_stats":{"frames":3600,"render_pass_begins":32400}}

加载器与层自己的 logcat (logcat_refbench_PASS.filtered.txt):

    D vulkan  : added global layer 'VK_LAYER_refknobs_readonly' from library '...'
    I vulkan  : Loaded layer VK_LAYER_refknobs_readonly
    I refknobs: rk_CreateInstance entered (pkg=io.github.hgamey.refbench)
    I refknobs: next vkCreateInstance -> 0

## 层是透明的 (没把应用弄坏) —— 有对照

| | swapchain | frames_submitted | clean_exit |
|---|---|---|---|
| 无层对照 | 2688x1216 MAILBOX | 3600 | true |
| 挂层 | 2688x1216 MAILBOX | 3600 | true |

层自己数到的 frames=3600 与 refbench 自报的 frames_submitted=3600 **逐帧相等**,
render_pass_begins=32400 = 3600 x 9 pass/帧。这是一次独立交叉校验:
层的计数不是自说自话, 与被测应用自己的账对得上 (判据 4 的基础)。

## 上机才暴露的四个真问题 (立项时的代码/文档全错)

1. **.so 落点**: 早先脚本把 .so 放 /data/data/<pkg>/, 加载器根本不搜那里。
   正确落点是目标应用的 nativeLibraryDir。
2. **动态链 STL**: `dlopen failed: library "libc++_shared.so" not found`。
   宿主应用 lib 目录里没有 libc++_shared.so → 必须 -static-libstdc++。
3. **枚举符号**: `missing some instance enumeration functions`。
   Android 的加载器用 **dlsym 发现层, 完全不读 JSON manifest** (那是桌面 loader 的约定),
   层必须导出 vkEnumerateInstance{Layer,Extension}Properties。
4. **sType 写错**: VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO 是**枚举量不是宏**,
   早先用 `#ifndef` 兜底 define 成 1000000000 —— `#ifndef` 对枚举恒为真, 于是无条件
   覆盖成错值, 层挂上了但 CreateInstance 永远找不到 layer link (实测 chain 里是 sType=47)。
   正确值是核心枚举的 47 / 48, 已用 static_assert 钉死。

第 5 个问题是"只读层把应用弄坏":早先版本的 vkEnumerateDeviceExtensionProperties
无条件回答"0 个扩展", 于是 refbench 找不到 VK_KHR_swapchain, swapchain 建成 0x0,
frames_submitted=0 而**不报任何错**。只读 = 不改渲染行为, 不等于可以乱答枚举。
现在 pLayerName 非本层时一律转发下层。

## 激活机制 (与立项设想不同)

见 matrix.txt。一句话: `debug.vulkan.layers` 属性是充分必要条件;
README 里那套 `settings put global gpu_debug_*` 在本机**完全不生效**。
