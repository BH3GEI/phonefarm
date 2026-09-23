# phonefarm

设备自动化与实测基础设施：一个 Rust 内核，两种设备后端（Android `adb` / OpenHarmony `hdc`），
在同一套「设备抽象 + 记录契约 + 遥测 + 证据分级」之上，并列提供多条互不依赖的上层能力。

多模态视觉模型（VLM）只是其中一条上层通路，不是这个项目的定义。
`script` / `test-batch` / `cts-fetch` / `bench` / `capture` / `keepalive` 全程零 Token，不碰模型也能独立工作。
宿主架构实现执行回路与决策机制分离：模型只做屏幕理解与动作规划，状态采集、规则拦截、
校验、复盘与系统监控全部由 Rust 宿主进程控制。

## 在整体里的位置（改这个仓库之前先看这一段）

整套东西是一个 harness。用户只提一种任务——「把某个游戏在某台手机上优化一下」，
内部自动决定用哪几层。本仓库是底座那一层，四件事：

- 设备：adb / hdc 两族的连接、保活、确定性脚本回放。
- 应用改动（`knobs/`）：黑档系统参数，灰档 Vulkan layer 注入。
- 测量：安卓 sysfs 电源轨加 raw ftrace kgsl，鸿蒙 HiSmartPerf，上层拿同一份 JSON；
  测功耗期间停充。
- 统计判定与还原：规则先于数据冻结，不显著就回退，退出前恢复现场。

提改动的是上层（`game_opt_loop` 的决策层，或者人），判定这一侧留在本仓库。
这条边界是硬的：**判定口径与阈值只能在本仓库里定，且在看到候选数据之前冻结**。
上层可以随请求带参数（比如 `incumbent_latency_ms` 说明现任冠军的耗时），
但那是被判定的输入，不是判定规则本身——出主意的一方不能改判定，否则整套证据失去意义。

优化分三个层级：① 游戏代码（需要白盒）② 图形接口 Vulkan 层 ③ 系统参数。
闭源商用游戏只剩 ②③。鸿蒙商用机在没有 root 的情况下只能测不能改：HiSmartPerf 采得到
帧率、帧时、温度、功耗，但 ② 和 ③ 都动不了。可能的出路有工程机、官方性能接口、
开发者模式下自签名应用三条，都还没核实过，不要当成现成能力写进文档或代码注释。

测试载体在 `loop_v1/carriers/`（Megacity / Vulkan-Samples / AnKi），
白盒靶子在同级的 `refbench` 仓库。`sr_loop` 是已结项的前序实验，不在当前主线上。

**进行中，别写成已完成**：`loop_v1/` 的判定口径（解析→归因→统计→判据收口→白名单→判定→
画面判据→模型交互/本地变异器）、两通路对照、Vulkan-Samples 三道闸与两臂判读面、refbench/vks
报告都已经收进 Rust，剩下 `autoloop.py`（编排与设备驱动）、载体构建/路线工具、
设备端 shell 与 `knobs/` 那批还没收。上层的 `game_opt_loop` 那边，`optimize` 总入口、agent skill 与
可视化前端都已经落地，还差的只是系统参数与画面流程改写两层的真机评测——`eval --request`
入口已经落地（离线验证，真机未跑），首次真机跑按 `probe_only` 起步。写目标形态时要标明状态。

## 能力族与常用命令

```bash
# 编译构建（Apple Silicon 必须重新签名，见「安全边界」第 7 条）
cd src && cargo build --release && cp target/release/phonefarm .. && cd .. && codesign --force --sign - ./phonefarm
```

**设备与农场运维**（零 Token）

```bash
./phonefarm devices                        # adb 与 hdc 两族设备一并列出
./phonefarm keepalive [--status|--watch]   # 农场级保活：唤醒 + 解锁 + 不息屏
./phonefarm fleet [--json]                 # 农场只读快照：每台手机的在线/型号/电量/温度/风扇/
                                           # 前台应用/保活策略 + 设备锁。别人持锁时自动降成
                                           # 只读 sysfs 的轻量档，不跑 dumpsys
./phonefarm probe --serial S "只读命令"     # 只读调试通道
./phonefarm exec --serial S "命令" --yes    # 写操作通道，高危，必须显式 --yes
```

**自主 UI 遍历**（VLM 通路，**烧 Token**）

```bash
./phonefarm run --task 今日头条遍历 --endless --budget-calls 90 --app com.ss.android.article.news "<目标>"
./phonefarm benchmark --task 今日头条遍历 --rounds 10 --budget-calls 90 --app <pkg> --json "<目标>"
./phonefarm run --serial hdc:<serial_id> --task OH设置冒烟 --budget-calls 30 "<目标>"
./phonefarm parallel --job "任务A|目标A|emulator-5554|com.pkg" --job "任务B|目标B|hdc:<key>"
./phonefarm plugins                        # 列出场景插件
./phonefarm quest --mode auto --sec 1800   # 原神插件的独立长跑 Agent
```

**确定性脚本与回放**（零 Token，同一套遥测）

```bash
./phonefarm script --task 游戏压测 --app <pkg> --repeat 10 examples/sample_game_benchmark.json
./phonefarm script --task 轨迹重放 20260831-213215      # 原样回放历史对局
./phonefarm script --detach ...                        # 后台跑，立即回报局 ID
```

**一致性测试 harness（CTS / XTS）**（零 Token，Android/OH 双协议）

```bash
# 批量执行：profile / 模块 / APK 目录三种入口
./phonefarm test-batch --profile P.json --environment E.json --serial S --out 结果目录
./phonefarm test-batch --module android.content.cts/androidx.test.runner.AndroidJUnitRunner
./phonefarm test-batch --module oh:com.example.demo/entry_test/OpenHarmonyTestRunner --serial hdc:<key>
./phonefarm test-batch --dir /path/to/apks --install-cmd 'pm install -r {apk}'
#   --include/--exclude 正则  --resume 断点续跑  --retry N  --timeout-ms/--idle-timeout-ms 双重看门狗
#   --heal-script 自愈脚本    --detach 后台跑，轮询 <out>/summary.json

# 结果提取：结果已由别的工具落在设备上时，不重跑，只拉回来扫断言
./phonefarm cts-fetch --remote /data/local/tmp/cts_result --serial hdc:<key> --out 本地目录
#   --pattern 正则  自定义断言行匹配     --max-mb N  单文件上限(缺省16)，超限与二进制跳过并如实标注
```

产出 `summary.json`（逐用例判定 + 完整断言栈）与 JUnit XML，**不必再 hdc 进机器翻日志**。
跳过/假设失败不计为通过；崩溃、看门狗超时、runner 报错都会把未交代的用例如实标成
`NOT_RUN` / `ENV_BLOCKED` 并对账，绝不静默零记。

**端侧模型标尺与采集**（零 Token，需 root）

```bash
./phonefarm bench --serial S --model m.tflite --runs 3 --json   # 锁频+等冷+GPU Delegate，退出码 0/1/2
./phonefarm bench --serial S --unlock                           # 回滚遗留锁频态
./phonefarm capture --serial S --out 目录 --frames 200 [--ready-only] [--json]
```

**Compute Shader 算子标尺**（零 Token，需 root）

```bash
./phonefarm eval --request <eval_request.json> --serial S --json      # 评测唯一入口: v1 转交 gpu-op; v2 按 kind 路由 shader/sysparam/gray
./phonefarm gpu-op --request <eval_request.json> --serial S --json   # shader 通路的实现本体 (等冷+锁频+A/B/A/B+Welch t 检验)
./phonefarm gpu-op --serial S --unlock                               # 回滚遗留锁频态
```

`game_opt_loop` 决策层的物理采样与统计裁决端，契约见 `docs/SPEC_GPU_OP.md`。
设备条件化与功耗遥测在 `src/hwcond.rs`（与 `bench` 共享），统计在 `src/gpustat.rs`。
设备侧 `vkop_runner` 已构建（`tools/vkop/android_aarch64_vkop_runner`，源码与 `build.sh` 同目录）。
画质补测支持真机参考帧集，回包里 `quality_reference` 记清真值出处。

**性能优化闭环**（零 Token，需 root）

实现在 `loop_v1/`（工具链 + 纯函数解析器），**不是 phonefarm 子命令**。
跑法与五条判据见 `loop_v1/README.md`；候选改动从 `docs/MOBILE_GPU_OPT_ROUTES.md` 的清单里挑。

三条纪律，动手前必须清楚：

- **判定规则先于数据冻结**——统计判据在看到候选数据之前落盘，不达标自动回退且不留痕
- **sysfs 写入退出前全部恢复**，进出设备快照逐行相等才算这轮成立
- **解析全是纯函数**，同一份 trace 每次重算必须逐字节一致

帧时序走 raw ftrace 的 kgsl 事件（不侵入游戏进程）。`SurfaceFlinger --latency`、
`gfxinfo`、Perfetto GPU producer 三条路都已在本机实测排除，别再重复试，原因见 `loop_v1/README.md`。

**假设—实验—证据闭环**

```bash
./phonefarm hyp [--retract id|--supersede id]     # 竞争解释与人工裁决
./phonefarm pred                                   # 预测台账（登记在先）
./phonefarm caps [--adopt id|--rollback id]        # 能力候选固化与回滚
./phonefarm tools [--propose def.json|--retire id] # 测量工具提案/校准/退役
./phonefarm experiment <spec.toml> [--arm A|B|C] [--ablate ...] [--resume|--report-only] [--json]
./phonefarm export [--task T] --split train|heldout --out <文件>
./phonefarm eval --set <evalset.toml>              # 模型评估接口（骨架）
```

**只读下钻**（零 Token，全部离线解析本地产物；均支持 `--json`）

```bash
./phonefarm last                                  # 最近一局结论与性能概要
./phonefarm runs [--task T] | status [<局ID>|--task T]
./phonefarm show <局ID> [--step N]                # 局概要 / 步骤级上下文与遥测快照
./phonefarm show <局ID> --raw|--hooks|--events|--crashes|--anr|--trace
./phonefarm stats <局ID>                          # 局级遥测统计
./phonefarm cat <路径> [--head/--tail N] [--grep 词]  # gz 解压、JSONL 美化、图片查看
./phonefarm schema [--type r类型]                 # log.jsonl 完备记录 schema
./phonefarm tasks | tree | lessons | campaign | config
```

**MCP 工具服务**

```bash
./phonefarm serve [--root 目录]    # 21 个 phonefarm_* 工具（17 只读 + 4 执行），stdio JSON-RPC 2.0
```

## 安全边界与执行规范

1. **财务约束**：只有 VLM 通路（`run`/`benchmark`/`parallel` 及 `[evolution]` 配额）会产生真实 API Token 消耗。
   执行真机测试、大规模并行或高并发压测前，必须向用户汇报执行预算，经明确授权后方可运行。
   单测、CLI 数据解析、本地离线编译，以及 `script`/`test-batch`/`cts-fetch`/`bench`/`capture`/`keepalive`
   这些零 Token 通路，可自由执行。
2. **密钥管理**：程序运行时自动检测并加载 `./secrets.env`。缺失密钥时提示格式并安全退出。
   严禁在代码、日志及任何可提交文件（`AGENTS.md`、`README.md` 等）中硬编码真实密钥。
3. **测试环境约束**：VLM 通路与 CTS 通路的 Android 侧统一使用 AVD 模拟器（AVD 名 `agentphone`）；
   OpenHarmony 需在特定的 intel-mac 物理中转端上通过 ssh 控制，连接真机前先核实该物理端
   是否完成本地构建与代码同步。性能测量必须上真机，模拟器的帧时与功耗没有意义。
4. **设备纪律**（性能测量相关）：
   - 同一个候选的 A/B 两臂必须落在**同一台**手机上。跨机比较会把机间差异算进改动的效果里。
   - 设备是共享资源，跑之前用设备锁 `devlock` 抢占（拿锁 → 后台心跳续期 → 跑完释放）。
     被别人占着时排队，不要绕过去硬跑——两个任务同时压一台手机，两边的数都作废。
   - 影响稳态的环境条件（风扇开关、冷机阈值、供电状态）逐轮记进证据。这些不是无关细节，
     条件不同的两批数据不能混在一起比。
   - 规划中的设备构成：安卓旗舰机（一台跑测试、一台留给调试，要并行跑多条线得是同型号多台）、
     鸿蒙手机 1 到 2 台做对照（无 root，只能测）、海外版华为手机几台（EMUI，麒麟芯片，能 root，
     用来在麒麟上测 Vulkan 层与系统参数的改动效果）。这是计划不是现状，
     不要按「已有这些机器」来写流程。
5. **数据持久化规范**：程序唯一合法写入路径为 `tasks/<任务名>/`。`log.jsonl` 只追加；
   `lessons.jsonl` 必须原子写；禁止自行清理 `runs/` 历史目录；媒体与树结构大文件已由 `.gitignore`
   排除，严禁提交大体积非文本文件。`test-batch`/`cts-fetch`/`bench`/`capture` 的产物写入各自 `--out` 目录。
6. **代码稳定性**：任何逻辑或代码变更后，必须在本地运行 `cd src && cargo test`，确保单测 100% 通过。
7. **构建后必须重新签名（Apple Silicon 硬性要求）**：`[profile.release] strip = true` 会在链接后剥离符号，
   使 ad-hoc 代码签名失效；失效的二进制在 macOS 上不报错，而是**静默卡死在 `_dyld_start`**
   （进程存活、CPU 为零、无输出），极易被误判为死循环或设备失联。跑任何实机测试前，
   先确认用的是当前源码构建并已签名的二进制。
8. **缺陷修复规范**：新缺陷修复遵循项目编号管理（从 #20 起递增）。修复必须保证通用性，
   严禁针对特定任务或界面编写硬编码拦截逻辑。完成后用今日头条断言局做标准回归。
9. **版本控制与提交**：`push` 权限属于用户。完成本地提交后，把提交哈希与变更概要呈报用户，
   由用户决定推送。部署时用 `mv` 做原子产物替换，防止进程踩踏。
10. **汇报规范**：呈报进度或结果时给最直接的技术事实、运行指标与对局结论（成功率、耗时分位数、
    Token 支出），禁止含糊夸张的非技术词汇，严禁在文档中堆砌虚假的性能或战绩宣称。
    计划中的能力写成「计划」，不写成「已完成」；撤回过的结论不要再引用。

## 架构：不变内核 + 并列上层

```
 run/benchmark/quest   script   test-batch/cts-fetch   bench   capture   loop_v1   experiment
          │              │              │                │        │         │          │
          └──────────────┴──────────────┴────────────────┴────────┴─────────┴──────────┘
                                          │
              ┌───────────────────────────┴────────────────────────────┐
              │ 不变内核：设备抽象(adb/hdc) · 记录契约 · 遥测           │
              │           确定性纪律(三道检查/双重看门狗) · 证据分级     │
              └────────────────────────────────────────────────────────┘
```

上层各通路互不依赖：卸掉 VLM 通路，`script`/`test-batch`/`bench` 照常工作；反之亦然。
**新增一条上层通路不应该修改内核。**

VLM 通路内部再分两层：核心层（`src/universal/`、`runtime.rs`、`device.rs`）只做通用能力，
不含任何具体应用的包名、文案或流程假设；插件层（`src/plugins/`）承载全部专用场景。
若某需求迫使你改核心去迁就单一应用，那是设计错了，应改为插件。

## 目录结构

```
loop_v1/               性能优化闭环：采集/归因/旋钮/统计判定/回滚/证据归档（Python 工具链，正在往 Rust 收）
  carriers/            测试载体：megacity / vulkan-samples / anki-sponza
  refbench/            白盒靶子的驱动侧（靶子本身在同级 refbench 仓库）
  auto/                自动挑旋钮：白名单探测 → 冻结规则 → 大模型选参 → 逐组对照
knobs/                 可改动面：黑档系统参数 + 灰档 Vulkan layer 注入（由 loop_v1 驱动，
                       说明见 knobs/README.md；原 HGamey/knobs 仓库已归档并入，shell 正在往 Rust 收）
src/                   Rust 内核源码
  universal/           通用核心：统一动作协议、三大算子、优先级引擎、插件契约
  plugins/             场景插件层
  cts.rs               一致性测试 harness（双协议解析、对账、报告、结果提取）
  bench.rs capture.rs  端侧模型标尺 / 数据采集管线
  gpuop.rs gpustat.rs  Compute Shader 算子标尺 / A-B 冻结判定统计
  hwcond.rs            共享的设备条件化（等冷/锁频/还原）与功耗遥测
  perfsrc.rs smartperf.rs  性能数据来源抽象：安卓 sysfs 与鸿蒙 HiSmartPerf(SP_daemon/Xpower) 同一份 JSON
  hypo.rs caps.rs mtools.rs experiment.rs   假设—实验—证据闭环
  keepalive.rs serve.rs script.rs parallel.rs telemetry.rs device.rs
phonefarm.toml         控制参数、规则阈值及模型 Provider 回退链配置
docs/                  架构契约与各能力规格（索引见 README「文档索引」）
skills/                给 AI Agent 直接阅读的英文说明书（phonefarm / phonefarm-cts / a2oh-diagnose）
examples/              示例脚本与 CTS profile 样例
round.sh               单轮调度外壳脚本（保留兼容，生产建议用 benchmark）
ocr.swift / ocr        OCR 辅助识别模块
tasks/<任务名>/        任务数据目录（lessons.jsonl、tree.json、campaign.tsv、runs/<局ID>/）
```

## 下钻分析流程

- 排查首选 `phonefarm last`；定位异常局 ID 后 `show --step N` 看具体步骤上下文；
  `cat ...stepN.xml.gz` 查原始 UI 树做离线解析与断言复盘。
- 局 ID 支持前缀模糊匹配。
- VLM 通路严格遵守单步六阶段回路（采集 → 组装 → 决策 → 校验 → 执行 → 验收）。
- 核心架构契约见 `docs/DESIGN.md`；任何核心功能升级或协议重写，必须先做 SPEC 定义，并与既有架构保持一致。
