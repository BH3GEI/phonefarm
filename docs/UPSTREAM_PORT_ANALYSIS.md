# 上游功能树 → phonefarm(公司版) 逐功能对照

> 状态: 分析稿（未落地代码）。本文件先只回答「有哪些功能、每个功能在 phonefarm 里算什么」。
> 目标仓库: 公司版 = `git@github.com:HGamey/phonefarm.git`（本机 checkout `~/Desktop/code/agent/projects/phonefarm`），
> 个人版 = `git@github.com:BH3GEI/phonefarm.git`（同一 checkout 的 `origin`）。
> 上游三项目（均已完整 clone 到 `/tmp`，HEAD 见下表）：

| 上游 | 许可 | 语言 | 规模 | HEAD |
| --- | --- | --- | ---: | --- |
| [unixzii/SimRun](https://github.com/unixzii/SimRun) | MIT (© 2026 Cyandev) | ObjC / Swift | ~19.6k 行 / 151 文件 | `e77beff` |
| [FluidInference/FluidUse](https://github.com/FluidInference/FluidUse) | Apache-2.0 | Swift(CoreML) / Python | ~41.3k 行 / 315 文件 | `9384e70` |
| [zhongerxin/iPhone-use](https://github.com/zhongerxin/iPhone-use) | MIT (© 2026 ZHONG XIN) | Python / TS | ~12.4k 行 / 81 文件 | `08ed5b0` |

---

## 0. 前提：为什么是「逐功能移植」而不是「逐字节 copy」

三个上游和 phonefarm 的语言、平台、领域都不同，字面意义上没有可以「逐字节」搬的东西：

| 轴 | phonefarm | SimRun | FluidUse | iPhone-use |
| --- | --- | --- | --- | --- |
| 语言 | Rust | ObjC + Swift | Swift + Python | Python + TS |
| 宿主 | macOS/Linux CLI | macOS AppKit + iOS-sim 注入 dylib | macOS (CoreML/ANE) | macOS + MCP stdio |
| 被控设备 | Android / OpenHarmony 真机 | iOS 模拟器构建产物（在 Mac 上跑） | 本机 Mac App（AX API） | 真实 iPhone（USB + WDA） |
| 核心动作 | 图形/系统参数优化 + 统计判定 | 把 sim app 变 Mac 窗口 | 本地小模型做 form 决策 | 通过 MCP 操作手机 |

所以本文档把「copy 过来」拆成三类可执行动作：

- **搬**（port）：功能语义可直接在 phonefarm 的架构里重建，产物是 Rust 代码 / 新 Backend / 新 MCP 工具。
- **借**（borrow）：不搬代码，搬工程模式或协议约定（如锁语义、观测预算、skill 分发）。
- **不适用**：平台特有、或与 phonefarm 的硬边界冲突。

**许可证义务（必须落进 PR）**：MIT / Apache-2.0 允许商用与修改，但要求保留版权与许可声明。
任何「搬」进来的代码都要在 `THIRD_PARTY_NOTICES.md`（新建）里登记来源仓库、commit、许可，
并在派生文件头部注明出处。这部分不是可选项，不能只写「同事做的」。

---

## 1. phonefarm 现状功能面（对照基线）

来自 `AGENTS.md`、`src/cli.rs`、`src/serve.rs`、`src/platform.rs`：

- **F1 设备与农场运维**（零 token）：`devices` / `keepalive` / `fleet` / `probe` / `exec` / `tools/devlock`。
- **F2 自主 UI 遍历**（VLM，烧 token）：`run` / `benchmark` / `parallel` / `plugins` / `quest`。
- **F3 确定性脚本与回放**（零 token）：`script`（含 `--detach`）。
- **F4 一致性测试 harness**：`test-batch` / `cts-fetch`（Android CTS + OH 双协议）。
- **F5 端侧模型标尺与采集**：`bench`（锁频 + 等冷 + GPU delegate）/ `capture`。
- **F6 Compute Shader 算子标尺**：`eval`（v1/v2 路由）/ `gpu-op`（A/B/A/B + Welch t）。
- **F7 系统参数与灰档**：`sysparam` / `gray-layer` / `gray-ab` / `shot-diff` / `autoloop`。
- **F8 判定与证据（硬边界）**：`loopstat` / `loopreport` / `experiment` / `hyp` / `pred` / `caps` / `lessons`。
- **F9 MCP stdio 服务**：`serve`，工具 `phonefarm_{devices,run,runs,last,cat,show,tree,lessons,hyp,pred,caps,tools,export,eval,experiment,campaign,tasks,config,stats,status,probe,exec,benchmark,quest,...}`。
- **F10 设备后端抽象**：`platform::Backend` trait + `open()` 按 serial 前缀登记（现仅 `Adb` / `Hdc`）。
- **F11 记录契约与遥测**：`telemetry` / 设备锁 `$DEVLOCK_LABEL`。
- **F12 模型调用层**：`brain.rs`（唯一模型入口，多 provider failover）/ `llm.rs`（白名单选参 + 本地变异器降级）。

---

## 2. A 树：SimRun

**定位**：把 iOS 模拟器构建产物（`.app`）当普通 macOS 进程跑起来，不要 Simulator.app / `launchd_sim` /
backboardd / SpringBoard；每个 app 一个 macOS 窗口。

### A1 构建与分发
- A1.1 `make core` — 产出 `SimRun.app`（simrund + launcher + simns + 13 个注入 dylib + SimFramebuffer）与 `simrun` CLI
- A1.2 `make test-apps` — 10 个示例 app（Hello/SUI/HIDTest/Glass/DragTest/WebTest/BGDownload/PickerTest/Showcase/AudioPlayer）
- A1.3 `make test-programs` — 7 个探针（nettest/nwtest/pbtest/kctest/loctest/mpstest/audiotest）
- A1.4 `make install` — 装进 `/Applications`
- A1.5 自带 agent skill 并随 app 分发（`skills/simrun`，含 `reference/api.md`）

### A2 CLI（`simrun`，13 个子命令，经 unix socket 与常驻 server 对话）
- A2.1 `run [options] <app|sim-binary> [args…]` — 参数：`-p/--profile`、`-r/--runtime`、`-d/--device`、`--insert <dylib>`、`--headless`、`--pick`（可重复）、`--mute`；透传 argv/env/stdio fd，退出码 = app 退出码
- A2.2 `list` — 运行中 app（pid/app/profile/device/runtime，标 headless）
- A2.3 `quit <pid|app>`
- A2.4 `screenshot <pid|app> [-o file.png]` — 按上一次绘制的像素尺寸导出
- A2.5 `eval <pid|app> [--snapshot] [<script>|-f file|-]` — 在运行中 app 里跑 JS，实时回放 console，可顺带打印无障碍树文本
- A2.6 `pick <pid|app> [--timeout s] [--cancel|path…]` — 代替人回答文档选择器
- A2.7 `inspect <pid|app> [--watch] [--png file [--3d]] [--check] [--set <id> <key>=<json>]` — 图层树 + 内容拷贝 + 3D 视图 + 差异校验 + 运行时改图层属性
- A2.8 `profiles [<bundle|Sim.app>]` — 数据容器 / 多 profile
- A2.9 `runtimes` / A2.10 `devices [-r runtime]`
- A2.11 `skill [path|install [--project][--claude]|install --dir <dir>]` — 分发 agent skill，支持同时链到 Claude Code
- A2.12 `status` / `stop` / `log`
- A2.13 `tools/crash.py <procname> [n]` — 取最新崩溃报告回溯

### A3 宿主服务（simrund，AppKit）
- A3.1 launcher UI：sidebar 库 / 详情面板 / Profiles 标签 / 工具栏 / 右键菜单 / 记忆库
- A3.2 **Add Xcode Simulator Builds**：扫 `~/Library/Developer/Xcode/DerivedData/*/Build/Products/*-iphonesimulator/`
- A3.3 **device build 转换**：把真机构建（`IOS` 且未加密）APFS clone 后打上 `IOSSIMULATOR`、改名 `__RESTRICT`、重签 ad-hoc，带 stamp 缓存（85 MB ≈ 0.3 s，后台队列）
- A3.4 Launch Options sheet：runtime / device / argv / env / 语言地区 / 服务开关 / 诊断开关，按 app+profile 存 `SRLaunchOptions`
- A3.5 session：一 app 一窗口，窗口层承接帧
- A3.6 services：宿主侧假系统服务（IOSurface.Remote、metal.simulator、pasteboard、docpicker、audiohald 转发）
- A3.7 inspector：图层树面板 + Mac 侧真实 CALayer 拷贝 + 3D 展开 + 编辑

### A4 注入到 app 的 sim 侧库（13 个 dylib）
- A4.1 `rshim` — Metal shader 编译器 XPC 拉起、bootstrap 兜底、Mac 语言/地区写进设备 global prefs、把 app HOME 钉到 sandbox 容器
- A4.2 `SimFramebuffer` — 进程内 render server 显示端（reverse-engineered C API）
- A4.3 `glue` — 进程内启动 `CAWindowServer`，把 scene 图层托管到显示（干 SpringBoard 的活）
- A4.4 `scenes` — 进程内 FrontBoard host，创建/更新 `UIWindowScene`
- A4.5 `hid` — 鼠标/键盘 → `IOHIDEvent` → `-[UIApplication _enqueueHIDEvent:]`；触控板滚动/捏合/旋转转成**真多点触控**
- A4.6 `pboard` — 进程内 pasteboard daemon，桥到 Mac 剪贴板
- A4.7 `drag` — 进程内 druid drag server（app 内拖放）
- A4.8 `docpicker` — `UIDocumentPickerViewController` 桥到 Mac 打开/保存面板
- A4.9 `keychain` — 明文假 keychain（SecItem 语义复刻 + 明文 plist + access group）
- A4.10 `webkit` — WKWebView 的 helper 进程（WebContent/Networking/GPU appex）当 app 子进程拉起
- A4.11 `mps` — 修 MPSGraph 在模拟器上的两个 bug（device descriptor 无 architecture、shared heaps），让图跑上 GPU
- A4.12 `script` — JavaScriptCore 宿主：`simrun eval` 的 JS 在 app 主线程跑；无障碍树 API；watched 时流式吐图层树

### A5 `simns` — launchd_sim 替代
- A5.1 sim libxpc 的 XPC-pipe 协议实现（`--serve`）
- A5.2 每 app 一个私有 namespace，叠在共享 global namespace 上
- A5.3 service inventory 日志（`OK`/`MISSING`，`SIMNS_QUIET` 可关）
- A5.4 按需拉起真 sim daemon（trustd / cfprefsd …），共享、死后可再拉起
- A5.5 shared server mode

### A6 设备 / 运行时 / 数据
- A6.1 设备类型（`simrun devices`，默认 `*`）
- A6.2 iOS runtime 选择（`simrun runtimes`，`*` = SimRun 自带）
- A6.3 数据容器与多 profile（`default` + 自定义）
- A6.4 环境变量 30+ 个：`SIMRUN_{RUNTIME_ROOT,DEVICE,DATA,HEADLESS,DEBUG_STALL,CHROME_DEMO}`、`SIMNS_{QUIET,DAEMONS}`、`SIMRUND_{IDLE,SOCKET}`、`SIMAPP_EXTRA_INSERT`、`AUDIO_MUTE`、`SCENES_{W,H,SCALE}`、13 个 `*_VERBOSE`、9 个 `*_DISABLE`、`SIMFB_CHECK`、`OS_ACTIVITY_DT_MODE`
- A6.5 键盘快捷键：⌘R / ⌥⌘R / ⌘L / ⌥⌘I / ⌘Q·⌘W / ⌘0 / ⇧⌘K / ⌥⌘Q；Edit 菜单转发到 app

### A7 headless 模式
- A7.1 无窗口、不抢焦点、不显示 Dock 图标（`--serve` 时按需升为 regular app）
- A7.2 **每个 headless app 一套私有 pasteboard**（`dev.simrun.pboard.<pid>.<name>`），互不覆盖、不碰用户剪贴板
- A7.3 文件选择器不弹面板，挂起等 `simrun pick`
- A7.4 可在 CI / agent 并行场景下多开而不互相干扰

### A8 图层调试（`inspect` 的系统面）
- A8.1 app 侧：Core Animation commit hook 收集变更批次
- A8.2 Mac 侧：用真实 CALayer 复刻一份拷贝
- A8.3 过滤与特殊图层、contents 拷贝、动画、冻结
- A8.4 `--check` 与 live copy 对账（不一致 exit 1 + diff）

### A9 已知缺口（20 条，原文列的边界）
无 RunningBoard（后台 app 继续跑）/ 无软键盘 / 首个文本框聚焦卡 2–3 s / 无旋转与传感器 / 无 notifyd /
音频只有静音开关没有音量 / 后台 URLSession 不复活 app / 扩展与嵌入 framework 未测 / 无 secure paste /
无 LaunchServices / 拖放仅 app 内 / 文档选择器看不到 file provider / keychain 无访问控制 /
web view helper 未沙箱 / UIKit app 忽略 SIGTERM（1.5 s 后 SIGKILL）/ app 必须走 UIScene 生命周期。

---

## 3. B 树：FluidUse

**定位**：Apple silicon 上的本地 computer-use + 一堆「亚 1B 决策模型」的 Core ML 运行时。
读表单 → 小模型决定每格填什么 → 打进真 app。约 1 ms/决策，全在本机。

### B1 表单自动填写（computer-use）
- B1.1 `AccessibilityFormDriver` — 用 AX API 快照任意 `NSRunningApplication` 的字段/标签/值
- B1.2 `WebFormDriver` — 同上，针对嵌入 `WKWebView`
- B1.3 `FormSchema` — `renderOptions(entities:)` / `renderContext(formTitle:element:)` / `decode`（"fill Email: …"、check、click、skip）
- B1.4 `DocumentEntities` — PDF 或 `Label: value` 文本 → 填表 profile
- B1.5 `PredeterminedAnswer` — 覆盖模型不决策的问题式字段
- B1.6 `CuaS1FormsManager` — CUA-S1-FORMS（706K 参数）CoreML 表单专家
- B1.7 `cua-observer.js` — 浏览器侧观察脚本

### B2 决策模型运行时（原生 Swift，Core ML）
- B2.1 **Laya** — Jev 风格 typed decisions（`choice`/`score`/`noul`），322M mmBERT + decision head，4 个桶 128/256/512/1024、32 选项；M5 Pro 3.7 ms/短问；`Configuration` 选桶/计算单元/精度（fp16 / e8 int8 embedding）；mmBERT/Gemma byte-fallback BPE tokenizer 的 Swift 移植
- B2.2 **GLiNER 2.5** — small/base/multilingual，L128 桶、≤8 标签、只量化 embedding 到 8 bit
- B2.3 **Verdict** — 分数 + 校准（`.shipped`/`.uncalibrated`/`.temperature(T)`）+ 训练过的弃答
- B2.4 **GLiClass Edge Apps v2**
- B2.5 **D3（Decision 3.0）** — d3-lite / nano / mini
- B2.6 **Decision2** / **InternDecision** / **Kev**（0.5B/0.6B，含 Fast 变体） / **Vela2**
- B2.7 **ClefText** / **ClefVision**（含图像预处理 + record encoder） / **ClefFlash**
- B2.8 **ShortReply** / **EmbeddingGemma2**（含音频）/ **SigLIP2** / **Qwen BPE tokenizer**
- 每个模型统一形态：`XxxManager` + `XxxModelStore` + `XxxTypes`（+ tokenizer / preprocessor）

### B3 Python 桥（`PublishedCoreMLManager`）
- B3.1 pinned manifest（`published-coreml-manifest.json`）+ 每文件 size/SHA-256 校验
- B3.2 只拉请求的 precision 包；缓存到 `~/Library/Application Support/FluidUse/Models/<repo>`
- B3.3 `uv sync --frozen --no-dev` 建隔离环境（自动装 Python 3.12），首次拉 PyTorch、之后 <1 s
- B3.4 worker 环境隔离（剥 `PYTHONPATH`/`PYTHONHOME`/`VIRTUAL_ENV`/`CONDA_PREFIX`…）
- B3.5 typed 请求：`SystemOneRequest`（`choice`/`score`/`noul`）、`constrained(context:fields:)`、`classify(text:labels:)`、`decide(state:question:)`
- B3.6 顺序与并发语义：保序、一次一个、按调用顺序排队
- B3.7 超时/取消：`startupTimeout` 600 s、`requestTimeout` 300 s，超时 SIGTERM→SIGKILL，之后 `.closed` 需重开会话
- B3.8 模型侧错误不杀会话（`.runtime`）；worker stderr 单独收（尾 16 KB）
- B3.9 per-model quirks（Kev 一次一问、Kai/Lex 释放再载、LFM 必须 `.all`、Jeff 独立 sigmoid）

### B4 上层应用 / 演示（30+ executable target）
- B4.1 排序/分流族：`SortAnything` / `SortDecisions` / `BookmarkSort` / `TopicSort` / `CodeSearch` / `ImageSort` / `AudioSearch`
- B4.2 游戏决策族：`Game2048` / `LayaTetris` / `LayaTetrisDemo` / `GLiClass2048Demo` / `Decision2048BenchDemo`
- B4.3 其他 demo：`GuardrailDemo` / `FrontDoorDemo` / `IssueTriageDemo` / `ShortReplyDemo` / `KevGuessWhoDemo`
- B4.4 `GLiClassServe`（本地服务形态）

### B5 基准与工具
- B5.1 `FluidUseOfficialBench` + `Tools/official-benchmarks/{kev,verdict,jeff,rlcd}/run.py` + `summarize.py` + `pin_upstream_calibration.py`
- B5.2 `Tools/showdown` — 教师采集 / 学生训练 / 量化 / CoreML 转换 / 对局 / footprint 测量
- B5.3 `Tools/doom/sauerkraut` — Doom 评测
- B5.4 `Tools/pin_published_coreml.py` — 换 Hub revision 后重生成 manifest
- B5.5 `Benchmarks.md` / `Documentation/OfficialBenchmarks.md` — 全套对照表与校准讨论

---

## 4. C 树：iPhone-use

**定位**：MCP（stdio）服务，让 Codex 通过 USB + WebDriverAgent 操作**真实 iPhone**；含安装引导、
技能包与实时屏幕 widget。17 个模型可用工具 + 2 个 widget 专用工具。

### C1 MCP 工具族
- C1.1 setup 组：`pua_doctor`（环境诊断）/ `pua_setup`（设备配置 + 后台安装/启动，返回可查询 job）/ `pua_ready`（`ready=true`）
- C1.2 observe 组：`pua_observe`（控件树/截图）/ `pua_find`（目标查询）
- C1.3 apps 组：`pua_apps`（app 标识 + 安装证据）/ `pua_launch_app`
- C1.4 action 组：`pua_tap` / `pua_swipe` / `pua_press_button`（含主屏幕）/ `pua_type_text`（Unicode，长文分段续传）/ `pua_wait`（有界等待）
- C1.5 compose 组：`pua_batch`（已知步骤打包成一次往返）/ `pua_scroll_find`（有界滚动查找）/ `pua_collect_list`（重叠读取 + 去重 + 返回覆盖边界）
- C1.6 misc：`pua_screen`（预览开关）/ `pua_metrics`（有界耗时统计）
- C1.7 widget 专用：`pua_screen_frame` / `pua_screen_action`（annotations 标 `idempotentHint` 等）
- C1.8 `structuredContent` 同时返回（既给模型文本也给 widget 结构化数据）

### C2 WDA 层（`server/wda_*.py`）
- C2.1 `wda_client` — HTTP session 复用、loopback 转发（默认端口 18100）、拒绝远程地址
- C2.2 `wda_controller`（915 行）— tap/swipe/type/wait/scroll/collect 的动作语义
- C2.3 `wda_apps` — App 标识、安装证据、启动
- C2.4 `wda_text` — Unicode 与长文本分段
- C2.5 `wda_screen`（498 行）— 屏幕流与帧
- C2.6 `wda_image` — `image.pixel_to_point` 把截图像素转 iPhone 点坐标
- C2.7 `wda_setup`（918 行）— 诊断 / 设备发现 / 签名配置 / 拉固定版本 WDA(16.14.0) / 后台构建 / 启动；异步 job 可查询、可复用

### C3 观察预算（省 token 的核心设计）
- C3.1 紧凑控件树：省略重复字段
- C3.2 只要截图时不生成 XML
- C3.3 有界滚动查找（一次最多滑一次）
- C3.4 列表采集的重叠读取 + 去重 + 覆盖边界回报
- C3.5 已有截图直接复用，不自动续盲滑

### C4 屏幕 widget（`ui/`，TS + build.mjs）
- C4.1 iPhone 外壳 + 设备状态 + 连接/暂停状态
- C4.2 操作光效、点击/拖动提示
- C4.3 刷新 / 主屏幕 / 截图按钮；无图像时黑屏 + 状态图标
- C4.4 锁屏与「主动暂停」区分；解锁后 READY 可恢复锁屏预览
- C4.5 同聊天复用 widget，屏幕流不落盘

### C5 失败恢复与并发语义
- C5.1 截图回退：控件定位失败 → 返回截图 + 处理指引
- C5.2 多进程共享操作锁
- C5.3 不确定语义：超时/断线可能标 uncertain，先读实际状态，不盲目重放
- C5.4 认证（密码/验证码/Face ID）交给人

### C6 安装 / 打包 / 技能 / 工程
- C6.1 `scripts/install.sh` — 校验并暂存源码、注册本地 marketplace、装插件与技能、注册标准 MCP `iphone_use`
- C6.2 `register_mcp.py` / `smoke_mcp.py` / `package.py`（出自包含屏幕 HTML 的源码包）/ `check.sh`
- C6.3 技能包：`iphone-use`（8 个 reference：authentication/airdrop/verification/screen/startup/tool-fallback/apps/apps.json）+ `iphone-use-setup`（troubleshooting/recovery）
- C6.4 状态目录 `~/.local/share/iphone-use/`（`IPHONE_USE_STATE_DIR` 可改，兼容旧 `WDA_STATE_DIR`），权限 700/600
- C6.5 匿名 analytics（PostHog 独立项目，随机安装 ID，可 `IPHONE_USE_ANALYTICS=0`/`DO_NOT_TRACK=1` 关闭）
- C6.6 35 个测试文件（setup / protocol / latency / recovery / observation_budget / screen / apps / fallback / registration / packaging / analytics / benchmark / controller / state_directory …）+ `predicate_probe.m`
- C6.7 `THIRD_PARTY_NOTICES.md`（Appium / WebDriverAgent / appium-ios-device）

---

## 5. 逐功能映射

处置图例：**搬** = 在 phonefarm 里重建该功能；**借** = 借工程模式/协议约定；**等价** = phonefarm 已有同类能力；
**不适用** = 平台特有或与硬边界冲突（说明原因）。

### 5.1 SimRun → phonefarm

| 编号 | 上游功能 | phonefarm 现状 | 处置 | 落点 / 说明 |
| --- | --- | --- | --- | --- |
| A2 全部 | CLI over 常驻 socket | 每命令起一次进程；`serve.rs` 只有 MCP stdio | 借 | 「一个常驻 server + 多客户端命令」的连接复用模型可参考；不改现有 CLI 语义 |
| A2.11 | `simrun skill install [--claude]` | 无 | 搬 | phonefarm 也可分发自己的 agent skill 到 `~/.agents/skills`，并 `--claude` 链到 Claude Code |
| A3.1/A3.4 | launcher UI + Launch Options | 无 GUI | 不适用 / 借 | UI 不搬；「按 app+profile 存启动选项并按 profile 复用数据容器」的概念可借给 `tasks/` 与 `--profile` |
| A3.3 | device build → simulator 转换 | 无 | 不适用 | iOS Mach-O `LC_BUILD_VERSION` 改写，纯 iOS 概念 |
| A4.5 | 触控板手势 → 真多点触控 | `gamepad.rs` + adb/hdc input | 搬（概念） | 多点触控序列的表达与注入，可对齐 phonefarm 的输入脚本契约 |
| A4.9 | 明文假 keychain | 无 | 不适用 | Android 有真 keystore |
| A4.10 | WKWebView helper 进程托管 | 无 | 不适用 | 平台特有 |
| A4.11 | MPSGraph 模拟器 bug 修补 | `gpustat.rs` / `gpuop.rs`（真机 GPU 状态与算子标尺） | 借 | 「为让图跑上 GPU 而做的 device descriptor / heap 修补」的思路可作为真机 GPU 兜底经验对照 |
| A4.12 + A2.5 | `script.dylib` + `simrun eval`（JS + 无障碍树） | VLM 通路（烧 token）+ `script.rs`（零 token 确定性回放） | 搬 | 这是 SimRun 对 phonefarm 最有价值的一条：**零 token 的结构化观测 + 脚本化注入**，正好补 phonefarm 现在只有「VLM」和「死板回放」两极的中间层 |
| A5 | `simns`（launchd_sim 替代） | 无 | 不适用 | XPC pipe 协议是 iOS sim 专有 |
| A6.1–A6.5 | 设备/runtime/profile/环境变量/快捷键 | `platform::Backend` + `phonefarm.toml` | 等价 | phonefarm 的 serial 前缀与 toml 配置覆盖同类需求 |
| A7.1–A7.4 | headless + 私有 pasteboard + 并行隔离 | `parallel.rs`（并行任务）+ `devlock.rs`（设备锁） | 借 | 「并行实例之间互不污染（剪贴板/焦点/面板）」的隔离原则可写进 parallel 的契约 |
| A8 | 图层树 live 拷贝 + diff 对账 | `frameaudit.rs` / `framecheck.rs` / `vksreport.rs` | 等价（更强） | phonefarm 已有帧审计与判据收口；SimRun 的 `--check` 对账思路可对齐「证据分级」 |
| A9 | 20 条已知缺口 | — | 借 | 作为「能力边界要如实写明」的写作范例 |

**SimRun 小结**：真正值得移植的是 **A4.12/A2.5（脚本 + 无障碍树，零 token 观测/注入）** 和
**A2.11（agent skill 分发）**；其余大多是 iOS 模拟器专有机制，与 phonefarm 的硬边界无关。
若目标真是「让 phonefarm 也能驱动 iOS 模拟器」，那是**新增一个 `platform::Backend`（`sim:` 前缀）**，
把 SimRun 当外部依赖调用而不是复制代码——这是另一个量级的工程，需要单独立项。

### 5.2 FluidUse → phonefarm

| 编号 | 上游功能 | phonefarm 现状 | 处置 | 落点 / 说明 |
| --- | --- | --- | --- | --- |
| B1 全族 | AX 表单填写 / WebView 表单 / 文档→entity | VLM 通路做 UI 遍历 | 不适用 | macOS AX 与 Android 无障碍树是两套东西；但「先把界面压成结构化候选，再让模型做选择」的**两段式**完全可借 |
| B2.1 Laya | Jev 风格 typed decisions（choice/score/noul） | `llm.rs` 云端 LLM 选参数 + 本地变异器降级 | 搬 | 正是用户要的「把最新出来的 jev 用上」：把「选下一组参数」从云端 LLM 换成**本地决策模型**，或作为云端失败时的降级层（`llm.rs` 已声明「失败必须可降级」） |
| B2.2 GLiNER2 | 文本分类/抽取，≤8 标签 | 无 | 搬 | 可用于 phonefarm 的记录/日志**分流与打标**（对应现有 `jev-triage` 技能场景） |
| B2.3 Verdict | 校准分数 + 弃答 | `loopstat.rs` 统计判定（p 值） | 借 | 「校准 + 明确弃答」的语义与 phonefarm「有界失败、绝不静默零记」同源，可对齐判定输出格式 |
| B2.5 D3 / B2.6 Decision2·Kev·Vela2 | 更多 sub-1B 决策模型 | 无 | 搬（按需） | 同一运行时下多模型可选；先只搬 1–2 个验证通路 |
| B2.7 ClefVision / SigLIP2 | 图像编码 + 分类 | VLM 通路看图 | 搬（长期） | 本地小视觉模型替代云端 VLM 做「屏幕是否到达目标态」的判据 |
| B2.8 ShortReply 等 | 短回复/embedding | 无 | 不适用 | 与 harness 目标无关 |
| B3.1/B3.2 | pinned manifest + SHA-256 + 按需下载 | 无 | 搬 | 模型/资产必须**可校验、可复现**；phonefarm 的 `phonefarm.toml` + `docs/GOLDEN_RULES.md` 正好需要这个纪律 |
| B3.3/B3.4 | `uv --frozen` 隔离环境 | `pyjson.rs` / `pyrandom.rs`（逐位复刻 CPython） | 借 | phonefarm 已在用「Python 语义逐位复刻」保证可复现，FluidUse 的「环境隔离 + 锁文件」是同一目标的另一层 |
| B3.6/B3.7 | 保序 + 超时/取消 + 明确 `.closed` | `brain.rs` failover + timeout | 借 | 会话生命周期语义可对齐 phonefarm 的 provider failover 契约 |
| B4 / B5 | 30+ demo / 基准套件 | `refbench` / `vks` / `loopreport` | 不适用 | 目标领域不同；**基准方法论**（多套件、参考行、逐行对账）可借 |

**FluidUse 小结**：价值集中在 **B2（本地决策模型运行时）** 与 **B3（可复现资产管线）**。
但它是 Swift/Core ML，phonefarm 是 Rust——**不能直接搬代码**，只能：
1. 作为**外部 sidecar**（Swift CLI/服务）由 phonefarm 调用；或
2. 在 Rust 侧用同等模型（ONNX/CoreML via `coreml` crate）重建。
两条路都要先把「`llm.rs` 的候选生成 → 本地决策模型」这条接口定下来。

### 5.3 iPhone-use → phonefarm

| 编号 | 上游功能 | phonefarm 现状 | 处置 | 落点 / 说明 |
| --- | --- | --- | --- | --- |
| C1.1 | `pua_doctor`/`pua_setup`/`pua_ready` 引导流程 | `onboard.rs`（收尾核对）+ `selftest.rs` | 借 | 「环境诊断 → 异步安装 job（可查询/可复用）→ ready 判据」三段式，比 phonefarm 现在的一次性 onboard 更稳 |
| C1.2/C1.3 | observe/find/apps/launch | VLM 通路 `run` + `probe`/`exec` | 等价 | 能力已有，接口形态不同 |
| C1.4 | tap/swipe/press/type/wait | `script.rs` 动作原语 | 等价 | — |
| C1.5 | `pua_batch` / `pua_scroll_find` / `pua_collect_list` | 无对应 | **搬** | 这是省 token 的关键：把「已知的多步」压成一次往返、把「滚动查找/列表采集」变成一个有界原语并回报覆盖边界 |
| C1.7/C1.8 | widget 专用工具 + `structuredContent` | `serve.rs` 只回 `content:[{type:text}]` | 搬 | MCP 返回结构化内容（`structuredContent`）是 phonefarm 看板/并行场景需要的 |
| C2.1–C2.7 | 整套 WDA 客户端与 setup | **无 iOS 后端** | **搬（新 Backend）** | 新增 `platform::Ios`（serial 前缀 `ios:`），实现 `Backend` trait：连 USB、转发、session、截图、输入。这是「让 phonefarm 支持 iPhone」最直接的一条路 |
| C3.1–C3.5 | **观察预算**（紧凑树/去重/不产 XML/有界滚动/复用截图） | VLM 通路的观测契约（未成文） | **搬** | 直接对抗 phonefarm 最大的成本项（token）；应写成 `docs/SPEC_*` 级别的契约 |
| C4.1–C4.5 | 实时屏幕 widget（TS，自包含 HTML） | 看板网页（用户已提） | 搬 | 屏幕流 + 操作光效 + 暂停/恢复，是「看板」最缺的实况层 |
| C5.1 | 截图回退 | 无 | **搬** | 控件定位失败 → 回截图 + 让模型判断，与 phonefarm「如实标注、不静默零记」一致 |
| C5.2 | 多进程共享操作锁 | `devlock.rs`（设备锁 + `$DEVLOCK_LABEL`） | 等价 | phonefarm 的设备锁更完整（含只读降级档） |
| C5.3 | 不确定语义（不盲目重放） | 记录契约里的 `NOT_RUN`/`ENV_BLOCKED` | 等价（同源） | 可互相引用 |
| C5.4 | 认证交给人 | `human-console` | 等价 | — |
| C6.1–C6.2 | 一键安装 + 自包含打包 | `bin/` + `docs/DEVICES.md` | 借 | 安装引导与自包含发行包可借 |
| C6.3 | 两套技能包 + 8 个 reference | phonefarm 的 `skills/` | 借 | 技能包分层（使用 / 安装排障）值得学 |
| C6.5 | 匿名 analytics（可关） | 无 | 不适用 | phonefarm 是企业项目，是否引入埋点应由公司决定 |
| C6.6 | 35 个测试（含 observation_budget / latency） | `src/testdata` + 自测 | 借 | 「观测预算」与「延迟」有专门测试，值得对齐 |

**iPhone-use 小结**：这是三个上游里与 phonefarm **同域度最高**的一个（设备自动化 + MCP + 省 token 观测）。
最有价值的三条：**C2 新增 `ios:` 后端**、**C3 观察预算契约**、**C1.5 组合动作原语**。

---

## 6. 建议的移植批次

按「价值 / 与硬边界冲突程度 / 可验证性」排序：

- **批次 0（先做，纯文档）**：把本文件转成正式设计稿，加 `THIRD_PARTY_NOTICES.md`，明确署名与边界。
- **批次 1（零风险，收益高）**：C3 观察预算契约 + C1.5 组合动作原语 + C1.8 `structuredContent`。
  —— 直接降 token，不碰判定口径，可独立测试。
- **批次 2（新能力）**：C2 `platform::Ios` 后端（WDA over USB），先只实现 `devices`/`capture`/`script` 三条零 token 路径。
- **批次 3（本地模型）**：B2/B3 —— 以 sidecar 形式接 Laya 做「降级决策器」，不进判定层。
- **批次 4（看板）**：C4 屏幕 widget 接进看板网页。
- **暂不做**：A 树全部（除 A4.12/A2.11 外均为 iOS 模拟器专有）；B1 表单填写；B4/B5 演示与基准。

---

## 7. 需要确认的三件事

1. **范围**：「把三个上游都搬空」与「只搬与 phonefarm 同域的部分」差一个数量级。上面批次 1–4 是后者。
2. **语言边界**：FluidUse 是 Swift/Core ML。走 sidecar 还是 Rust 重建？sidecar 快但引入 Swift 构建；重建干净但工作量大。
3. **判定层红线**：`AGENTS.md` 写明「判定口径与阈值只能在本仓库定，且在看到候选数据之前冻结」。
   搬到判定层的任何东西（B2.3 Verdict 等）都必须守这条，否则整套证据失去意义。
