# phonefarm

设备自动化与实测基础设施：一个 Rust 内核，三种设备后端（Android `adb` / OpenHarmony `hdc` /
iOS `wda`），
在同一套「设备抽象 + 记录契约 + 遥测 + 证据分级」之上，并列提供多条互不依赖的上层能力。

多模态视觉模型（VLM）只是其中一条上层通路，不是这个项目的定义。
`script` / `test-batch` / `cts-fetch` / `bench` / `capture` / `collect` / `keepalive` 全程零 Token，不碰模型也能独立工作。
宿主架构实现执行回路与决策机制分离：模型只做屏幕理解与动作规划，状态采集、规则拦截、
校验、复盘与系统监控全部由 Rust 宿主进程控制。

## 在整体里的位置（改这个仓库之前先看这一段）

phonefarm 是移动端 / 鸿蒙设备的**设备层**：在一台宿主机上发现设备、操作设备、采集产物、
执行测试，并用本机设备锁保证同一台设备不被两个任务同时使用。它不做跨宿主调度，
也不替上层做业务判断。典型调用方：

- 测试与迁移 harness（例如 Westlake / AOSP→OH harness 的 D600 开发板集群）：
  上层负责板号分配、跨宿主板锁与任务派发，phonefarm 负责单台宿主上的设备操作与采集。
  接入说明见 `docs/D600_HARNESS.md`。
- AI Agent：通过 `serve`（MCP）或 `skills/phonefarm/SKILL.md` 直接驱动设备。
- 人：命令行直接用，产物全部落盘，可离线下钻。

当前能力分四类：

- 设备：adb / hdc / iOS(WebDriverAgent) 三族的发现、连接、保活、确定性脚本回放。新增一种设备/平台 = 实现 `platform::Backend`
  并登记 serial 前缀，各平台差异见 `docs/DEVICES.md`。
- 采集：每步截图、原始 UI 树、`log.jsonl` 记录、遥测；`capture` / `collect` / `fleet` / `screen` / `perf`。
- 测试：CTS / XTS 批量执行与结果提取（Android / OpenHarmony 双协议）。
- 可选的图形性能研究通路：`knobs/`（系统参数与 Vulkan layer 注入）、`loop_v1/`、`eval` / `gpu-op` / `gray-*`。
  这一组工具服务于移动 GPU 能效研究（上游客户端之一是 HGamey/game_opt_loop），设备层不依赖它们。

能力声明按设备实例核验：鸿蒙商用机通常拿不到 root，系统参数与图形层改动不能照搬 Android；
OpenHarmony 开发板、不同系统版本上 `uitest` / `snapshot_display` / `SP_daemon` 的可用性和字段要逐项验证，
不从系统名或品牌推断支持。写文档时，计划中的能力写成「计划」，不写成「已完成」。

## 能力族与常用命令

```bash
# 编译构建（Apple Silicon 必须重新签名，见「安全边界」第 7 条）
cd src && cargo build --release && cp target/release/phonefarm .. && cd .. && codesign --force --sign - ./phonefarm
```

**设备与农场运维**（零 Token）

```bash
./phonefarm devices                        # adb / hdc / iOS 三族设备一并列出（iOS 只列 USB 上看得见的，不代表 WDA 已就绪）
./phonefarm keepalive [--status|--watch]   # 农场级保活：唤醒 + 解锁 + 不息屏
./phonefarm fleet [--json]                 # 农场只读快照：每台手机的在线/型号/电量/温度/风扇/
                                           # 前台应用/保活策略 + 设备锁。租约域必须明确 clear；
                                           # 持锁、恢复待处理、未知或未初始化时返回暂不可用。
                                           # 标签与 --probe-while-locked 不能绕过租约检查。
tools/devlock status                       # 设备锁：谁持有、谁在排队（碰设备的子命令自己拿锁）
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

CTS携带本地APK的安装目前仅支持Android默认或精确`pm install -r {apk}`模板；需要aapt/aapt2核身份、明确命令回执及实际安装字节。旧hash缓存不代表已安装。HAP/自定义安装adapter未实现，已安装OH模块和cts-fetch仍可用。未知安装仅保存证据/报错，尚无typed恢复屏障；不能把本地进程退出当远端已停止。合同见 https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/CTS-INSTALLATION.md 。

产出 `summary.json`（逐用例判定 + 完整断言栈）与 JUnit XML，**不必再 hdc 进机器翻日志**。
跳过/假设失败不计为通过；崩溃、看门狗超时、runner 报错都会把未交代的用例如实标成
`NOT_RUN` / `ENV_BLOCKED` 并对账，绝不静默零记。

**端侧模型标尺与采集**（零 Token，需 root）

```bash
./phonefarm bench --serial S --model m.tflite --runs 3 --json   # 锁频+等冷+GPU Delegate，退出码 0/1/2
./phonefarm bench --serial S --unlock                           # 回滚遗留锁频态
./phonefarm capture --serial S --out 目录 --frames 200 [--ready-only] [--json]
```

**有界滚动采集**（零 Token）

```bash
./phonefarm collect --query "关于我们" --max-swipes 8   # 滚动查找：命中即停 + 回报位置
./phonefarm collect --max-swipes 12 --json              # 列表采集：重叠去重 + 覆盖边界
```

产出的是**清单**不是**结论**：一次调用最多滑 `--max-swipes` 屏，某屏无新内容立即停（绝不盲滑），
`coverage.complete` 如实区分「采完整屏列表」与「撞到有界上限」。判定仍归判定那一侧。
契约见 `docs/SPEC_OBSERVATION_BUDGET.md`。

**决策原语**（零 Token 降级档）

```bash
./phonefarm decide --local --seed 7 --request 请求.json      # 降级档；同 seed 逐位可复现
./phonefarm decide --sidecar "你的桥" --request 请求.json     # 外部进程（Core ML 那一半接在这里）
./phonefarm decide --verify 模型目录 --manifest 清单.json     # 只校验资产，不推理
```

一个 state + 一组 typed question（`choice`/`score`/`noul`）→ 一组 typed answer。
**仓库不含任何模型**：`--local` 那档是降级占位不是模型（概率均匀、未校准），
只在「总比停摆强」这个意义上成立。契约见 `docs/SPEC_DECISION_PATH.md`。

**设备屏幕观战窗口**（零 Token，只读）

```bash
./phonefarm screen                                    # 浏览器看画面 + 刚点了哪
./phonefarm screen --interval-ms 1000 --actions tasks/局/log.jsonl
```

一个 std-only 的本地 HTTP 服务 + 一份自包含 HTML（无外链、无构建）。
有效观看订阅触发短时采样；每帧在同一设备域强租约内完成，测量占用、排队或恢复待核时暂缓并保留旧图年龄。
服务本身与缓存读取不持锁。子进程超时/退出不确定时保留屏障，不能自动推断设备端空闲。
当前合同见 `https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/OBSERVATION-OWNERSHIP.md`。

**可选：图形性能研究通路**（多数需 root；服务于移动 GPU 能效研究）

```bash
./phonefarm perf --serial S [--app 包名] [--source sysfs|smartperf|sflatency] --json   # 归一化性能快照（鸿蒙走 SP_daemon）
./phonefarm frame-sources --serial S [--app 包名] --json                              # 这台设备有哪些帧率/帧时来源
./phonefarm eval --request <eval_request.json> --serial S --json                     # 评测入口：v1 转交 gpu-op；v2 按 kind 路由 shader/sysparam/gray
./phonefarm gpu-op --request <eval_request.json> --serial S --json                   # Compute Shader 算子标尺（等冷 + 锁频 + A/B + Welch t）
./phonefarm gray-layer session --mode MODE --package PKG --serial S --out DIR        # Vulkan 灰层会话：挂层、抓图、有限输入与恢复
./phonefarm autoloop --out <证据目录>                                                  # 系统参数自动闭环
```

`loop_v1/`（帧时序 / 功耗采集与统计判定闭环）与 `knobs/` 的说明见各自目录下的 README。
三条纪律：判定规则先于数据冻结；sysfs 写入承担恢复义务，进出快照与恢复收据独立核验；解析全是纯函数。
灰层会话、确认采样、资格评估等合同与实验记录在
https://github.com/HGamey/docs/tree/main/repos/phonefarm 与
https://github.com/HGamey/docs/tree/main/plans/2026-10-22-agent-platform 。
游戏资源部署 `game-deploy` 是 Cargo 可选特性 `game`，默认不编译（`cargo build --release --features game`）。

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
./phonefarm serve [--root 目录]    # 23 个 phonefarm_* 工具（18 只读 + 5 执行），stdio JSON-RPC 2.0
```

## 安全边界与执行规范

1. **财务约束**：只有 VLM 通路（`run`/`benchmark`/`parallel` 及 `[evolution]` 配额）会产生真实 API Token 消耗。
   执行真机测试、大规模并行或高并发压测前，必须向用户汇报执行预算，经明确授权后方可运行。
   单测、CLI 数据解析、本地离线编译，以及 `script`/`test-batch`/`cts-fetch`/`bench`/`capture`/`keepalive`
   这些零 Token 通路，可自由执行。
2. **密钥管理**：程序运行时自动检测并加载 `./secrets.env`。缺失密钥时提示格式并安全退出。
   严禁在代码、日志及任何可提交文件（`AGENTS.md`、`README.md` 等）中硬编码真实密钥。
3. **测试环境约束**：VLM 通路与 CTS 通路的 Android 侧可用 AVD 模拟器（AVD 名 `agentphone`）；
   OpenHarmony 设备通过所在宿主机上的 `hdc` 访问，远程宿主先核实它用的是当前源码构建的二进制。
   性能测量必须上真机，模拟器的帧时与功耗没有意义。
4. **设备纪律**（性能测量相关）：
   - 同一个候选的 A/B 两臂必须落在**同一台**手机上。跨机比较会把机间差异算进改动的效果里。
   - 设备入口共用 fd 锁：同机互斥，不同手机并行；默认等待到拿锁或被取消。
     `tools/devlock run` 可包住多阶段实验，PF 子命令会借用继承的实际 fd。
     `--lock-label` 只作显示，`--lock-wait` 可显式指定等待上限；Python 子进程须传递 `PF_LOCK_FDS`。
     任务结束关闭最后一个 fd 后立即释放，恢复用 reset 和原 session／adb。
   - 影响稳态的环境条件（风扇开关、冷机阈值、供电状态）逐轮记进证据。这些不是无关细节，
     条件不同的两批数据不能混在一起比。
   - 连接、root、测量、图形接口与系统写能力均按具体设备实例验证，不从系统或品牌推断支持。
   - 跨宿主的设备分配与锁（例如 D600 集群的板锁）归上层；本仓库的锁只管同一台宿主。
5. **数据持久化规范**：程序唯一合法写入路径为 `tasks/<任务名>/`。`log.jsonl` 只追加；
   `lessons.jsonl` 必须原子写；禁止自行清理 `runs/` 历史目录；媒体与树结构大文件已由 `.gitignore`
   排除，严禁提交大体积非文本文件。`test-batch`/`cts-fetch`/`bench`/`capture` 的产物写入各自 `--out` 目录。
6. **代码稳定性**：运行与改动相关的关键行为和既有回归测试；共享合同变化再按影响扩大。
   完整集成包含真实等待与严格心跳，须与快速单元测试分开；失败留日志并定位，不反复盲跑。
   不为覆盖数堆镜像实现的测试，不计算无用哈希，优先用明确状态、所有权和数据流降低复杂度。
7. **构建后必须重新签名（Apple Silicon 硬性要求）**：`[profile.release] strip = true` 会在链接后剥离符号，
   使 ad-hoc 代码签名失效；失效的二进制在 macOS 上不报错，而是**静默卡死在 `_dyld_start`**
   （进程存活、CPU 为零、无输出），极易被误判为死循环或设备失联。跑任何实机测试前，
   先确认用的是当前源码构建并已签名的二进制。
8. **缺陷修复规范**：记录触发条件、行为变化和验证范围；保证通用性，
   不针对特定任务或界面硬编码拦截，也不以无关真机流程代替受影响行为的回归。
9. **版本控制与提交**：只从最新 main 开分支，经 PR 合并，不直接推 main；一个 PR 只做一件事，不叠 PR。
10. **汇报规范**：呈报进度或结果时给最直接的技术事实、运行指标与对局结论（成功率、耗时分位数、
    Token 支出），禁止含糊夸张的非技术词汇，严禁在文档中堆砌虚假的性能或战绩宣称。
    计划中的能力写成「计划」，不写成「已完成」；撤回过的结论不要再引用。

## 架构：不变内核 + 并列上层

```
 run/benchmark/quest   script   test-batch/cts-fetch   bench   capture   collect   loop_v1   experiment
          │              │              │                │        │         │         │          │
          └──────────────┴──────────────┴────────────────┴────────┴─────────┴─────────┴──────────┘
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
  refbench/            白盒靶子的驱动侧（靶子本身在 HGamey/refbench）；判定链已知答案自检 phonefarm selftest 的用法也在这里
  auto/                自动挑旋钮：白名单探测 → 冻结规则 → 大模型选参 → 逐组对照
knobs/                 可改动面：黑档系统参数 + 灰档 Vulkan layer 注入（由 loop_v1 驱动，说明见 knobs/README.md）
src/                   Rust 内核源码
  universal/           通用核心：统一动作协议、三大算子、优先级引擎、插件契约
  plugins/             场景插件层
  cts.rs               一致性测试 harness（双协议解析、对账、报告、结果提取）
  bench.rs capture.rs  端侧模型标尺 / 数据采集管线
  ios.rs               iOS 设备后端（经 WebDriverAgent）+ ios-probe 真机体检
  collect.rs           有界滚动采集（组合动作原语，零 Token）：有界滚动 / 重叠去重 / 覆盖边界
  screen.rs            设备屏幕窗口（零 Token，每帧短时租约；忙时保留旧图）+ screen_widget.html
  decision.rs          决策原语通路（零 Token 降级档 + 侧车协议 + 资产校验）；仓库不含训练权重
  gpuop.rs gpustat.rs  Compute Shader 算子标尺 / A-B 冻结判定统计
  hwcond.rs            共享的设备条件化（等冷/锁频/还原）与功耗遥测
  platform.rs          设备接口 Backend（Adb / Hdc / ios::Ios 三个实现）+ 各平台三层可行性声明
  perfsrc.rs smartperf.rs  性能数据来源抽象：安卓 sysfs 与鸿蒙 HiSmartPerf(SP_daemon/Xpower) 同一份 JSON
  hypo.rs caps.rs mtools.rs experiment.rs   假设—实验—证据闭环
  keepalive.rs serve.rs script.rs collect.rs screen.rs decision.rs parallel.rs telemetry.rs device.rs
phonefarm.toml         控制参数、规则阈值及模型 Provider 回退链配置
docs/                  架构契约与各能力规格（索引见 README「文档索引」）
skills/                给 AI Agent 直接阅读的英文说明书（phonefarm / phonefarm-cts / a2oh-diagnose）
examples/              示例脚本与 CTS profile 样例
tools/decision-sidecar/  决策侧车（Swift + Core ML）：把真模型接进 decide --sidecar
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

## 分支与 PR

- 只从最新 main 开分支，经 PR 合并；一个 PR 只做一件事，不叠 PR。
- 文档跟代码一起改：命令、参数或行为变了，同一个 PR 里更新 README / AGENTS.md / docs / skills。
- 每批记录已做、未做、测试层级和恢复义务；模拟通过不等于真机或全流程已可用。
- 来自 HGamey/hgamey 单仓的改动按提交同步，提交信息里写清来源提交号。

## 任务锁与恢复

所有设备入口共用 fd 锁：单机 device EX + farm SH，全场 farm EX。同设备别名归一到 device_id，普通显式 serial 自动选择单设备范围；tools/devlock run 与上层调用方使用同一 PF 实现。Python 子进程须 pass_fds，adb/hdc server 不能继承任务 fd。

锁只由最后一个 fd 关闭释放，owner JSON 仅作显示。cancel/reset 停止任务剩余进程组，设备恢复使用原 session 或 adb；不得删除锁文件来解锁，不留持久恢复屏障。原实验结果、恢复上下文与真假失败事实保留。

锁路径来自 PHONEFARM_STATE_DIR、DEVLOCK_DIR、PHONEFARM_LEASE_DIR，不再需要 init/domain-init、layout fence 或共享 GID。部署协议切换须先清空旧任务；不混跑新旧参与者。宿主替身验证不代表设备恢复已通过真机验收。
