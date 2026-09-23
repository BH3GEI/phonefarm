# loop_v1/carriers/anki-sponza — AnKi 3D Engine / Sponza 白盒载体

把 [AnKi 3D Engine](https://github.com/godlikepanos/anki-3d-engine) (BSD) 的 Sponza
场景做成 harness 可用的白盒测试载体：完整延迟渲染管线、现代 Vulkan、有源码、能改，
比 refbench 更像真游戏，又比原神可以动手术。长期目标是把 `game_opt_loop` 的后处理
算子 (Compute Shader) 挂进它的渲染管线尾部。

第三方源码克隆在 `/Users/mac/projects/thirdparty/anki-3d-engine`（**不进本仓库**）。
本目录只放我们写的胶水：构建脚本、启动契约、patch、说明。

---

## 当前状态

| 项 | 状态 |
|---|---|
| 源码浅克隆 (commit `7ea7005`) | ✅ 1.6 GB |
| 宿主着色器编译链路 (容器) | ✅ 跑通，63/63 `.ankiprogbin` |
| arm64 APK 构建 | ✅ **出包成功**，133 MB |
| 确定性跑法 patch 进包 | ✅ 编译通过，产物内已验到标记字符串 |
| 装机 / 上机跑通 | ⏳ 手机被别的 agent 占着，没轮到 |
| 3 轮基线 / 帧时 p95 离散度 | ⏳ 同上 |
| 挂 Compute 算子要改哪几处 | ✅ [`DESIGN_COMPUTE_HOOK.md`](DESIGN_COMPUTE_HOOK.md) |

**没上过机**：APK 从没在设备上启动过。所以下面这些仍然**不是实测事实** ——
相机轨迹是否真的逐帧可重复、`frames` 到了会不会干净自退、标记文件与 JSON 落盘路径
对不对、每帧提交几次。`contract/launch.json` 里带 `unverified` 的字段同理。
第一次上机要先验这些，再谈基线。

已知没接的部分：intent extras 还没接（Android 走 NativeActivity，`argc/argv` 是空的，
要走 JNI），所以 `frames`/`run_id`/`campath` 目前用的是代码里的默认值；
驱动线程改名 `AnkiDrv` 也还没做。详见 [`patches/README.md`](patches/README.md)。

---

## 1. 为什么要绕一圈：macOS 编不了 AnKi 的着色器

AnKi 的 Android 构建要求**先有一个宿主平台的 `ShaderCompiler`**，把 63 个
`AnKi/Shaders/*.ankiprog`（HLSL）编成 `*.ankiprogbin`。它在运行期 `dlopen` DXC：

```cpp
// AnKi/ShaderCompiler/Dxc.cpp:75
g_dxcLib = dlopen(ANKI_SOURCE_DIRECTORY "/ThirdParty/Dxc/Lib/LinuxX64/libdxcompiler.so", RTLD_LAZY);
```

而仓库只随包 `WinX64` / `WinArm64` / `LinuxX64` 三份 DXC，**没有 macOS 版**，上游
DXC 也不发布 macOS 二进制，brew 里没有。自己编 DXC 是 LLVM 量级，不现实。
（AnKi 的 CMake 本身也没有可用的 Darwin 路径：它认
`CMAKE_SYSTEM_NAME MATCHES ".*MacOS.*"`，而 macOS 上 CMake 报 `Darwin`，
所以宿主构建会直接 `FATAL_ERROR "Unknown apple"`，`CMakeLists.txt:64-70`。）

**但这一步可解耦，而且只做一次。** `AnKi/Shaders/CMakeLists.txt` 只是把
`ANKI_OVERRIDE_SHADER_COMPILER` 当一个命令行工具调：

```
<compiler> -o <out.ankiprogbin> -j <N> -I <ankiroot> -DANKI_PLATFORM_MOBILE=1 -spirv <in.ankiprog>
```

产物就是普通 asset。于是：

1. `build_shadercompiler.sh` 在 **linux/amd64 容器**里编出 ShaderCompiler 并跑一遍，
   产出 63 个 bin 缓存（约 9 MB）
2. `build_apk.sh` 把 `ANKI_OVERRIDE_SHADER_COMPILER` 指向 `shadercompiler_shim.py`，
   它只负责从缓存按名字拷一份到 `-o` 指定位置
3. macOS 侧于是只剩**纯 NDK 交叉编译**，完全不碰 DXC

Android 交叉编译在 macOS 上没问题：目标是 Android 时 CMake 走 `ANDROID` 分支，
不触发 `Unknown apple`。

---

## 2. 一次性跑通

```bash
# ① 容器里编 ShaderCompiler 并产出 63 个 ankiprogbin 缓存
bash scripts/build_shadercompiler.sh
#    产物: /Users/mac/projects/thirdparty/anki-shaderbins/*.ankiprogbin + MANIFEST.txt
#    跑完构建目录可删: rm -rf /Users/mac/projects/thirdparty/anki-build-linux

# ② 打 patch (确定性跑法 + NDK 28 兼容)
cd /Users/mac/projects/thirdparty/anki-3d-engine
git apply <本目录>/patches/0001-sponza-deterministic-run.patch
git apply <本目录>/patches/0002-ndk28-alooper-pollonce.patch

# ③ macOS 侧出 arm64 APK (不需要 DXC)
bash scripts/build_apk.sh

# ④ 装机
adb -s 91253241019A install -r \
  /Users/mac/projects/thirdparty/anki-3d-engine/AndroidProject_Sponza/app/build/outputs/apk/release/app-release.apk
```

脚本自带 8 GiB 磁盘下限守卫（破线自己 `exit 9`）。整条链路占用：源码 1.6 GB +
容器构建目录约 22 MB + 着色器缓存 9 MB + gradle/CMake 产物约 3 GB。

### 踩过的坑（都已写进脚本，列在这里是为了下次别再查一遍）

| 现象 | 真因 | 处理 |
|---|---|---|
| `dxcompiler.dll/libdxcompiler.so missing or wrong architecture` | **报错有误导性**，其实不是架构问题：随包的 `libdxcompiler.so` 要 `GLIBC_2.38`，而 `debian:bookworm` 只有 2.36 | 基础镜像换 `debian:trixie-slim`（glibc 2.41） |
| `cc1plus: internal compiler error: Segmentation fault` | qemu 模拟 x86_64 下 gcc 随机 ICE，和源码无关；同一文件重试往往就过 | `ninja -j 2` + 最多 10 轮增量重试（ninja 是增量的，每轮都推进） |
| SDL `could not find X11 or Wayland` | 只想编 ShaderCompiler，却把 SDL3 也配置了 | `-DANKI_HEADLESS=ON`（根 CMakeLists 里它会置 `SDL FALSE`，直接跳过 `add_subdirectory`） |
| `'ALooper_pollAll' is unavailable` ×3 | NDK 28 移除了它（AnKi 按 NDK 26 写的） | `patches/0002`，换成签名相同的 `ALooper_pollOnce`；调用处本来就是 `while(...>=0)` 排空循环，语义等价 |
| gradle 跑不动 | 模板自带 wrapper 是 Gradle 7.4.2 + AGP 7.1.3，跑不了 JDK 21/25 | 用 `./gradlew`（**不要**用系统 gradle 9.x）+ `openjdk@17` |
| 缺 `platforms;android-32` | 模板 `compileSdk 32`，本机只装了 android-35 | 宁可 `sdkmanager "platforms;android-32"` 去迁就模板，也不擅自改 `compileSdk` —— 模板那套是上游实际测过的组合 |

NDK 是唯一例外：模板写死 `26.1.10909125`，本机只有 `28.2.13676358`，装一个 26.1 要
另外约 2.5 GB，所以用 28.2 + `0002` patch 顶上。

---

## 3. 确定性跑法（契约见 `contract/launch.json`）

形状照抄 refbench：`am start` 传 intent 字符串 extras 进 → 进渲染循环前落
`anki_sponza_started` 标记文件 → 渲染满固定帧数自退 → 写 `anki_sponza_out.json`。
harness 用 `pidof` 存活轮询判结束，**不依赖 logcat**（这台测试机上 logcat 会整个哑掉）。

`patches/0001` 已经做了：相机取成 `frame_index` 的**纯函数**（不用 `elapsedTime` ——
墙钟会让每轮轨迹不同）、绕过 `SampleApp::userMainLoop` 的交互控制、渲满自退、
落 started 标记、退出写 JSON。**还没做**的见 `patches/README.md`。

---

## 4. 挂外部 Compute 算子

完整答案在 **[`DESIGN_COMPUTE_HOOK.md`](DESIGN_COMPUTE_HOOK.md)**。一句话版本：

AnKi 里**已经存在** refbench `sr_pipeline` 的那个形状 —— cvar `Render.RenderScaling < 1.0`
时 post-process 分辨率低于 swapchain，引擎自动插入一个 `"Final Blit"` pass
（`Renderer.cpp:952-978`）用三线性采样放大。那就是现成的 A 臂，一行代码不用改。
B 臂就是把这个 blit 换成一次 compute dispatch，shader 从设备侧 `candidate.spv` 动态装。

改动量：**新增 1 个 pass 文件（~200 行）+ 3 处共约 13 行接线**，渲染架构不用动
（`AnKi/Renderer/CMakeLists.txt` 是 `file(GLOB)`，新文件自动进构建）。

动态装 SPIR-V 不需要 DXC：`GrManager::newShader/newShaderProgram` 是公开的，
唯一必填的 `ShaderReflection` 可用引擎自带的 `doReflectionSpirv()`
（`ShaderCompiler/Spirv.h:16`）从 SPIR-V 反推；`AnKiShaderCompiler` 压根没链接 DXC
（CMake 里那行是注释掉的），该函数走 SPIRV-Tools，所以设备上可用。

---

## 目录

```
DESIGN_COMPUTE_HOOK.md          挂 Compute 算子要改哪几处
contract/launch.json            启动契约 (照 refbench 冻的形状, 未上机验证)
scripts/build_shadercompiler.sh 一次性: 容器里编 ShaderCompiler + 产出 bin 缓存
scripts/shadercompiler_shim.py  假扮宿主 ShaderCompiler, 从缓存发货
scripts/build_apk.sh            macOS 侧出 arm64 APK
scripts/run_anki_sponza.sh      loop_v1 驱动面, 形状照抄 refbench/run_refbench.sh (未上机)
patches/                        对第三方源码的改动 (0001 确定性跑法 / 0002 NDK28 兼容)
```
