# loop_v1/carriers/vulkan-samples — Khronos Vulkan-Samples 白盒载体

驱动 [KhronosGroup/Vulkan-Samples](https://github.com/KhronosGroup/Vulkan-Samples)(Arm 发起)
的 performance 样例跑两臂对照。靶子只提供负载与开关；采集、解析、归因、统计判定全在这边
(复用 `../tools/` 的纯函数链，回放纪律不变)，和 `../refbench/` 是同一套骨架。

**为什么用它**：它的 performance 样例把 `docs/MOBILE_GPU_OPT_ROUTES.md` §2 A 表里几条标着
「白盒」的路线做成了带开关的成对实现 —— render pass 的 loadOp/storeOp、subpass 合并
(G-buffer 留 tile memory)、MSAA 的 tile 内 resolve vs writeback。这正好是我们自己没有靶子、
只能靠黑盒猜的那几条。用它验的不是「这些优化有没有用」(上游已经论证过)，而是
**我们这套 harness 能不能把它们的差异测出来**。

```
bash build_vks.sh                                   # 浅克隆 + 裁剪 + 打补丁 + 出 arm64 APK
bash install_vks.sh                                 # 装 APK + 推场景/纹理/shader 到设备
bash run_vks.sh <label> <outdir> <sample> <config> <frames> [capdur]   # 单轮
bash run_ab.sh <sample> <config_A> <config_B> [rounds]                 # 交错 NvN + 统计判定
python3 vks_report.py --root <outdir> --sample S --config-a A --config-b B
```

---

## 1. 开关怎么钉死 —— `--config`(我们加的)

上游把每个 performance 样例的「好/坏」两档写在样例构造函数里，注册成
`vkb::Configuration` 的若干个 index。但上游只给了两条路去换档：**ImGui 手点**，或者
`--batch` 按秒轮转。两条都不适合做对照实验 —— 一个不可脚本化，一个在一次运行里把两档
混在一起。

所以我们加了一个插件 `sample_config`(`patches/sample_config/`)，提供
`--config <N>`：在 `Hook::OnAppStart` 上把样例自己注册的第 N 档取出来 `set()` 一次。
样例每帧重读这些值(改档时还会重建 render target)，所以 `prepare()` 之后应用即可。
运行日志里会留一行 `sample_config: applied configuration index N to "<sample>"`，
**这一行就是「这一轮确实跑在哪一档」的证据**，每轮都收进 `run.log`。

各样例的档位语义(逐行读自上游源码，冻结在 `vks_report.py` 的 `KNOBS` 里)：

| sample | config 0 | config 1 | 2 / 3 |
|---|---|---|---|
| `render_passes` | 颜色 loadOp=**LOAD** + 深度 storeOp=**STORE** | 颜色 loadOp=**CLEAR** + 深度 storeOp=**DONT_CARE** | — |
| `subpasses` | subpass 合并，G-buffer 留 tile memory | 两个独立 render pass，G-buffer 走 DRAM | 2=关 transient attachments，3=加大 G-buffer |
| `msaa` | 单 pass，tile 内 resolve | 开后处理→两 pass，writeback resolve | — |
| `afbc` | 交换链额外带 STORAGE usage(压制帧缓冲压缩) | 只带 COLOR_ATTACHMENT(允许压缩) | — |

`afbc` 是 Arm AFBC 的样例；在 Adreno 上对应的是 UBWC，语义要靠实测说话，
所以 `vks_report.py` 对它不预设方向。

## 2. 启动契约

```
am start -n com.khronos.vulkan_samples/.SampleLauncherActivity --es cmd \
  "sample <SAMPLE> --config <N> --stop-after-frame <FRAMES> --hideui --force-close \
   --vsync OFF --log-fps --log-file <externalDataPath>/run.log"
```

- `--stop-after-frame` 跑满固定帧数自退，`--force-close` 兜底；
  收尾会往日志写 `Total device memory leaked: ...`，**这一行是「干净退出」的判据**。
- `--vsync OFF` 是必须的：面板 120Hz 会把帧时间钉死在 8.3ms，开关差异就只剩 GPU 忙碌
  与带宽两项，`frame_p95` 这个主指标直接失去分辨力。
- 资产**不在 APK 里**：上游 Android 端从 `ANativeActivity.externalDataPath` 读
  (`components/android/src/context.cpp`)，所以 `scenes/ textures/ fonts/ shaders/`
  由 `install_vks.sh` 用 adb 推过去。**`shaders/` 也必须推**，样例运行时从那里读 `.spv`。
- trace 过滤**不写死线程名**：Adreno 驱动在应用进程内部线程发 kgsl 提交，而那个线程的
  名字形如 `binder:<pid>_5`(每轮都不一样)。`pick_comm.py` 采完当场按
  `adreno_cmdbatch_submitted` 的分布认第一名，占比低于 80% 直接判无效
  (说明这轮提交是多线程发的，对照口径不成立)。

## 3. 三个必须知道的设备坑(都是实测踩出来的，2026-09-23 · NX809J)

这三条不是保险措施，是没有它们就会拿到**看起来正常但完全是假的**数据。

**(a) 日志默认不落盘。** spdlog 的 file sink 带缓冲，运行期日志会一直停在 4096 字节，
外部脚本读不到任何进度，只能等进程退出后一次性看到全部。这台设备的 logcat 又是哑的
(refbench 载体也记了这一条)，进度信号只能走文件系统。
→ 补丁 `file_logger` 加 `flush_on(spdlog::level::info)`，见 `build_vks.sh` 第 3c 步。

**(b) 窗口没被合成时，样例会「空跑」。** 应用启动后窗口要过一段时间才真正被合成；
在那之前 present 完全不被节流，样例自己数出约 **2080 fps**，而 kgsl 里
**一条 GPU 提交都没有**。实测这个状态还会在一次运行中**反复出现**
(实测一轮里见过 真实 15s → 空跑 9s → 真实 7s → 空跑)。
只看应用自报的 FPS 完全看不出来，只看日志会以为跑得飞快。
→ 两道闸：`ready.py` 等帧率序列里那一级阶跃下降才开采；采完再用
`crosscheck.py` 把**内核侧提交节奏**和**应用侧自报帧率**对账，差过 35% 当场判无效。
这道对账不依赖任何时长常数，换机器换系统也不会悄悄失效。

**(c) `pidof` 不能当结束信号。** Android 在所有 activity 结束后会把进程留成
cached process，`pidof` 仍然有值。拿它当结束判据会让每一轮都空等满超时再判成无效。
→ 用 (a) 里那行 `Total device memory leaked` 当结束信号。

## 4. 轮内有效性判定(任一条不过 → `INVALID`，证据保留、样本不计、就地重试至多 2 次)

1. 采集窗内 GPU 提交数 ≥ 200(低于此值 = 没在出帧)
2. 提交线程占比 ≥ 80%(提交口径唯一)
3. 内核侧/应用侧帧率交叉校验相对差 ≤ 35%
4. 采集窗内热事件数 = 0
5. 进程干净自退

## 5. 硬盘纪律

整仓带 submodule 与资产浅克隆后 **4.2 GB**；`build_vks.sh` 会裁到只留 4 个样例
(全仓 114 个)与 2 个场景(`sponza` 82M / `space_module` 31M，删掉的
`bonza` 359M / `vokselia` 272M / `morpheus_team` 135M 等)。
构建过程另占约 1.5 GB(gradle 依赖 + 原生中间产物)。

`build_vks.sh` 全程带看门狗：任何一步剩余空间低于 `VKS_MIN_FREE_MB`(缺省 8500MB)
立刻停并退 9。这台机器上同时有别的 agent 在编译，实测剩余空间会在几分钟内被别人吃掉
数 GB，所以这个看门狗是常开的，不是调试开关。

## 6. 本次落地时的实测记录(2026-09-23 · NX809J / Adreno 840v2)

已经证实可用的：

| 项 | 结果 |
|---|---|
| 浅克隆 + 裁剪后占盘 | 4.2 GB → 1.6 GB(工作树)；APK 19 MB |
| arm64 release APK | 出包成功(gradle 8.9 / AGP 8.7.2 / NDK 28.2.13676358 / SDK cmake 3.22.1) |
| `--config` 钉档 | 生效，日志有 `applied configuration index N` |
| 固定帧数自退 | 生效(`--stop-after-frame` + `--force-close`) |
| 提交线程自动识别 | 生效，6s 窗口内 2430 次提交、占比 99.5% |
| `render_passes` 两档的量级 | config 0 ≈ 335 fps，config 1 ≈ 360 fps(单次观测，**不是统计结论**) |

**还没做完的**：`run_ab.sh` 的交错 5v5 电池没跑完，所以还没有 p 值。
原因是这台机器只有一台手机、多 agent 排队，本轮拿到设备的时间窗被上面 §3 的三个坑
吃掉了，修完坑之后设备已被别的任务接管。脚本本身是完整的，拿到设备直接：

```
bash run_ab.sh render_passes 0 1 5     # loadOp/storeOp 两臂，交错 5v5
bash run_ab.sh subpasses    0 1 5      # subpass 合并 vs 两个 render pass
```

`frames` 要给够：本机真实渲染约 340 fps，而每轮要跨过 §3(b) 的空跑段 + settle + 12s
采集窗，所以缺省 3000 帧不够，建议 `run_ab.sh <sample> <a> <b> 5 <outdir> 30000`。
