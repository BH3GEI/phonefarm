# phonefarm

**设备自动化与实测基础设施**：一个 Rust 内核，两种设备后端（Android `adb` / OpenHarmony `hdc`），
在同一套「设备抽象 + 记录契约 + 遥测 + 证据分级」之上，并列提供多条互不依赖的上层能力。

多模态视觉模型（VLM）只是其中**一条**上层通路，不是这个项目的定义。
`script` / `test-batch` / `cts-fetch` / `bench` / `capture` / `keepalive` 全程零 Token，不碰模型也能独立工作。
宿主架构实现执行回路与决策机制分离：模型只做屏幕理解与动作规划，状态采集、规则拦截、
校验、复盘与系统监控全部由 Rust 宿主进程控制。

## 能力族与常用命令

```bash
# 编译构建（Apple Silicon 必须重新签名，见「安全边界」第 6 条）
cd src && cargo build --release && cp target/release/phonefarm .. && cd .. && codesign --force --sign - ./phonefarm
```

**设备与农场运维**（零 Token）

```bash
./phonefarm devices                        # adb 与 hdc 两族设备一并列出
./phonefarm keepalive [--status|--watch]   # 农场级保活：唤醒 + 解锁 + 不息屏
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
./phonefarm gpu-op --request <eval_request.json> --serial S --json   # 等冷+锁频+A/B/A/B+Welch t 检验
./phonefarm gpu-op --serial S --unlock                               # 回滚遗留锁频态
```

`game_opt_loop` 自主进化闭环的物理采样与统计裁决端，契约见 `docs/SPEC_GPU_OP.md`。
设备条件化与功耗遥测提在 `src/hwcond.rs`（与 `bench` 共享），统计在 `src/gpustat.rs`。
**当前阻塞**：设备侧 `vkop_runner` 需要 Android NDK 构建，尚未就绪。

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
3. **测试环境约束**：Android 统一使用 AVD 模拟器（AVD 名 `agentphone`）；OpenHarmony 需在特定的
   intel-mac 物理中转端上通过 ssh 控制，连接真机前先核实该物理端是否完成本地构建与代码同步。
4. **数据持久化规范**：程序唯一合法写入路径为 `tasks/<任务名>/`。`log.jsonl` 只追加；
   `lessons.jsonl` 必须原子写；禁止自行清理 `runs/` 历史目录；媒体与树结构大文件已由 `.gitignore`
   排除，严禁提交大体积非文本文件。`test-batch`/`cts-fetch`/`bench`/`capture` 的产物写入各自 `--out` 目录。
5. **代码稳定性**：任何逻辑或代码变更后，必须在本地运行 `cd src && cargo test`，确保单测 100% 通过。
6. **构建后必须重新签名（Apple Silicon 硬性要求）**：`[profile.release] strip = true` 会在链接后剥离符号，
   使 ad-hoc 代码签名失效；失效的二进制在 macOS 上不报错，而是**静默卡死在 `_dyld_start`**
   （进程存活、CPU 为零、无输出），极易被误判为死循环或设备失联。跑任何实机测试前，
   先确认用的是当前源码构建并已签名的二进制。
7. **缺陷修复规范**：新缺陷修复遵循项目编号管理（从 #20 起递增）。修复必须保证通用性，
   严禁针对特定任务或界面编写硬编码拦截逻辑。完成后用今日头条断言局做标准回归。
8. **版本控制与提交**：`push` 权限属于用户。完成本地提交后，把提交哈希与变更概要呈报用户，
   由用户决定推送。部署时用 `mv` 做原子产物替换，防止进程踩踏。
9. **汇报规范**：呈报进度或结果时给最直接的技术事实、运行指标与对局结论（成功率、耗时分位数、
   Token 支出），禁止含糊夸张的非技术词汇，严禁在文档中堆砌虚假的性能或战绩宣称。

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
loop_v1/               性能优化闭环：采集/归因/旋钮/统计判定/回滚/证据归档（非 Rust，独立工具链）
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
