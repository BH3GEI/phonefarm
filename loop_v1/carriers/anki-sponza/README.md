# loop_v1/carriers/anki-sponza — AnKi 3D Engine / Sponza 白盒载体

把 [AnKi 3D Engine](https://github.com/godlikepanos/anki-3d-engine) (BSD) 的 Sponza
场景做成 harness 可用的白盒测试载体：完整延迟渲染管线、现代 Vulkan、有源码、能改，
比 refbench 更像真游戏，又比原神可以动手术。长期目标是把 `game_opt_loop` 的后处理
算子 (Compute Shader) 挂进它的渲染管线尾部。

第三方源码克隆在 `/Users/mac/projects/thirdparty/anki-3d-engine`（**不进本仓库**）。
本目录只放我们写的胶水：构建脚本、启动契约、patch、说明。

---

## 当前状态 —— 读之前先看这里

**没有跑起来。** 到目前为止本目录里的东西全部是静态分析 + 脚本，**唯一实测通过的
是 `shadercompiler_shim.py` 的参数解析与缓存未命中硬失败**。具体：

| 项 | 状态 |
|---|---|
| 源码克隆 (浅克隆, commit `7ea7005`) | ✅ 完成, 1.6 GB |
| 阻塞点定位 (macOS 没法编着色器) | ✅ 查清, 见下 §1 |
| 绕过方案设计 (容器里编一次 + 缓存 + shim) | ✅ 设计完, 脚本写好 |
| 着色器 bin 缓存实际产出 | ❌ **磁盘不够, 中止** |
| APK 构建 | ❌ 未做 (依赖上一步) |
| 装机 / 确定性跑法验证 | ❌ 未做 |
| 3 轮基线 / 帧时 p95 离散度 | ❌ 未做 |
| 挂 Compute 算子要改哪几处 | ✅ 查清并写完 → [`DESIGN_COMPUTE_HOOK.md`](DESIGN_COMPUTE_HOOK.md) |

`contract/launch.json` 和 `scripts/run_anki_sponza.sh` 里带 `unverified` /
`PROPOSED_NOT_IMPLEMENTED` 标记的字段，是照 refbench 契约冻的形状，**不是实测事实**，
第一次真跑起来之前不要当结论引用。

### 为什么中止：磁盘

本机硬盘与其它 agent 共用，约定**剩余空间任何时候不得低于 8 GiB**。实际观测：

```
克隆前          15 GiB
浅克隆后        13 GiB   (AnKi 占 1.6 GB)
开始装容器工具链 9.3 GiB
中止时           8.5 GiB  ← 编译阶段还没开始, 必然破线, 主动杀掉
杀掉并清理后     8.8 GiB
之后(非本任务)   5.2 GiB  ← 已被别的 agent 跌破下限
```

着色器编译那一步还需要约 1–2 GB（容器工具链 + 构建目录），在 8 GiB 下限下放不进去。

**一个要交代的代价**：容器实验往 colima 虚拟磁盘镜像里写了约 1.5 GB（apt 装
build-essential），而 **colima 的镜像删文件不会还给宿主**，所以这 1.5 GB 不会自动
回来。要收回得 `colima delete` 重建虚拟机（会影响其它在用 docker 的 agent，没动）。

---

## 1. 核心阻塞点：macOS 编不了 AnKi 的着色器

AnKi 的 Android 构建流程要求**先有一个宿主平台的 `ShaderCompiler`**，用来把 63 个
`AnKi/Shaders/*.ankiprog`（HLSL）编成 `*.ankiprogbin`。官方 README 也是这么说的
（"Android builds requires the ShaderCompiler to compile the shaders for Android"）。

这个工具在运行期 `dlopen` DXC：

```cpp
// AnKi/ShaderCompiler/Dxc.cpp:75
g_dxcLib = dlopen(ANKI_SOURCE_DIRECTORY "/ThirdParty/Dxc/Lib/LinuxX64/libdxcompiler.so", RTLD_LAZY);
```

而仓库只随包了三份 DXC：`Lib/WinX64`、`Lib/WinArm64`、`Lib/LinuxX64`。
**没有 macOS 版**，上游 DirectXShaderCompiler 也不发布 macOS 二进制，brew 里没有
（`brew search dxc / directx / vulkan-sdk` 全空）。自己从源码编 DXC 是 LLVM 量级，
十几 GB，这台机器的磁盘预算下根本不用考虑。

另外 AnKi 的 CMake 本身也没有可用的 Darwin 路径 —— 它认的是
`CMAKE_SYSTEM_NAME MATCHES ".*MacOS.*"`，而 macOS 上 CMake 报的是 `Darwin`，
所以会直接 `FATAL_ERROR "Unknown apple"`（`CMakeLists.txt:64-70`）。

### 好消息：这一步是可解耦的，而且只需要做一次

`AnKi/Shaders/CMakeLists.txt` 只是把 `ANKI_OVERRIDE_SHADER_COMPILER` 当成一个
**命令行工具**来调：

```
<compiler> -o <out.ankiprogbin> -j <N> -I <ankiroot> -DANKI_PLATFORM_MOBILE=1 -spirv <in.ankiprog>
```

产物 `*.ankiprogbin` 就是普通 asset。所以：

1. 在**任意 x86_64 Linux** 上把 ShaderCompiler 编出来、跑一次，产出 63 个 bin（`build_shadercompiler.sh`）
2. 之后 macOS 侧的 Android 构建把 `ANKI_OVERRIDE_SHADER_COMPILER` 指向
   `shadercompiler_shim.py`，它只负责从缓存里按名字拷一份出来
3. 于是 macOS 侧只剩**纯 NDK 交叉编译**，完全不碰 DXC

Android 交叉编译本身在 macOS 上没有问题：目标是 Android 时 CMake 走的是
`ANDROID` 分支而不是 `APPLE` 分支，上面那个 `Unknown apple` 不会触发。

`linux/amd64` 容器可用性**已实测**（`docker run --platform linux/amd64` 正常出
`x86_64`），所以这条路是通的，纯粹是磁盘没让它跑完。

---

## 2. 怎么接着做

```bash
# 前提: 剩余磁盘 ≥ 10 GiB (脚本自带 8 GiB 下限守卫, 破线自己退出码 9)

# ① 一次性: 容器里编 ShaderCompiler 并产出 63 个 ankiprogbin 缓存  (~1-2 GB, 模拟执行较慢)
bash scripts/build_shadercompiler.sh
#    产物: /Users/mac/projects/thirdparty/anki-shaderbins/*.ankiprogbin + MANIFEST.txt
#    跑完可以 rm -rf /Users/mac/projects/thirdparty/anki-build-linux 回收构建目录

# ② macOS 侧出 arm64 APK (不需要 DXC)
brew install gradle          # 本机还没有
bash scripts/build_apk.sh

# ③ 装机
adb -s 91253241019A install -r <上一步打印的 APK 路径>
```

本机工具链现状（`build_apk.sh` 已按这些事实写好，会就地改模板）：

| | 上游模板要求 | 本机实际 | 处理 |
|---|---|---|---|
| NDK | 26.1.10909125 | 28.2.13676358 | 脚本 sed 改 `ndkVersion` |
| compileSdk / targetSdk | 32 | 只装了 android-35 | 脚本 sed 改成 35 |
| CMake | 3.22.1 | SDK 里有 3.22.1 ✅ | 不用动 |
| JDK | — | brew `openjdk@21`（`java` 不在 PATH） | 脚本设 `JAVA_HOME` |
| gradle | — | **没装** | 需 `brew install gradle` |

---

## 3. 确定性跑法（契约见 `contract/launch.json`）

形状照抄 refbench：`am start` 传 intent 字符串 extras 进 → 进渲染循环前落
`anki_sponza_started` 标记文件 → 渲染满固定帧数自退 → 写 `anki_sponza_out.json`。
harness 用 `pidof` 存活轮询判结束，**不依赖 logcat**（这台测试机上 logcat 会整个哑掉）。

落地时要动的地方（patch 形式保存在 `patches/`，**尚未编译验证**）：

- **相机固定路径**：改 `Samples/Sponza/Main.cpp` 的 `userMainLoop()`，相机姿态取成
  `frame_index` 的纯函数，不用 `elapsedTime`（墙钟会让每轮轨迹不一样），并且不吃输入。
  同理要把那段骑士动画（`Main.cpp:30-38`）关掉或改成按帧推进。
- **固定帧数自退**：`userMainLoop(Bool& quit, ...)` 里计数到 `frames` 就 `quit = true`，
  是引擎自带的干净退出口。
- **intent extras**：Android 侧走 NativeActivity，`argc/argv` 是空的，但
  `g_androidApp->activity`（native_app_glue 的 `android_app*`）在
  `AnKi/Window/NativeWindowAndroid.cpp` 里是可见的，所以能用 JNI 取
  `getIntent().getStringExtra()`。取到之后直接灌进引擎自带的 cvar 系统
  （`CVarSet::setMultiple`，`AnKi/Util/CVarSet.h:292`）。
- **驱动线程改名**：和 refbench 一样，要把进程内 Adreno 驱动提交线程改名成
  `AnkiDrv`，否则 raw ftrace kgsl 没法按 comm 过滤。
  ⚠ AnKi 每帧提交次数**未实测**，不像 refbench 那样保证恰好 1 次 ——
  解析侧不能假设 `submits_per_frame == 1`。

---

## 4. 挂外部 Compute 算子

完整答案在 **[`DESIGN_COMPUTE_HOOK.md`](DESIGN_COMPUTE_HOOK.md)**。一句话版本：

AnKi 里**已经存在** refbench `sr_pipeline` 的那个形状 —— cvar `Render.RenderScaling < 1.0`
时 post-process 分辨率低于 swapchain，引擎自动插入一个 `"Final Blit"` pass
（`Renderer.cpp:952-978`）用三线性采样放大。那就是现成的 A 臂，一行代码不用改。
B 臂就是把这个 blit 换成一次 compute dispatch，shader 从设备侧 `candidate.spv` 动态装。

改动量：**新增 1 个 pass 文件（~200 行）+ 3 处共约 13 行接线**，渲染架构不用动，
`AnKi/Renderer/CMakeLists.txt` 是 `file(GLOB)` 所以新文件自动进构建。

动态装 SPIR-V 这块比预想的容易：`GrManager::newShader/newShaderProgram` 是公开的，
唯一必填的 `ShaderReflection` 可以用引擎自带的 `doReflectionSpirv()`
（`AnKi/ShaderCompiler/Spirv.h:16`）从 SPIR-V 直接反推。而且**这条链路不需要 DXC** ——
`AnKiShaderCompiler` 压根没链接 DXC（CMake 里那行是注释掉的，DXC 是运行期 dlopen 的），
`doReflectionSpirv` 走的是 SPIRV-Cross / SPIRV-Tools，所以在手机上可用。

两个真实的坑（swapchain 能不能当 UAV 写、UI 在 compute pass 里画不了）在设计文档
§2 里写了规避办法。

---

## 目录

```
DESIGN_COMPUTE_HOOK.md          挂 Compute 算子要改哪几处 (本次主要产出)
contract/launch.json            启动契约 (照 refbench 冻的形状, 未实测)
scripts/build_shadercompiler.sh 一次性: 容器里编 ShaderCompiler + 产出 bin 缓存
scripts/shadercompiler_shim.py  假扮宿主 ShaderCompiler, 从缓存发货 (参数解析已实测)
scripts/build_apk.sh            macOS 侧出 arm64 APK
scripts/run_anki_sponza.sh      loop_v1 驱动面, 形状照抄 refbench/run_refbench.sh (未实测)
patches/                        对第三方源码的改动 (未编译验证)
evidence/                       实测证据 (目前为空 —— 还没跑起来)
```
