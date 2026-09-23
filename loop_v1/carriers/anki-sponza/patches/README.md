# patches/ — 对 AnKi 源码的改动

第三方源码不进我们的仓库，所以对它的改动以 patch 形式存在这里。

基线：`godlikepanos/anki-3d-engine` commit `7ea70050a4de716c3e1e36e130d79e57bcb00c53`
（2026-08-31，浅克隆在 `/Users/mac/projects/thirdparty/anki-3d-engine`）。

```bash
cd /Users/mac/projects/thirdparty/anki-3d-engine
git apply /Users/mac/projects/phonefarm/loop_v1/carriers/anki-sponza/patches/0001-sponza-deterministic-run.patch
git apply /Users/mac/projects/phonefarm/loop_v1/carriers/anki-sponza/patches/0002-ndk28-alooper-pollonce.patch
```

| patch | 内容 | 状态 |
|---|---|---|
| `0001-sponza-deterministic-run.patch` | Sponza 改成确定性负载：相机固定轨迹（纯 `frame_index` 函数）、渲满固定帧数自退、started 标记、退出写 ground-truth JSON | **编译通过**，产物 `libSponza.so` 里验到 `anki_sponza_started` / `anki_sponza_out.json` 两个标记字符串；**未上机** |
| `0002-ndk28-alooper-pollonce.patch` | NDK 28 移除了 `ALooper_pollAll`，3 处调用换成签名相同的 `ALooper_pollOnce` | **编译通过**（不打这个 patch，NDK 28 下根本出不了包） |

## 0001 没做完的部分

- **intent extras 没接。** Android 走 NativeActivity，`argc/argv` 是空的。要用 JNI 从
  `g_androidApp->activity`（`AnKi/Window/NativeWindowAndroid.cpp` 里可见）取
  `getIntent().getStringExtra()`，再灌进 `CVarSet::setMultiple`（`AnKi/Util/CVarSet.h:292`）。
  在那之前 `frames` / `run_id` / `campath` 用的是文件里的默认值，
  `contract/launch.json` 的 extras 还不生效。
- **驱动线程改名没做。** 要把进程内 Adreno 提交线程改名成 `AnkiDrv`，
  否则 raw ftrace kgsl 没法按 comm 过滤（参考 refbench DESIGN.md §2 的做法）。
- **骑士动画。** patch 里因为绕过了 `SampleApp::userMainLoop` 而顺带不再启动它，
  但没有显式关掉场景里其它可能带时间的东西 —— 真跑起来后要核一遍逐帧是否真的可重复。
- **整体没上过机。** 编得出来不等于跑得起来：相机轨迹、自退、标记/JSON 落盘路径
  都还没在设备上验过。
- **`campath=static`** 契约里写了，代码里还只有 `orbit`。

## 还没写的 patch

`0002` 挂外部 Compute 算子 —— 设计在 `../DESIGN_COMPUTE_HOOK.md`，代码未落地。
