# phonefarm

移动端图形能效 harness 的底座，兼通用移动端自动化基础设施。

## 在整体里的位置

整套东西是一个 harness。用户只提一种任务——「把某个游戏在某台手机上优化一下」，
内部自动决定用哪几层。本仓库是底座那一层，负责四件事：

- 设备：Android `adb` 与 OpenHarmony `hdc` 两族的连接、保活、确定性脚本回放。
- 应用改动（[`knobs/`](knobs/README.md)）：黑档系统参数（DVFS / 限帧 / 热 / 调度 / 刷新率），
  灰档 Vulkan layer 注入（render pass 观测，以及 LoadOp / 精度 / 分辨率 / shader 改写）。
- 测量：安卓读 sysfs 电源轨加 raw ftrace kgsl，鸿蒙走 HiSmartPerf，上层拿同一份 JSON。
  测功耗期间 root 停充，供电状态写进报告。
- 统计判定与还原：判定规则在看到候选数据之前落盘冻结，不显著就回退，sysfs 写入退出前
  全部恢复，进出快照逐行相等才算这轮成立。

提改动的是上层——[`game_opt_loop`](https://github.com/HGamey/game_opt_loop)
（入口与决策层：接任务、摸底、决定开哪几路、出方案、归档优胜、出报告），或者直接是人。
判定这一侧留在本仓库，规则先于数据冻结，所以出主意的一方改不了判定。

### 三个优化层级

| 层级 | 改什么 | 白盒（有源码） | 黑盒（商用游戏） |
|---|---|---|---|
| ① 游戏代码 | 渲染流程、资源、shader 源码 | 可用 | 不可用 |
| ② 图形接口 Vulkan 层 | LoadOp / 精度 / 分辨率 / shader 改写 | 可用 | 可用（需能挂层） |
| ③ 系统参数 | DVFS、总线、调度、限帧、刷新率 | 可用 | 可用（需 root） |

鸿蒙商用机在没有 root 的情况下只能测不能改：HiSmartPerf 采得到帧率、帧时、温度、功耗，
但 ② 和 ③ 都动不了。可能的出路有工程机、官方性能接口、开发者模式下自签名应用三条，
都还没核实过。

### 测试载体

载体在 [`loop_v1/carriers/`](loop_v1/carriers/)：Megacity、Vulkan-Samples、AnKi，
另有白盒靶子 [`refbench`](https://github.com/HGamey/refbench) 🔒。
白盒载体源码在手、瓶颈已知，用来验机制；闭源真游戏用来验效果。
`sr_loop` 是已结项的前序实验（端侧超分网络演化），不在当前主线上。

### 设备

同一个候选的 A/B 两臂必须落在同一台手机上：跨机比较会把机间差异算进改动的效果里。
规划中的最小配置是 4 台——骁龙 2 台（一台跑测试、一台留给调试）、天玑 1 台、鸿蒙 1 台；
完全体 8 到 10 台。这是计划，不是现状。

设备是共享资源，跑之前用设备锁 `devlock` 抢占：拿锁、后台心跳续期、跑完释放，
被占着时排队而不是绕过去硬跑。两个任务同时压一台手机，两边的数都作废。

---

## 📌 先读这两份

| 文档 | 是什么 | 什么时候看 |
|---|---|---|
| **[移动 GPU 优化技术路线梳理](docs/MOBILE_GPU_OPT_ROUTES.md)** | 移动 GPU 上能做的优化手段全清单，按带宽/几何/shader/分辨率/时序热分五类。每条标注**移动端特有性、量级、在 loop_v1 里用哪个指标验、需不需要白盒** | **决定下一步优化做什么时** —— 闭环里的候选改动从这份清单里挑 |
| **[loop_v1 — 最小可信闭环](loop_v1/README.md)** | 端到端性能优化闭环：负载 → 采集 → 归因 → 旋钮 → 复量 → 统计判定 → 回滚 → 证据归档。五条判据 2026-09-16 实测全部通过 | **想跑一轮真实优化实验时** |

可改动面（「能改什么」这一侧）在 [`knobs/`](knobs/README.md)：黑档系统旋钮 + 灰档 Vulkan layer 注入，供 loop_v1 驱动。

几个已落盘的实测结论，写在这里免得重复踩：

- 「带宽不够」不能当默认假设。DDR/LLCC 下限钉到硬件上限（3187→5333MHz，+67%）后帧时间 p95 显著改善 −1.61%（p=0.0079），但 `gpu_active_mean` 不显著（p=0.42）——该负载下 GPU 并没有被带宽饿着。
- 帧率封顶时，降工作量换不出帧率。2026-09-16 那一轮归因 5 轮一致：帧率封顶 @30fps，GPU 只用掉 64.5%，余量 11.91ms，热事件 0。在这种负载上，任何降低 GPU 工作量的路线（含 NN 超分）都只能换功耗与热预算。
  需要更正的是当时的归因：把它说成「厂商把原神限在 30fps」没有依据。那是原神 7.0.0、当时那套游戏设置下的观测。2026-09-23 同一台机器、7.1.0、游戏内 60 帧设置下实测就是 60fps（`frame_p50` 16.69ms），`gpu_active_mean` 约 13.3ms/帧，证据在 `knobs/evidence/06_genshin_world/ab_report.json`。当时为什么封在 30fps 还没核实，别再当定论引用。
- 改得准不等于换得出性能。灰档 Vulkan 层在原神大世界里把 LoadOp `LOAD→DONT_CARE` 的改写绑上了 4.4 到 5.2 万次，确实命中；但 5 对交替 A/B 下 `frame_p95` 只差 −0.51%，置换检验 p=0.325，判无效（PR #31）。机制成立（能改、改得准、可自报、可回滚），这一条换不出性能。
- 功耗要停充了才能比。原神固定场景两通路并排采 30 秒、全窗口停充：整机功耗 HiSmartPerf 报 4.867 W，本仓库报 4.979 W，相对差 2.3%（PR #32）。对照充电态那一轮，同一条轨两边分别报 3.086 W 与 777 W。
- NN 的账要算端到端。`phonefarm bench` 区分 `gpu`（只含模型算子）与 `invoke`（含张量拷贝同步）两个口径，后者高约 7ms 且与网络结构无关，540×960→1080×1920 每帧拷贝 31MB。端到端约 10ms，吃掉 60fps 帧预算的 60%。这 7ms 是否测量工装产物尚未定论，见路线图 §3。

载体的可重复性基线（`frame_p95` 离散度，越小越好）：refbench 0.45%，Megacity 0.987%（PR #33），原神 1.37%。白盒靶子最稳，闭源真游戏最散，所以验机制在前者、验效果在后者。

NN（端侧超分 / 插帧）是这个闭环里的**一类候选改动**，不是主线；那条线的前序实验在 [`sr_loop`](https://github.com/HGamey/sr_loop)（已结项），后续的 Compute 算子直通管线在 `game_opt_loop`。

---

## 底座怎么搭的

性能闭环不是独立造的一套东西，它和其它能力共用同一个底座：
**一个 Rust 内核，两种设备后端（Android `adb` / OpenHarmony `hdc`），
在「设备抽象 + 记录契约 + 遥测 + 证据分级」之上并列挂多条互不依赖的上层通路。**

```
   性能优化闭环   自主UI遍历   确定性脚本回放   一致性测试   端侧模型标尺   采集   假设-实验-证据
    (loop_v1)   run/benchmark    script      test-batch      bench      capture   experiment
         │            │            │             │             │           │          │
         └────────────┴────────────┴─────────────┴─────────────┴───────────┴──────────┘
                                          │
              ┌───────────────────────────┴────────────────────────────┐
              │ 不变内核：设备抽象(adb/hdc) · 记录契约 · 遥测           │
              │           确定性纪律(三道检查/双重看门狗) · 证据分级     │
              └────────────────────────────────────────────────────────┘
```

上层各通路互不依赖：卸掉 VLM 通路，`script` / `test-batch` / `bench` 照常工作；反之亦然。
**多模态视觉模型（VLM）只是其中一条通路，不是这个项目的定义**——
`script` / `test-batch` / `cts-fetch` / `bench` / `capture` / `keepalive` 全程零 Token，
不碰模型也各自成立。遥测口径全局统一，因此性能局、脚本局、CTS 局、VLM 局可以横向对比。

> **🤖 给 AI Agent 用（推荐姿势）**：本项目自带 agent 说明书 `skills/phonefarm/SKILL.md`。
> 复制下面这段发给你的 Agent（Claude Code / Codex / Cindy / octos 等），立即开始使用：
>
> ```
> Read https://raw.githubusercontent.com/BH3GEI/phonefarm/main/skills/phonefarm/SKILL.md and strictly follow its rules. Then drive the phonefarm tool accordingly. Run `phonefarm last` first to verify the environment, then report the current device status to me.
> ```
>
> `skills/` 下另有 `phonefarm-cts`（一致性测试）与 `a2oh-diagnose`（A2OH 桥接环境排障）两份专用说明书。
> 这些说明书为全英文，以便各家 Agent 直接阅读理解。

## 能力全景

| 能力族 | 命令 / 位置 | 烧 Token | 说明 |
| :--- | :--- | :--- | :--- |
| **性能优化闭环**（主线） | `loop_v1/` | 否 | 对可重复游戏负载跑完整一轮「基线 → 归因 → 改动 → 对照 → 统计判定 → 回滚 → 证据归档」。判定用精确置换检验 + 置换反演 CI，**规则先于数据冻结**；sysfs 写入退出前全部恢复，进出快照逐行相等才算这轮成立；下游解析全是纯函数，同一份 trace 每次重算逐字节一致 |
| **帧时序与 GPU 归因** | `loop_v1/`（raw ftrace kgsl） | 否 | 不侵入游戏进程，一次采集同时拿到帧节奏、GPU 执行时长、总线带宽投票、热降频。`SurfaceFlinger --latency`、`gfxinfo`、Perfetto GPU producer 三条路都已实测排除，原因见 [`loop_v1/README.md`](loop_v1/README.md) |
| 端侧模型实测标尺 | `bench` | 否 | TFLite 模型真机延迟：锁频 + 等冷 + GPU Delegate 算子日志解析。区分 `gpu` 与 `invoke` 两个口径 |
| 数据采集管线 | `capture` | 否 | 按状态筛选的原始帧采集 + manifest 路线分段 |
| 确定性脚本与回放 | `script` | 否 | 固定动作序列执行，或原样回放某次历史对局——性能评测要求各轮动作路径完全一致，靠它消除模型决策发散 |
| 自主 UI 遍历 | `run` `benchmark` `parallel` `quest` `plugins` | **是** | VLM 决策的六步回路：深度遍历、多轮评测、多设备并行 |
| 一致性测试 harness | `test-batch` `cts-fetch` | 否 | CTS/XTS 批量执行与结果提取，Android/OpenHarmony 双协议，逐用例出断言栈 |
| 设备与农场运维 | `devices` `keepalive` `probe` `exec` | 否 | adb/hdc 双族设备发现、保活巡检（唤醒+解锁+不息屏）、只读调试通道 |
| 假设—实验—证据闭环 | `hyp` `pred` `caps` `tools` `experiment` `export` `eval` | 部分 | 竞争解释登记、预测先行、能力固化、A/B/C 对比实验、训练数据导出 |
| 只读下钻 | `last` `runs` `show` `status` `stats` `cat` `tasks` `tree` `lessons` `campaign` `schema` `config` | 否 | 全部离线解析本地落盘产物 |
| 工具服务 | `serve` | 否 | 21 个 `phonefarm_*` MCP 工具，供 octos 等客户端挂载 |

底座层面的两条老能力贯穿全部通路：**经验库与状态共享**（`lessons.jsonl` 经验库 +
`tree.json` 状态转移图，跨局累积）、**无损落盘与下钻**（步骤级截图、原始 UI 树、
模型原始回包、规则判定结果全部落盘，`phonefarm show` 可单步回溯）。

## 快速开始

```bash
# 1. 配置密钥（只有 VLM 通路需要；script/test-batch/bench/capture 不需要）
cp secrets.env.example secrets.env   # 填入 GLM_KEY(智谱 Coding 套餐)等
# 程序运行时自动读取 ./secrets.env；检测到必选密钥缺失时给出配置说明并安全退出

# 2. 编译构建（Apple Silicon 必须重新签名，见下方说明）
cd src && cargo build --release && cp target/release/phonefarm .. && cd .. && codesign --force --sign - ./phonefarm
```

> **为何要 codesign**：release profile 的 `strip = true` 会在链接后剥离符号，使 Rust 附加的 ad-hoc 代码签名失效。
> 失效的二进制在 macOS 上不报错，而是**静默卡死在 `_dyld_start`**（进程存活、CPU 为零、无任何输出），
> 极易被误判为死循环或设备失联。
>
> `adb` 自动定位顺序：`ADB_BIN` 环境变量 → 仓库根 `platform-tools/` → `PATH` → 常见系统 SDK 目录。
> OCR 辅助通道首跑时自动编译（需系统装有 `swiftc`）；编译失败则该通道自动关闭，不影响主回路。

```bash
# 3. 先确认设备在线
./phonefarm devices                        # 列出当前连接的 adb 与 hdc 设备

# 4. 挑一条上层通路开跑（下面各节分述）
```

## 设备与农场运维

```bash
./phonefarm devices                        # adb 与 hdc 两族设备一并列出
./phonefarm keepalive                      # 对全部在线设备巡检一轮：唤醒 + 解锁 + 不息屏
./phonefarm keepalive --status             # 只读报告：连接/亮屏/不息屏是否生效
./phonefarm keepalive --watch              # 常驻守护（默认 300s 一轮，新上线设备自动纳入）
./phonefarm probe --serial <S> "只读命令"   # 只读调试通道
./phonefarm exec --serial <S> "命令" --yes  # 写操作通道，高危，必须显式 --yes
```

`keepalive` 补的是**农场级**待命期：只要设备挂在农场里，就该醒着、解着锁、不息屏。
任务级的生命周期（进任务常亮、退出锁屏保电池）由各上层通路自己管。规格见 `docs/SPEC_KEEPALIVE.md`。

## 自主 UI 遍历（VLM 通路）

```bash
# 单局遍历（Android 模拟器）
./phonefarm run --task 今日头条遍历 --endless --budget-calls 90 --app com.ss.android.article.news "<目标文本>"

# 多轮评测，出成功率/耗时/Token 分位数
./phonefarm benchmark --task 今日头条遍历 --rounds 10 --budget-calls 90 --app com.ss.android.article.news --json "<目标文本>"

# OpenHarmony 真机
./phonefarm run --serial hdc:<serial_id> --task OH设置冒烟 --budget-calls 30 "<目标>"

# 多设备并行（每设备独立一局，stdout 逐行带 [设备] 前缀，任一失败整体退出码非 0）
./phonefarm parallel --job "任务A|目标A|emulator-5554|com.pkg" --job "任务B|目标B|hdc:<key>" --budget-calls 60
# 同一任务名派给多台设备会被拒绝——经验库 lessons/tree 会互踩，请用不同任务名分开

# 场景插件与其独立长跑 Agent
./phonefarm plugins                        # 列出已登记的场景插件
./phonefarm quest --mode auto --sec 1800   # 原神插件的独立长跑 Agent（零 Token 轻量视觉 + 手柄注入）
```

这条通路会消耗真实 API Token。跑大规模并行或长测之前请先估算预算。

## 确定性脚本与回放（零 Token）

```bash
# 跑一份固定动作脚本，保留全套遥测
./phonefarm script --task 游戏压测 --app com.tencent.tmgp.projectc --repeat 10 examples/sample_game_benchmark.json

# 原样回放某次历史对局的动作流
./phonefarm script --task 轨迹重放 20260831-213215

# 后台跑，立即回报局 ID
./phonefarm script --detach --task 游戏压测 examples/sample_game_benchmark.json
```

脚本模式不调用任何模型，但走同一套执行与遥测管线，因此产出的记录与 VLM 局完全同构，
`show` / `stats` / `cat` 一视同仁。规格见 `docs/SPEC_SCRIPT_MODE.md`。

脚本的 `instrument` 原语跟设备后端走：hdc 设备上合成 OH 的 `aa test`，adb 设备上合成 `am instrument`。

## 一致性测试 harness（CTS / XTS）

把测试 APK/HAP 的批量执行、逐用例结果解析、断言栈提取、对账与报告串成一个 mini-Tradefed。
Android 与 OpenHarmony 两套官方 runner 协议都解析，**上层报告格式一致**。

```bash
# 按 profile 批量跑（同事的 API 测试配置直接喂）
./phonefarm test-batch --profile P.json --environment E.json --serial <S> --out 结果目录

# 直接指定模块：Android 写 pkg/runner
./phonefarm test-batch --module android.content.cts/androidx.test.runner.AndroidJUnitRunner

# OpenHarmony 写 oh:bundle/module/Runner（stage model 的 HAP 模块名不能省）
./phonefarm test-batch --module oh:com.example.demo/entry_test/OpenHarmonyTestRunner --serial hdc:<key>

# 扫 APK 目录，aapt 取包名并与 pm list instrumentation 对账
./phonefarm test-batch --dir /path/to/apks --install-cmd 'pm install -r {apk}'

# 常用旋钮
#   --include/--exclude 正则筛用例   --resume 断点续跑   --retry N 失败重试
#   --timeout-ms / --idle-timeout-ms 双重看门狗
#   --heal-script 卡死时的自愈脚本
#   --detach 后台跑，轮询 <out>/summary.json
```

产出：`summary.json`（逐用例判定 + 断言栈）与 JUnit XML。失败用例带完整调用栈，**不必再 hdc 进机器翻日志**。

**结果提取（`cts-fetch`）** —— 当结果是别的工具（官方 CTS/XTS 套件、同事的流程）落在设备上的，
不重跑，只把结果拉回来扫断言：

```bash
./phonefarm cts-fetch --remote /data/local/tmp/cts_result --serial hdc:<key> --out 本地目录
#   --pattern 正则   自定义断言行匹配（缺省匹配常见断言失败字样）
#   --max-mb N       单文件大小上限（缺省 16），超限与二进制文件跳过并在报告中如实标注
```

设备侧只读（`file recv` 拉树），与 `test-batch` 互补。产出 `assertions.json`。

判定纪律：跳过/假设失败**不计为通过**；崩溃、看门狗超时、runner 报错都会把未交代的用例如实标成
`NOT_RUN` / `ENV_BLOCKED` 并对账，绝不静默零记。规格见 `docs/SPEC_CTS_HARNESS.md`。

## 端侧模型实测标尺

```bash
./tools/tflite/fetch.sh                    # 首次：拉官方 nightly 的 benchmark_model (Android arm64, 内置 GPU Delegate)
./phonefarm bench --serial <ID> --model model.tflite --runs 3 --json
./phonefarm bench --serial <ID> --unlock   # 异常退出遗留锁频态的回滚
```

每轮：未锁频等冷（<40C）→ CPU performance + kgsl 档位锁频 → benchmark_model GPU Delegate → 立刻解锁。
判定：全图 GPU（无 CPU 回退）、3 轮离散度 ≤5%、GPU 内核时延 median ≤4.0ms；退出码 0/1/2 = PASS/FAIL/ERROR。
需 root。规格见 `docs/SPEC_SR_LOOP.md`。

## 数据采集管线

```bash
./phonefarm capture --serial <S> --out 目录 --frames 200 --settle-ms 800 --json
./phonefarm capture --serial <S> --ready-only    # 只确认目标状态，不走位不单步
```

按状态筛选留帧 + manifest 路线分段，供下游训练/评测管线消费。

## 性能数据来源：安卓与鸿蒙同一份 JSON

```bash
./phonefarm perf --serial hdc:<connect key> --app <包名> --rounds 30 --json   # 鸿蒙 HiSmartPerf
./phonefarm perf --serial emulator-5554 --power-rail usb --json               # 安卓 sysfs 电源轨
./phonefarm perf --from-csv /path/to/data.csv --json                          # 离线解析，不碰设备
```

安卓读 `/sys/class/power_supply` 电源轨，鸿蒙走 HiSmartPerf 的 `SP_daemon`（帧率/帧时/温度）
与 Xpower（功耗）——两条通路没有任何共同点，但上层只拿同一个 `PerfSnapshot`：
字段集合由同一个 struct 保证逐字相同。**采不到的字段是 `null` 并附一条原因，绝不填 0**
（插着 USB 充电时功耗读数是垃圾值，一个 `0.000 W` 会把上层的 A/B 裁决直接带沟里）。
安卓侧只是把 `hwcond` 现成的采样包了一层，既有行为一行未动。

> **鸿蒙侧未上真机验证**：本机既没有鸿蒙真机也没装 `hdc`。命令口径取自本机
> HiSmartPerf-Editor 的实现与 SmartPerf-Device 官方参数表，解析器由 2026-09-18
> 的真实采集产物钉死，但整条设备通路一次也没在真机上跑过。口径出处、单位约定与
> 上真机后要补的验证清单见 `docs/SPEC_PERF_SOURCE.md`。

## 性能优化闭环（`loop_v1/`）怎么跑

```bash
cd loop_v1
adb push tools/*.sh /data/local/tmp/ && adb shell chmod 755 '/data/local/tmp/*.sh'
adb shell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > runs/snap_before.txt

# 交错跑改动臂与对照臂，让残余漂移对两臂等量影响
for i in 1 2 3 4 5; do
  adb shell "su -c 'sh /data/local/tmp/knob_ddr_boost.sh apply'"
  bash tools/run_once.sh knob$i runs/knob$i
  adb shell "su -c 'sh /data/local/tmp/knob_ddr_boost.sh restore'"
  bash tools/run_once.sh ctrl$i runs/ctrl$i
done

./phonefarm report --baseline 'runs/ctrl*' --knob 'runs/knob*' \
    --snap-before runs/snap_before.txt --snap-after runs/snap_final.txt > runs/report.json
bash tools/replay_test.sh runs      # 离线回放自检
```

两条写死在流程里的纪律：**判定规则在看到候选数据之前落盘冻结**；负载必须在外部条件稳定的
窗口内跑完（原神的昼夜循环会把离散度从 1.37% 推到 5.35%，而且是单向漂移，只看离散度发现不了，
所以 `phonefarm analyze` 除离散度外还报最小二乘斜率与相邻递增比例）。

细节、测帧手段的选型排除过程、以及「一帧不等于一次 GPU 提交」这个坑，见 [`loop_v1/README.md`](loop_v1/README.md)。
候选改动从 [`docs/MOBILE_GPU_OPT_ROUTES.md`](docs/MOBILE_GPU_OPT_ROUTES.md) 里挑。

> **进行中**：`loop_v1/` 的 Python 工具链与 `knobs/` 的 shell 脚本正在往 Rust 内核里收，
> 目标形态是这两块都不再需要宿主机上另装解释器。上面的跑法在收完之前仍然有效。

## 假设—实验—证据闭环

局内发生重复失败或 done 复核未达时，系统让模型给出 2~4 个**竞争解释**和一份能区分它们的检验；
检验预测**先登记**，执行后由确定性代码对照封闭断言词表链接结果，支持/反对计数只从此处来——
模型不得给自己记功。证据达标的假设在局末固化为能力候选，经评测启用后注入后续决策。

```bash
./phonefarm hyp                            # 假设库：竞争解释与其生命周期
./phonefarm hyp --retract <id>             # 人工裁决落账（--supersede 同理）
./phonefarm pred                           # 预测台账：登记在先的预测与其兑现情况
./phonefarm caps [--adopt id|--rollback id]  # 能力候选的固化与回滚
./phonefarm tools [--propose def.json|--retire id]  # 测量工具的提案/校准/退役
./phonefarm experiment <spec.toml> [--arm A|B|C] [--ablate no-active-testing] [--resume|--report-only] [--json]
./phonefarm export [--task T] --split train|heldout --out <文件> [--redact-config toml]
./phonefarm eval --set <evalset.toml>      # 模型评估接口（骨架，接口先行）
```

全部机制默认开启；`phonefarm.toml` 的 `[evolution] enabled = false` 可整体旁路，行为与旧版完全一致。
一切模型调用走 `[evolution]` 独立配额。规格见 `docs/SPEC_EVOLUTION.md`，日常操作面见 `docs/EVOLUTION_OPS.md`。

## 数据查询与下钻

所有历史对局及遥测产物均在本地落盘，通过内置 CLI 直接调阅，**不消耗任何 API 额度**：

```bash
phonefarm last                       # 最近一局的结论与关键指标
phonefarm runs [--task T]            # 指定任务下的所有局 ID
phonefarm status [<局ID>|--task T]   # 运行中 / 已结束 / 中断
phonefarm show <局ID>                # 局概要：目标、步骤、判定、产物清单
phonefarm show <局ID> --step 5       # 第 5 步：截图、UI 元素、遥测快照
phonefarm show <局ID> --raw          # 模型原始回复包
phonefarm show <局ID> --hooks        # 各步的拦截与规则判定记录
phonefarm show <局ID> --events       # 异常事件流（崩溃、ANR、FD 增长）
phonefarm show <局ID> --crashes|--anr|--trace
phonefarm cat <路径> [--head/--tail N] [--grep 词]   # gzip 自动解压、JSONL 格式化、图片属性
phonefarm stats <局ID>               # FPS/CPU/内存/温度的分位数与分布
phonefarm schema [--type r类型]      # log.jsonl 全部合法记录的字段模型（代码内生成，永远最新）
phonefarm tasks | tree | lessons | campaign | config
```

查看类命令全部支持 `--json`。局 ID 支持前缀模糊匹配（多项匹配时输出候选列表）。

下钻链路：`last` 确认异常局 → `show <局ID> --step <N>` 看该步遥测与上下文 → `cat .../stepN.xml.gz` 提取原始 UI 树离线复盘。

## 遥测

每步采集 72 项高低频设备指标（字段定义见 `src/telemetry.rs` 的 `Telemetry` 结构）：FPS、Janky 占比、
帧时 p50/p90、各 CPU 核心频率、内存 Pss 明细、SoC 与电池温度、电流电压、FD/Socket 占用、磁盘与网络状态等。
指标按设备能力尽力采集，取不到的字段留空而不是填 0。**所有上层通路共用同一套遥测**——脚本局、CTS 局、VLM 局的遥测口径完全一致，
因此可以横向对比。

## 架构：不变内核 + 并列上层

```
        run/benchmark/parallel/quest   script   test-batch/cts-fetch   bench   capture   experiment
                     │                   │              │                │        │          │
                     └───────────────────┴──────────────┴────────────────┴────────┴──────────┘
                                                  │
                        ┌─────────────────────────┴──────────────────────────┐
                        │  不变内核：设备抽象(adb/hdc) · 记录契约 · 遥测      │
                        │            确定性纪律(三道检查/双重看门狗) · 证据分级 │
                        └────────────────────────────────────────────────────┘
```

上层各通路互不依赖：卸掉 VLM 通路，`script`/`test-batch`/`bench` 照常工作；反之亦然。
新增一条上层通路不应该修改内核。

### 通用核心 + 场景插件

VLM 通路内部再分两层，防止单一应用的特殊需求侵蚀通用性：

- **核心层**（`src/universal/`、`src/runtime.rs`、`src/device.rs`）只做通用能力：统一感知、
  统一动作协议、通用三大算子（弹窗确认 / 对白跳过 / 导航寻路）、优先级调度。
  核心代码**不含任何具体应用的包名、界面文案或流程假设**。
- **插件层**（`src/plugins/`）承载全部专用场景，通过 `universal::ScenarioPlugin` 契约接入。

单帧决策的优先级阶梯：

| 档位 | 归属 | 说明 |
| :--- | :--- | :--- |
| 1 | 插件 `intercept` | 场景特有语义，抢在通用规则之前 |
| 2 | 通用弹窗算子 | 公告、权限、评分等阻断性弹窗 |
| 3 | 通用对白算子 | 字幕、分支卡片、CG 转场 |
| 4 | 通用寻路算子 | 循迹转向、列表滚动、脱困 |
| 5 | 插件 `decide` | 场景兜底策略，命中即省一次模型调用 |
| 6 | 视觉大模型 | 前五档都不接管时才调用 |

前五档均为本地规则，零 Token。新增一个专用场景**只需在 `src/plugins/` 下加模块并在
`register_builtin` 登记，核心代码一行不改**；卸下全部插件后核心仍能独立工作。

若某需求迫使你修改核心去迁就单一应用，那是设计错了，应当改为插件。

### 六步执行回路（VLM 通路）

1. **状态采集**：并行截屏 + 提取 UI 布局树。
2. **上下文组装**：任务目标、lessons 经验、近期路径、当前屏幕拼接为提示词。
3. **模型决策**：单次调用最多规划 4 组动作；非首个动作无需额外调用。
4. **执行前校验**：值域、前科（ban 半径）、落点内容（空白点击）三道拦截。
5. **动作执行**：坐标归一化换算 → 下发输入事件 → 等待画面定格。
6. **验收差异**：比对动作前后 UI 树或像素差异，做进展判定、空击仲裁、感知自愈。

契约细节见 `docs/DESIGN.md`。

## MCP 工具服务（`phonefarm serve`）

`phonefarm serve [--root <目录>]` 把 CLI 能力以 **MCP stdio 服务**（换行分隔 JSON-RPC 2.0）
暴露给 octos 等外部 Agent 宿主：**21 个 `phonefarm_*` 工具**，全部经既有 CLI 契约自调用，零新依赖。

- 只读面（17）：`devices` `tasks` `runs` `last` `status` `show` `stats` `lessons` `tree` `campaign`
  `hyp` `pred` `caps` `tools` `schema` `config` `cat`
- 执行面（4）：`run` `benchmark` `script` `quest`

安全护栏：

- **run/benchmark/script 强制 `--detach`**：立即回报局 ID，进度用 `phonefarm_status` / `phonefarm_show`
  轮询（适配客户端 60s 工具超时）。
- **cat 路径监狱**：只允许读 tasks 数据根之下的文件，逃逸一律拒绝。
- **不暴露 probe/exec/parallel**：设备写操作的唯一入口是受三道拦截保护的六步回路，裸 shell 不进 MCP 面。
- 密钥不依赖环境继承：detached 子进程仍走 `./secrets.env` 自举（`--root` 先把工作目录钉在仓库根）。

octos 侧挂载（`config.json` 或 profile 的 `[[mcp_servers]]`）：

```jsonc
{
  "mcp_servers": [
    {
      "command": "/path/to/phonefarm",
      "args": ["serve", "--root", "/path/to/phonefarm-repo"],
      "concurrency_class": "exclusive"  // 设备是独占资源，串行化本服务的全部工具调用
    }
  ]
}
```

规格见 `docs/SPEC_MCP_SERVE.md`。

## 目录结构

```
loop_v1/               性能优化闭环：负载→采集→归因→旋钮→复量→统计判定→回滚→证据
  scripts/ tools/      采集与解析（raw ftrace kgsl；解析全是纯函数，可逐字节复算）
  carriers/            测试载体：megacity / vulkan-samples / anki-sponza
  refbench/            白盒靶子的驱动侧（靶子本身在 refbench 仓库）
  auto/                自动挑旋钮：白名单探测 → 冻结规则 → 大模型选参 → 逐组对照
  fixtures/ runs/      夹具与历轮证据
knobs/                 可改动面：黑档系统参数 + 灰档 Vulkan layer 注入（原 HGamey/knobs，已归档并入）
  black/ gray/         两档的脚本与层源码
  contract/ evidence/  旋钮接口约定与上机证据
src/                   Rust 内核源码
  universal/           通用核心：统一动作协议、三大算子、优先级引擎、插件契约
  plugins/             场景插件层：一切专用场景都落在这里
  cts.rs               一致性测试 harness（双协议解析、对账、报告、结果提取）
  bench.rs capture.rs  端侧模型标尺 / 数据采集管线
  hypo.rs caps.rs mtools.rs experiment.rs   假设—实验—证据闭环
  keepalive.rs serve.rs script.rs parallel.rs telemetry.rs device.rs
phonefarm.toml         判定阈值、提示词模板、模型 Provider 轮询与降级链
docs/                  架构契约与各能力规格（见下方索引）
skills/                给 AI Agent 直接阅读的英文说明书
examples/              示例脚本与 CTS profile 样例
tools/                 辅助工具（如 tflite/fetch.sh）
round.sh               单轮调度外壳脚本（保留兼容，生产建议用 benchmark）
ocr.swift / ocr        OCR 辅助识别组件（macOS Vision，首跑自动编译）
tasks/<任务名>/        程序唯一可写区域
  lessons.jsonl        本任务经验库（原子写）
  tree.json            状态转移图
  campaign.tsv         评测汇总
  runs/<局ID>/         log.jsonl（只追加）、ctx.log、步骤截图与 XML UI 树
```

## 文档索引

| 文档 | 内容 |
| :--- | :--- |
| **`docs/MOBILE_GPU_OPT_ROUTES.md`** | 移动 GPU 优化技术路线梳理：五类候选 / 每条怎么在 loop_v1 里验 / 黑盒白盒分叉 / NN 的实测代价 |
| **`loop_v1/README.md`** | 端到端性能优化闭环：负载→采集→归因→旋钮→复量→统计判定→回滚→证据，五条判据实测通过 |
| `docs/GOLDEN_RULES.md` | 研发与操作金科玉律：架构铁律 / 设备生命周期 / 模型选型纪律 / 工程规范 |
| `docs/DESIGN.md` | 核心架构契约：分层与跨通路不变量 / 记录契约 v1 / 六步回路 / 任务隔离 / 写入权限 |
| `docs/SPEC_PERF_SOURCE.md` | 性能数据来源抽象：安卓 sysfs 与鸿蒙 HiSmartPerf 同一份 JSON / 命令口径出处 / null 纪律（鸿蒙侧未上真机验证） |
| `docs/SPEC_SR_LOOP.md` | 端侧模型标尺与采集管线：bench / capture 的判定口径 |
| `docs/SPEC_SCRIPT_MODE.md` | 确定性脚本与历史轨迹回放：纯离线 / 零 Token / 全遥测 / 重放契约 |
| `docs/SPEC_CTS_HARNESS.md` | 一致性测试 harness：双协议解析 / 对账判定 / 结果提取 |
| `docs/SPEC_KEEPALIVE.md` | 农场级设备保活巡检：顺序契约 / adb 与 hdc 两族差异 |
| `docs/SPEC_MCP_SERVE.md` | MCP stdio 工具服务：工具面 / 协议子集 / 安全护栏 / octos 接法 |
| `docs/SPEC_EVOLUTION.md` | 假设—实验—证据闭环：证据分级 / 预测先行 / 能力固化 / A_B_C 实验 |
| `docs/EVOLUTION_OPS.md` | 上一条的日常操作与审计面 |
| `phonefarm schema` | log.jsonl 全部合法记录的字段手册（代码内生成，永远最新） |
