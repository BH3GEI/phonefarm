# 设备接口：怎么接一种新设备 / 新平台

phonefarm 里所有「和设备说话」的功能（任务回放 `run`、`perf`、`fleet`、`cts`、`gpu-op` 的设备部分……）
都只持一个 `device::Device`。`Device` 背后是一个 `platform::Backend` 实现：

```
调用方 ──► device::Device ──► Box<dyn platform::Backend> ──┬─ device::Adb  Android      serial 不带前缀
                                                           ├─ device::Hdc  OpenHarmony  serial = "hdc:<key>"
                                                           ├─ ios::Ios     iOS          serial = "ios:<udid>"
                                                           └─ 你的实现     serial = "<前缀>:<key>"
```

## 现有平台一览

| 平台 | 传输 | serial 形态 | 能做 | 不能做 |
| :--- | :--- | :--- | :--- | :--- |
| Android | `adb` | 裸 serial | 三层改动面 + 全套遥测 | — |
| OpenHarmony | `hdc` | `hdc:<connect key>` | 只能测（无 root） | 三层改动面 |
| **iOS** | `wda` | `ios:<udid>`（udid 可省） | **看与动**：UI 树、截图、点按/滑动/输入、应用启停 | 三层改动面、遥测、shell、文件进出（见 `src/ios.rs` 头部）；真机体检走 `phonefarm ios-probe` |

### iOS 后端（`src/ios.rs`）

经 **WebDriverAgent**（WDA）操作真实 iPhone。隧道取值顺序：环境变量 `WDA_URL` >
本机 `127.0.0.1:18100` > 该端口没人听且 serial 带了 udid 时自起 `iproxy` 并托管其生命周期。
WDA 本体需要在手机上先装好并启动（见 iPhone-use 的 setup 流程），本后端**不负责装**。

三件必须知道的事：

1. **坐标一律是 WDA 的「点」(point)**，`size()` 也返回点。安卓侧是像素，两者语义不同，
   但下游用的是归一化到 0~999 的相对位置，单位在归一化时相消。
2. **`clickable` / `scrollable` 是推导出来的，不是设备给的。** WDA 的 JSON source 没有
   安卓那套标志，本后端按 `XCUIElementType` 推导（口径在 `is_actionable` / `is_scrollable`）。
   推导保守：控件类型 + `accessible` + 可用且可见，三条都满足才算可点。
3. **性能数据来源是「整条通路不可用」**（`perfsrc::SRC_IOS_NONE`），不是「字段缺」。
   WDA 不提供帧时与功耗，设备侧也没有 sysfs/ftrace —— 因此 iOS 后端**不能用于优化闭环的
   改动面评测**，只能做 UI 遍历与画面取证。

> **状态：形状已可一条命令核实，但本仓库环境上未跑过真机。**
> 解析与报文构造有单测钉死（`src/ios.rs` 的 tests 段）；WDA 各版本的 JSON source 形状
> **不靠文档猜 —— 靠 `phonefarm ios-probe`**。

```bash
phonefarm ios-probe [--serial ios:<udid>] [--json]
```

逐项探 WDA 的每个端点（隧道/status、会话、窗口尺寸、**界面树形状**、前台应用、应用清单、
截图），并**核实界面树形状与解析器的假设对不对**：解出多少节点、多少个带 `type` / 文字 /
坐标、用的是 `rect` 对象还是平铺形态、有多少零面积框。形状对不上就把 `problems` 打印出来，
而不是静默返回空树。

退出码 `0` = 全部就绪（可以把上面那条「未上真机」改掉）；`1` = 有端点不通或形状不匹配
（并给出逐条修复建议）；`2` = 用法错。

这一条命令的存在理由：把「未验证」变成一个**跑一次就有答案**的问题，
而不是一句等人想起来的备注。

## 接一种新平台要做的事

1. **实现 `platform::Backend`**（`src/platform.rs`）。分两段：
   - 必须实现：身份（`transport` / `platform` / `layers`）、连接（`set_frozen` / `health_check` / `revive`）、
     shell 与文件（`shell` / `stream_shell` / `push_file` / `pull`）、看（界面树 `els_full`、前台包、截图、
     屏幕尺寸……）、动（点按 / 滑动 / 输入 / 返回 / 回桌面）、应用启停（`force_stop` / `launch`）、遥测。
     语义以 `device::Adb` 上的同名方法为准。
   - 可选（有缺省实现）：端口转发、内存截图 `screen_image`、虚拟手柄。缺省实现如实返回
     `Err("… 未实现")` / `None`，不假装成功；平台有就覆盖。
2. **在 `platform::open` 里登记 serial 前缀**（一行 match 分支）。不带已登记前缀的 serial 一律走 Android，
   保证安卓侧行为不变。
3. **填 `layers()`**：三层改动面 + 测量各一条，写清能不能（`Yes` / `IfMet` / `No` / `Unknown`）、前提、原因。
   单测 `every_platform_answers_for_every_layer_once` 要求每层恰好一条、原因不为空。
4. **性能数据来源**：`perfsrc::for_device` 按 `backend_name()` 选采集源（安卓 sysfs / 鸿蒙 HiSmartPerf），
   新平台在那里加一个 `PerfSource` 实现（见 `docs/SPEC_PERF_SOURCE.md`）。
5. **设备枚举**（可选）：`phonefarm devices` / `fleet` / `keepalive` 现在各自调 `adb devices` 与
   `hdc list targets` 列设备，新平台要出现在设备清单里，需要在这三处各加一段枚举。

做完 1–3，`run` / `perf` 等通用功能就能在新平台上用；`Device::from_backend` 也可以直接塞一个实现
（例如测试替身）。

## 还没走这个接口的地方（如实列出）

这些地方直接调 `adb`，换平台时要单独移植，或者明确拒绝：

| 位置 | 做什么 | 现状 |
|---|---|---|
| `eval.rs`（`adb()` / 采样进程） | `game_opt_loop` 下发的 ③ ② 评测：推工具、写 sysfs、su 跑采样脚本、拉证据 | 只支持 Android。整条判定链路假定 `su` + sysfs + ftrace |
| `capture.rs` | 超分训练数据截帧（`exec-out screencap`） | 遇 `hdc:` 明确拒绝 |
| `bench.rs` | kgsl + TFLite GPU Delegate 基准 | 遇 `hdc:` 明确拒绝 |
| `gamepad.rs` | uinput 虚拟手柄守护进程 | 只 Android；`Hdc` 走 trait 缺省实现报「不支持」 |
| `fleet.rs` / `keepalive.rs` / `main.rs devices` | 设备枚举 | 各自调 `adb devices` 与 `hdc list targets` |

`eval.rs` 是改动面真正落地的地方，也是新平台成本最高的一块：要改的不是传输（adb → 别的），
而是「怎么写系统参数、怎么挂渲染层、怎么采样功耗与帧时」这些平台机制本身。

## 每个平台能开哪几层，为什么

代码里的声明在 `platform::ANDROID_LAYERS` / `platform::OPENHARMONY_LAYERS`，真正开不开以每次
真机摸底（`game_opt_loop optimize` 的 caps）为准。

| 层 | Android（adb） | OpenHarmony 商用机（hdc） |
|---|---|---|
| ③ 系统参数 | **能**，要 root。白名单靠真机「写入→读回→还原」探出来，写了被内核弹回的不进白名单 | **不能**。拿不到 root，hdc shell 是普通用户，sysfs 频率/调度节点写不进去 |
| ② 渲染流程改写（Vulkan 层） | **能**，要 root 且目标应用已装：`setprop debug.vulkan.layers` + 把 `.so` 放进目标应用自己的 `nativeLibraryDir`。`settings put global gpu_debug_*` 那一套在实测机上不生效 | **不知道**。OpenHarmony 图形栈有没有等价的 layer 注入点没有设备验证过；即便有，往目标应用目录放 `.so` 同样要 root |
| ① 画面算子 | 有条件：白盒载体（refbench）直接挂进管线；黑盒游戏只能经 ② 的挂层通路注入 | **不能**：黑盒依赖 ② 的通路；refbench 没有鸿蒙版，没有白盒载体 |
| 测量 | 能：帧时走 SurfaceFlinger / ftrace kgsl，功耗走 sysfs 电源轨（要停充才是整机功耗，停充要 root） | 能：HiSmartPerf（SP_daemon），不需要 root |

所以国内鸿蒙商用机上 ①②③ 一层都改不了，只能测。可能的出路（工程机、厂商官方性能接口、
开发者模式自签名应用、海外版 EMUI 机型）都还没核实。
