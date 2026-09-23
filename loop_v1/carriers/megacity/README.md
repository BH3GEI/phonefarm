# carriers/megacity — 把 Megacity 做成确定性可复跑的渲染负载

loop_v1 到目前为止的负载有两类: 原神(真游戏, 但闭源、只能从外面观察)和
[refbench](https://github.com/BH3GEI/refbench)(白盒靶子, 但不是真游戏)。
这个载体想补中间那一格: **有源码的真游戏** —— 场景复杂度、shader 数量、
subscene 流式加载都是真的, 同时源码在手, 相机和帧序可以钉死。

> **状态: 首轮在红魔上跑通了契约, 但城市当时没渲染出来; 已定位并修复, 待复验。**
>
> 首轮实证(2026-09-23, 红魔 / Adreno 840 / Vulkan): `clean_exit=true`, 600/600 帧,
> 路线 sha256 与仓库一致, **`render_thread_comm` 实测为 `UnityGfxDeviceW`**(此前只是猜)。
> 契约的进/出、固定帧数、自报事实这条链是通的。
>
> 但首轮画面上只有天空 —— 见下面「首轮踩到的四个坑」。四个都已改, **还没复验**。

---

## 首轮踩到的四个坑

记在这里是因为每个都花了真金白银的调试时间, 而且没一个是从代码上看得出来的。

### 1. 城市没渲染 —— 不是 UGS 卡住, 是没等它装完

第一反应会怪那个 "TO USE UNITY'S DASHBOARD SERVICES…" 的弹窗, 或者怀疑
subscene 要玩家驱动才流式加载。**都不是。** `Main.unity` 里 6 个 SubScene
全是 `AutoLoadScene: 1`, 城市自动装、不需要玩家; 项目里也根本没有自定义流式
加载代码(`SubScene` / `SceneSystem` / `streaming` 三个词在 Assets/Scripts 下零匹配)。

真实原因平淡得多: **装完要时间, 而当时预热只有 300 帧**, 还没装完就开测了。

改法不是"把预热调大一点"——那只是换个数字继续赌。改成用
**实体总数连续 120 帧不再增长**作装完信号, `settle` 只当上界, 装完就提前走。
结果如实写进 `streaming.ready`(yes / timeout / unknown), **ready ≠ yes 的轮直接判无效**。

### 2. 录音权限框把应用挂起

Vivox 要 `RECORD_AUDIO`, 冷启动弹框 → 应用被挂起 → **协程根本不跑**,
表现是进程活着但永远不落 `megacity_started`。

`pm grant` 能解, 但根治办法更省事: 上游只在**非**单机模式下才实例化 Vivox
(`PlayerInfoController.Start()`)。所以 harness 直接切单机模式, 框就不会出现。
run 脚本里仍留一条 `pm grant` 作双保险, 幂等。

### 3. 进程杀不死

`Application.Quit()` 和 `System.Environment.Exit()` 在 Unity 的 Android 播放器上
**都杀不掉进程** —— json 写完 30 秒后 `pidof` 仍有结果, 上层的存活探测会一直空等。
改用 `android.os.Process.killProcess(myPid())`, 在 json fsync 之后调。

### 4. adb push 进去的路线, 应用读不到

scoped storage 下 `adb push` 到 `Android/data/<pkg>/files/` 的文件属主是 `shell`,
应用(`u0_a512`)看不见, `File.Exists` 返回 false; `su` chown 也没救回来。
首轮"成功"那次其实读的是 APK 内 StreamingAssets 那份。

所以整条 push 路径去掉 —— 留着只会让人以为能用。**副作用反而是好的**:
APK 加上游 commit 就完整决定了负载, 换路线必须重新出包, 而路线本来就是被测配置
的一部分, 本就该这样。run 脚本改为核对包内 `route.sha256` 与仓库是否一致。

---

## 对上游的三处干预, 全部走反射

切单机模式、关 UI、读实体数 —— 这些类型都在上游的 asmdef 程序集里。直接引用会把
本文件和上游的程序集结构绑死, 上游一改就炸编译, 而一次重新出包是几十分钟。
所以统一走反射, 拿不到就**如实记 `mode_set=none`, 不假装成功**。

---

## 选型: 为什么是 megacity-metro 而不是 Megacity-2019

任务书里倾向 Megacity-2019, 理由是它本来就是固定路线 flythrough、不用处理 netcode。
只读查证后这个前提不成立, 改用 megacity-metro。三条硬证据:

1. **Megacity-2019 是 HDRP, 打不了安卓包。**
   `Packages/manifest.json` 里是 `com.unity.render-pipelines.high-definition: 14.0.8`。
   HDRP 的目标平台只有桌面与主机, 没有 Android/iOS。它的 README 也自陈
   "Cross-platform support for **Windows and Mac**"、"currently tested on Windows and Mac"。
   工程里 Android 的图形 API 停在 `GLES3`、目标架构是 `5`(ARMv7|X86, **不含 ARM64**),
   就是"安卓从来不是真目标"留下的默认值。这条是硬阻断, 不是调设置能绕的。
2. **它已被上游废弃。** README 原话: "This repository is deprecated and will no longer be
   maintained", 并指向 megacity-metro。
3. **"不用处理 netcode"也不成立。** 它同样依赖 `com.unity.netcode: 1.0.15`。

反过来看 megacity-metro, 它的安卓目标**已经是照我们要的样子配好的**:

| 项 | megacity-metro | Megacity-2019 |
|---|---|---|
| 渲染管线 | **URP 17.1.0** (移动可用) | HDRP 14.0.8 (移动不可用) |
| Android 图形 API | **Vulkan** (已设, 非 auto) | GLES3 |
| Android 目标架构 | **ARM64** | ARMv7\|X86 |
| Android 脚本后端 | **IL2CPP** | — |
| minSdk | 23 | 22 |
| 官方平台声明 | Windows / Mac / **Android / iOS** | Windows / Mac |
| 维护状态 | 在维护 | **已废弃** |
| Unity | 6000.1.0f1 | 2022.3.9f1 |

还有一个现成的 `Assets/Settings/Build Profiles/Android Profile.asset` 和自定义的
`Assets/Plugins/Android/`(含 `MegacityMetro.java`) —— 安卓端是被认真做过的。

**代价**: metro 是 Netcode for Entities 的多人射击, 而我们要单机固定场景。
处理办法见下面「怎么绕开 netcode」—— 结论是不用动它。

---

## 为什么构建放在 magicbook 而不是采集机

采集机(本机 Mac)装不下 Unity。实测数字:

| 项 | 安装占用 |
|---|---|
| Unity 6000.1.0f1 Editor (macOS arm64) | 9.01 GB (另需 5.00 GB 临时下载) |
| Android 模块全树 (NDK r27c 2.57 GB + SDK/JDK/build-tools) | 5.89 GB |
| 小计 | **14.90 GB** |
| 工程检出 (git 40 MB + 工作树 89 MB + LFS 434 个对象 0.43 GB) | 0.55 GB |
| Library / 烘焙缓存 (DOTS subscene + ASTC 重导入 + Burst AOT) | 10–25 GB (估) |
| 合计 | **约 26–41 GB** |

采集机当时剩 10.87 GB 且在被其它任务持续吃掉 —— 光 Editor+Android 那 14.90 GB
就已经大于整块剩余空间。macmini2/macmini3 同样只剩约 11 GB。
**magicbook (办公室 Windows) C: 有 203 GB 空闲**, Windows 版 Editor 7.79 GB +
Android 5.63 GB = 13.42 GB, 加工程和缓存约 24–39 GB, 宽裕。

外接 U 盘(exFAT, 62.69 GB, ~12 MB/s)**不能**放 Editor、Library 或 git 工作区:
exFAT 没有符号链接和 unix 权限, 而 12 MB/s 跑 DOTS 烘焙等于没法用。
它的用处是存原始 ftrace 证据 —— 采集机自己没那个空间。

所以分工是: **magicbook 出包 → APK 传回采集机 → 采集机 adb install + 采数**。
手机只连采集机, 这一段不能挪。

---

## 怎么绕开 netcode

不绕。metro 的联机逻辑挂在游戏玩法上, 而我们要的只是"把这座城按固定路线渲染出来"。
harness 的做法是**旁路而非改造**:

- 启动后切到 `Assets/Scenes/Main.unity`(城市本体及其 subscene 都在这儿),
- 关掉场上所有相机, 换成自己的一台, 位姿只由帧序号决定,
- 玩法系统照常空转, 但它不再影响我们看到什么。

上游一个既有文件都不用改, 只新增 3 个 .cs。上游更新时不会冲突。

**残留风险(诚实记一笔)**: 交通/飞艇/全息广告这些 ECS 系统仍在跑, 它们如果用了
未固定种子的随机或依赖 wall-clock, 会给轮间引入差异。harness 固定了
`UnityEngine.Random` 的种子, 但管不到 ECS 侧自己的 `Unity.Mathematics.Random` 实例。
**这件事只能实测**: 跑 3 轮基线看 frame_p95 离散度 —— refbench 是 0.45%,
原神是 1.37%。落在这个区间说明可用; 明显更差就得回来把那几个系统按住。

---

## 确定性是怎么保证的

| 手段 | 为什么 |
|---|---|
| 相机位姿 `u = 帧序号/(frames-1)`, **不读 deltaTime** | 这是根。帧率高低不改变走过的路径, 两轮才可比。用 deltaTime 的话快的那轮会飞得更远, 测的就不是同一段负载 |
| 静置阶段等"实体数不再增长" | 等城市真的装完, 而不是赌一个帧数。装不完就标 `timeout` 并判该轮无效 —— 城市没装完, 量到的不是这座城 |
| 预热窗口(默认 600 帧)在路线起点空跑 | 静置之后再等 shader 与常驻分配落定。预热结束才落 `megacity_started`, 采集从那一刻起算 |
| 进场后反复关 Canvas / UIDocument | HUD 和 UGS 弹窗既挡视野又进渲染负载。UI 可能晚于场景出现, 所以整个静置期反复关 |
| 路线文件 sha256 写进输出 json | 两臂 sha256 不一致 = 走的不是同一条路线 = 这次对照无效, 一眼看得出来 |
| `resscale` / `vsync` / 质量档显式写死并回写实际生效值 | 不让引擎自适应。写进去的和落下来的对不上就是 bug |
| 路线缺失/非法一律 `clean_exit=false` 退出 | 与 refbench 的 `knob.postfx` 同一条原则: **绝不静默降级**。悄悄退回默认相机会让 A/B 两臂变成 A 和 A, 而上层还以为在对照 |
| 构建后自检图形 API / 架构 / 后端 | 悄悄退回 GLES3 或 armv7 的包, 测出来的数跟我们以为在测的东西没关系 |

载体侧**不测量、不判定**。帧时、离散度、归因全部由 loop_v1 从 raw ftrace 算 ——
和 refbench 同一条边界。

---

## 接口契约

进 = intent extras, 出 = JSON。完整定义在 [`contract/`](contract/)。

```bash
adb shell am start -W -n com.unity.megacity.metro/com.unity.megacity.MegacityMetro \
  --es scene city_flythrough --es run_id base1 --es frames 3600 \
  --es route route_a --es warmup 600 --es resscale 1.0 --es vsync off
```

- 开始标记: `/storage/emulated/0/Android/data/com.unity.megacity.metro/files/megacity_started`
- 输出: 同目录 `megacity_out.json`(`Flush(true)` 落盘后才退进程)
- 场景: `city_flythrough`(沿路线穿城) / `city_static`(钉在起点, 作对照臂)
- **不带 `scene` extra 启动时 harness 完全不激活**, 同一个 APK 就是原版游戏

两处与 refbench 的刻意不同, 别照抄:

1. **退出码不是契约**。这是 Unity Activity 不是 NativeActivity, 退出码经 `am start`
   观测不到。轮的有效性一律看 `clean_exit`。
2. **提交线程名不写死**。refbench 能自己 rename `/proc/self/task/<tid>/comm`, 这里不改
   上游引擎线程名, 所以 harness 自己扫一遍 `/proc/self/task/*/comm` 把实测名字写进
   `render_thread_comm`, `run_megacity.sh` 再把它传给 `parse_trace.py --comm`。
   定位不到就报 `UNRESOLVED[...]` 并把所有线程名列出来, 不返回一个猜的值。
   同理 `submits_per_frame` 也不写死, 交给 loop_v1 的自相关自检。

---

## 怎么用

### 1. 构建机准备(magicbook)

前置 — **这一步必须人工做, 脚本不代劳**: 在 magicbook 上装 Unity Hub,
用 Unity ID 登录并激活 Personal 许可证, 然后装 Unity **6000.1.0f1** +
Android Build Support(含 OpenJDK / SDK / NDK)。

```powershell
powershell.exe -ExecutionPolicy Bypass -File setup_on_magicbook.ps1 -ThirdParty C:\thirdparty
powershell.exe -ExecutionPolicy Bypass -File build_megacity.ps1 -ProjDir C:\thirdparty\megacity-metro
```

> 两个 `.ps1` **必须存成带 BOM 的 UTF-8**。Windows PowerShell 5.1 对无 BOM 的 .ps1 按 ANSI
> (中文系统上是 GBK) 解码, 注释里的中文会被打碎成非法字符, 直接报
> `Missing argument in parameter list` / `The string is missing the terminator` —— 实际踩过。
> 编辑这两个文件后别把 BOM 去掉。magicbook 上没有 pwsh, 只有 5.1。

第三方工程克隆到 `C:\thirdparty\`, **不进本仓库**。产物在 `out\megacity.apk`。

### 2. 路线

`routes/route_a.json` 是**从上游场景数据推导出来的**, 不是谁在编辑器里拖出来的一串数字。
生成脚本 `routes/make_route_a.py` 读 `Assets/Scenes/Main/MegacityMetroLevelBounds.unity`,
按下面三步算出航点:

1. 城市中心 C = 66 个边界 `Point` 的形心, 实测 `(-6.9, -3.7, -5.1)`
2. 城市半径 R = C 到各 Point 的最大水平距离, 实测 `130.8`; 城市顶 `y = 110.7`
3. 相机绕 C 走一段下降圆弧: 水平半径 `1.8R = 235.4`, 高度从 5 个出生点的平均高度
   `425.2` 降到 `城市顶 + 40 = 150.7`, 扫 270°, 全程朝向 C

**这条路线不可能穿进楼里**: 水平半径 235.4 > 城市半径 130.8, 且高度全程 > 城市顶 110.7 ——
确定性不依赖"运气好没撞到东西"。同时整座城一直在视野里, 渲染负载是满的, 且随距离和
角度连续变化, 不是一段死板的平移。

重新生成(上游场景变了才需要):

```bash
python3 routes/make_route_a.py \
  /path/to/megacity-metro/Assets/Scenes/Main/MegacityMetroLevelBounds.unity \
  routes/route_a.json
```

脚本输出固定键序与缩进, 同一份输入每次生成**逐字节一致** —— 否则输出 json 里那个
sha256 就没有意义了。上游场景结构变到解析不出 66 个 Point 或 5 个 SpawnPoint 时,
脚本会直接报错退出, 不会猜着往下算。

航点之间 Catmull-Rom 插值, 朝向 Slerp。**航点顺序和数量一旦定下就不要再改** ——
改了 sha256 就变, 跨轮不可比。要新路线就起个新名字, 别覆盖 route_a。

`route_a.example.json` 留着只作格式说明, 不参与实跑。

### 3. 采集(采集机)

**测试条件(每轮证据里都要记上)**:

- **红魔内置风扇: 开**, 且全程保持开启。这会抬高稳态帧率、压低热事件触发率,
  所以它不是"无关的环境细节" —— 风扇开/关的两批数据不能混在一起比。
- 设备是共享的。用 `/private/tmp/claude-501/devlock` 抢锁, 本载体的 label 是
  **`wb-megacity`**:

  ```bash
  /private/tmp/claude-501/devlock run wb-megacity -- \
    bash run_megacity.sh base1 runs/mc_base1 city_flythrough route_a 3600
  ```

  `run` 会拿锁 → 后台每 60 秒自动续期 → 跑完自动释放。被别人占着时直接返回 1,
  不要绕过去硬跑 —— 两个任务同时压一台手机, 两边的数都废。


```bash
adb install -r megacity.apk
bash run_megacity.sh base1 runs/mc_base1 city_flythrough route_a 3600
```

`run_megacity.sh` 的结构与 `loop_v1/refbench/run_refbench.sh` 对齐:
温度回落轮询 → 推路线 → 启动 → 等 `megacity_started` → ftrace 采集 → 等自退 →
收证据 → 轮内有效性判定。退出码 `0` 有效 / `2` 硬失败 / `3` 无效轮(证据保留)。

---

## 目录

```
contract/
  launch.json          进: intent extras 的完整定义
  output.schema.json   出: megacity_out.json 的 schema
unity/                 注入上游工程的胶水 (只新增, 不改上游既有文件)
  MegacityRefbenchHarness.cs   Assets/Scripts/Refbench/  运行时: 接管相机, 走固定路线, 出 json
  UpstreamCommit.cs            Assets/Scripts/Refbench/  构建时被重写, 把上游 commit 钉进包
  MegacityRefbenchBuild.cs     Assets/Editor/            批处理出包 + 构建后自检
routes/
  make_route_a.py      从上游场景推导航点(可复现, 逐字节确定)
  route_a.json         推导出的实跑路线
  route_a.example.json 格式样例, 不参与实跑
setup_on_magicbook.ps1 构建机: 克隆上游 + 注入胶水 + 版本核对
build_megacity.ps1     构建机: 零交互出 arm64/Vulkan/IL2CPP 的 apk
run_megacity.sh        采集机: 跑一轮并收证据
```

## 还没做的

- [x] ~~用户在 magicbook 登录 Unity 激活许可证~~ 已完成, 构建日志实证 `UnityPersXXXX`
- [x] ~~装 Unity 6000.1.0f1 + Android 模块~~ 已装齐(NDK/SDK/OpenJDK)
- [x] ~~让三个 .cs 过编译~~ 通过, `errors=0`
- [x] ~~标定 `route_a.json`~~ 已改为从上游场景推导, 见「路线」一节
- [x] ~~出第一个 APK, 核对图形 API 真的落在 Vulkan~~ 构建后自检通过: arm64 / Vulkan / IL2CPP
- [x] ~~装到红魔跑通第一轮~~ 契约跑通(clean_exit / 600 帧 / Vulkan), 但暴露四个坑, 已修待复验
- [ ] **复验: 目视确认相机看到的是城市而不是一片天空**(首轮是天空, 四个坑修完后待验)
- [x] ~~实测 `render_thread_comm`~~ 确认是 `UnityGfxDeviceW`, 已固化进 contract
- [ ] 3 轮基线, 报 frame_p95 离散度, 与 refbench 的 0.45% / 原神的 1.37% 对齐着看
