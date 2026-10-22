# phonefarm

设备自动化与实测基础设施：一个 Rust 内核，三种设备后端（Android `adb` / OpenHarmony `hdc` /
iOS `wda`），
在同一套「设备抽象 + 记录契约 + 遥测 + 证据分级」之上，并列提供多条互不依赖的上层能力。

多模态视觉模型（VLM）只是其中一条上层通路，不是这个项目的定义。
`script` / `test-batch` / `cts-fetch` / `bench` / `capture` / `collect` / `keepalive` 全程零 Token，不碰模型也能独立工作。
宿主架构实现执行回路与决策机制分离：模型只做屏幕理解与动作规划，状态采集、规则拦截、
校验、复盘与系统监控全部由 Rust 宿主进程控制。

## 在整体里的位置（改这个仓库之前先看这一段）

整套东西服务于外部 Agent：使用远程真机系统性优化指定游戏/系统，探索多种方向，
完成可复现实验并留下可审核结果。Agent 的优先级判断可在任意工具里完成；GOL 是可选客户端。
网页与 CLI/API 同等优先。目标、方向、实验、证据、人工指导/协助/评价独立于聊天工具持久化；
人异步参与，只阻断依赖其反馈的方向，其他可行方向继续。
当前产品准则与开发计划见 https://github.com/HGamey/docs/tree/main/plans/2026-10-22-agent-platform 。
本仓库提供设备与测量底座，当前能力主要分为四类：

- 设备：adb / hdc / iOS(WebDriverAgent) 三族的连接、保活、确定性脚本回放。新增一种设备/平台 = 实现 `platform::Backend`
  并登记 serial 前缀，各平台能开哪几层及原因见 `https://github.com/HGamey/docs/blob/main/repos/phonefarm/docs/DEVICES.md`。
- 应用改动（`knobs/`）：黑档系统参数，灰档 Vulkan layer 注入。
- 测量：安卓 sysfs 电源轨加 raw ftrace kgsl，鸿蒙 HiSmartPerf，上层拿同一份 JSON；
  测功耗期间停充。
- 统计判定与还原：规则先于数据冻结，不显著就回退，退出前恢复现场。

外部 Agent、GOL 或人提出候选；本仓库按冻结合同采集和判定。
用户可在实验前选择性能/画质预算、原游戏基线和模块 ms 目标；探索与确认分层。
不能在看到候选结果后改写旧实验的判据，也不能把未知功耗或没有胜过双线性直接等同于无优化价值。

优化分三个层级：① 游戏代码（需要白盒）② 图形接口 Vulkan 层 ③ 系统参数。
闭源商用游戏的可用路线还取决于游戏内设置和公开接口；不能仅按是否 root 做统一结论。
鸿蒙商用机的 root 注入/系统改动不能照搬 Android；HiSmartPerf 等来源须按具体设备、
版本和实际字段验证。工程机、官方接口或自签名应用也是待核路线，不是现成能力。

测试载体在 `loop_v1/carriers/`（Megacity / Vulkan-Samples / AnKi），
白盒靶子在同级的 `refbench` 仓库。`sr_loop` 是已结项的前序实验，不在当前主线上。

**进行中，别写成已完成**：`loop_v1/` 的判定口径（解析→归因→统计→判据收口→白名单→判定→
画面判据→模型交互/本地变异器）、两通路对照、Vulkan-Samples 三道闸与两臂判读面、refbench/vks
报告都已经收进 Rust，`autoloop` 编排（`phonefarm autoloop`）与灰档主机侧编排（`phonefarm gray-layer` / `gray-ab` / `shot-diff`）也已收进，剩下载体构建/路线工具、
设备端 shell（按约定保持设备端）与 `knobs/` 超分那批实验脚本还没收。上层的 `game_opt_loop` 那边，`optimize` 总入口、agent skill 与
可视化前端都已经落地，还差的只是系统参数与画面流程改写两层的真机评测——`eval --request`
入口已经落地并真机验证（③ 系统参数端到端已出报告，② 灰档机械链路已通、命中口径已对齐——真机实测命中 27 处 / begins 9195）。写目标形态时要标明状态。

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
契约见 `https://github.com/HGamey/docs/blob/main/repos/phonefarm/docs/SPEC_OBSERVATION_BUDGET.md`。

**决策原语**（零 Token 降级档）

```bash
./phonefarm decide --local --seed 7 --request 请求.json      # 降级档；同 seed 逐位可复现
./phonefarm decide --sidecar "你的桥" --request 请求.json     # 外部进程（Core ML 那一半接在这里）
./phonefarm decide --verify 模型目录 --manifest 清单.json     # 只校验资产，不推理
```

一个 state + 一组 typed question（`choice`/`score`/`noul`）→ 一组 typed answer。
**仓库不含任何模型**：`--local` 那档是降级占位不是模型（概率均匀、未校准），
只在「总比停摆强」这个意义上成立。契约见 `https://github.com/HGamey/docs/blob/main/repos/phonefarm/docs/SPEC_DECISION_PATH.md`。

**设备屏幕观战窗口**（零 Token，只读）

```bash
./phonefarm screen                                    # 浏览器看画面 + 刚点了哪
./phonefarm screen --interval-ms 1000 --actions tasks/局/log.jsonl
```

一个 std-only 的本地 HTTP 服务 + 一份自包含 HTML（无外链、无构建）。
有效观看订阅触发短时采样；每帧在同一设备域强租约内完成，测量占用、排队或恢复待核时暂缓并保留旧图年龄。
服务本身与缓存读取不持锁。子进程超时/退出不确定时保留屏障，不能自动推断设备端空闲。
当前合同见 `https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/OBSERVATION-OWNERSHIP.md`。

**持续灰层控制会话**（源码与宿主替身已验，尚未部署/真机验收）

`gray-layer session --mode MODE --package PKG --serial S --out DIR` 在同一个 owner 内挂层、抓图、有限输入和恢复。
网页、`pf-node session-status/session-action` 与本机 `gray-layer session-status/session-action` 共用有序请求及回执；
连接已有会话不另拿设备锁。请求超时保留原 request_id 并查询，不换 ID 重放输入。
人工接管只改变控制 epoch，lease 仍归原 owner；stop/SIGINT 在有界动作结束后恢复。
失败使用 `gray-layer session-recover --session DIR --serial S --json` 接续原上下文。
局部恢复收据不自动解除整个 job，沿用 fresh lease clear + 核查说明释放流程。
`sample` 动作可在当前场景采1至120秒，保持同一 owner；不自动重启，供电遵循创建时固定政策，风扇/频率不变。
可选 input + frame_id 在同一采样中执行有限计划；无 input 保持 idle。计划必须覆盖 seconds+10、最多600秒，
依最新真实截图核坐标，并在部署后放行前再核图龄/方向；不自动补输入。旧owner缺 sample v2 能力时明确拒绝。
每轮plan和自然完成原件保留，新资源父@3仍可恢复旧C1/C2A/B1会话。
可选 render_comm 必须显式给出；留空仍保留 raw/env，帧指标为未知，不套 Unity 默认。
GPU submit proxy 不等于显示 FPS，首批不按不同窗口推算每帧能耗。采样资源先恢复，之后才完成灰层范围。
会话可在创建时显式选择 --power-policy observe|battery，默认只观察；battery首次采样准备一次并跨轮保持，
原记录绑定当前context。独立恢复供电不代表自然输入/整个会话已恢复；未知时保留屏障及原件。
同期trace电池能量/平均功率是V×I采样积分估计，缺覆盖或放电资格时null；每帧能耗仍待完整分组窗口。
画质检查点使用同一owner的quality begin/checkpoint/finish动作，逐阶段保存当前控制epoch，
完整原图与明确派生的半尺寸预览分开；缺marker保原图且阶段不前进，显式新请求可重采失败阶段。
Web/CLI共读持久manifest；采集完成不代表同场景或画质通过。
旧genshin_quality客户端只连接显式既有session/job，要求尺寸/方向/坐标profile，每个输入前重新截图；
它不启动或重启游戏。重复完整调用返回旧原件，partial/未知不自动重走导航；远程状态缺失与断网分别处理。
合同见https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GRAY-CONTROL-QUALITY.md。
REF_ARM=1参考客户端也只连接显式既有passdump会话，按profile和供电期望执行capture/sample/marker。
默认保留40秒有限输入与30秒采样，WL/CAPDUR可给默认值，LEAD仅支持现有owner固定的6秒；
不安装/重启/停止会话、不推断世界场景或极高精度。完整重复返回旧结果，partial不自动重采。
网页已补整组profile/epoch/label校验及同组回执查询，隐藏或离开页面停止后续提交；合同见
https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GRAY-REFERENCE-CLIENT.md。
四臂refbench已完成源码与宿主替身验收，`gray-layer refbench-loadop --serial S --out DIR` 使用一个Session，
A/B确实未安装层，C/D才安装改写；所有extras为字符串，八份原件与started事实保留。
自然退出、应用自报、改写预期及资源恢复分开；D的创建期effective可以非空，实际begins必须为0。
不测FPS/功耗；恢复入口`gray-layer refbench-recover --session DIR --serial S --json`仅接续原上下文。
实现合同见https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GRAY-REFBENCH-SESSION.md。
旧gray-layer挂层后退出入口已退役（源码与零设备拦截验证），旧keep-prop仅安装不启动语义不能直接alias为session。
typed off/status和历史/离线分析保留；shell off改为同一typed入口，去掉全盘删SO/settings私链。
合同见https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GRAY-LEGACY-MOUNT-RETIREMENT.md。
持续会话合同见
`https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GRAY-CONTROL-SESSION.md`。

**Compute Shader 算子标尺**（零 Token，需 root）

```bash
./phonefarm eval --request <eval_request.json> --serial S --json      # 评测唯一入口: v1 转交 gpu-op; v2 按 kind 路由 shader/sysparam/gray
                                                                     # sysparam 真机已验证 (probe + 1 候选端到端, optimize 可直连);
                                                                     # 自驱动负载 workload.launch = {extras, marker, static_extras} (Megacity city_flythrough):
                                                                     #   sysparam 每臂带 extras 重启应用、等 marker 落地再采; gray 挂层时带 extras 启动;
                                                                     #   gray 的画面比对改为上机前跑一次 storeop-check (static_extras 场景, 孪生 pass 现场开关)
                                                                     # sysparam / gray 每臂开跑前按 conditions.cool_c 等冷 (每 8s 读一次, 最多 300s, 超时不作废),
                                                                     #   逐臂起跑温度记进 conditions_observed.cool_per_arm, cool_reached_c = 最热的一次
                                                                     # sysparam 改写臂施加后 +12s 与负载末尾各回读一次, 对不上 = 被厂商性能服务抢回 (contested),
                                                                     #   那一臂不进统计, conditions_observed.contested_arms 单列; 快照里只有厂商服务会动的
                                                                     #   频率边界 (kgsl.*pwrlevel、cpu.*.scaling_max/min) 变了不判留痕, 记进 snapshot_vendor_drift
                                                                     # gray 命中口径已对齐 (hits = effective[].begins 之和, 真机实测 9195); 注意游戏必须先进入渲染态, 否则 require_hit 如实 ABORT
                                                                     # gray 改写: loadop_dont_care / storeop_dont_care (scope="<frame-audit sig 前缀≥8位>:<附件下标>", 可多条);
                                                                     #   每轮两臂负载前同位置截图比 PSNR, 低于 payload.image_check.min_psnr_db (缺省 30) → ABORT「画面出错」;
                                                                     #   过了才做统计判定 (可判 PASS/REJECT), psnr_db = 最差一轮。真机未验证
./phonefarm gpu-op --request <eval_request.json> --serial S --json   # shader 通路的实现本体 (等冷+锁频+A/B/A/B+Welch t 检验)
./phonefarm gpu-op --serial S --unlock                               # 回滚遗留锁频态
```

当前 gray eval 的 launch 与有限触控 script 走同一父会话；恢复用 `eval recover --session DIR --serial S`。
有限脚本支持 sleep/tap/swipe 及精确旧 `input swipe` 数值格式；不支持 onboard、HID 或任意 shell。
输入中断而缺自然完成收据时，进程消失也不能解除恢复屏障。当前验收层级为宿主替身，不能外推真机恢复。
规格与限制见 https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GRAY-EVAL-SCRIPT-SESSION.md 。

`game_opt_loop` 决策层的物理采样与统计裁决端，契约见 `https://github.com/HGamey/docs/blob/main/repos/phonefarm/docs/SPEC_GPU_OP.md`。
设备条件化与功耗遥测在 `src/hwcond.rs`（与 `bench` 共享），统计在 `src/gpustat.rs`。
设备侧 `vkop_runner` 已构建（`tools/vkop/android_aarch64_vkop_runner`，源码与 `build.sh` 同目录）。
画质补测支持真机参考帧集，回包里 `quality_reference` 记清真值出处。

**性能优化闭环**（零 Token，需 root）

实现在 `loop_v1/`（工具链 + 纯函数解析器），**不是 phonefarm 子命令**。
跑法与五条判据见 `https://github.com/HGamey/docs/blob/main/repos/phonefarm/loop_v1/README.md`；候选改动从 `https://github.com/HGamey/docs/blob/main/repos/phonefarm/docs/MOBILE_GPU_OPT_ROUTES.md` 的清单里挑。

三条纪律，动手前必须清楚：

- **判定规则先于数据冻结**——统计判据在看到候选数据之前落盘，不达标自动回退且不留痕
- **sysfs 写入退出前全部恢复**，进出设备快照逐行相等才算这轮成立
- **解析全是纯函数**，同一份 trace 每次重算必须逐字节一致

帧时序走 raw ftrace 的 kgsl 事件（不侵入游戏进程）。`SurfaceFlinger --latency`、
`gfxinfo`、Perfetto GPU producer 三条路都已在本机实测排除，别再重复试，原因见 `https://github.com/HGamey/docs/blob/main/repos/phonefarm/loop_v1/README.md`。

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
3. **测试环境约束**：VLM 通路与 CTS 通路的 Android 侧统一使用 AVD 模拟器（AVD 名 `agentphone`）；
   OpenHarmony 需在特定的 intel-mac 物理中转端上通过 ssh 控制，连接真机前先核实该物理端
   是否完成本地构建与代码同步。性能测量必须上真机，模拟器的帧时与功耗没有意义。
4. **设备纪律**（性能测量相关）：
   - 同一个候选的 A/B 两臂必须落在**同一台**手机上。跨机比较会把机间差异算进改动的效果里。
   - 设备是共享资源，跑之前用设备锁 `devlock` 抢占（拿锁 → 后台心跳续期 → 跑完释放）。
     被别人占着时排队，不要绕过去硬跑——两个任务同时压一台手机，两边的数都作废。
     锁脚本在 `tools/devlock`（安装：`ln -sf "$PWD/tools/devlock" ~/.local/bin/devlock`）。
     碰设备的子命令自己拿锁（`src/devlock.rs`，通用参数 `--lock-label` / `--lock-wait` / `--no-devlock`，
     label 缺省取 `$DEVLOCK_LABEL` 再缺省 `phonefarm`），不必在外面包 `devlock run`；
     锁已在同一 label 手里时原地执行，所以外面包了 `devlock run <label>` 也不会自己等自己。
     lead 预排了队列时 label 用队列里的名字。
   - 影响稳态的环境条件（风扇开关、冷机阈值、供电状态）逐轮记进证据。这些不是无关细节，
     条件不同的两批数据不能混在一起比。
   - 规划中的设备构成：安卓旗舰机（一台跑测试、一台留给调试，要并行跑多条线得是同型号多台）、
     鸿蒙手机 1 到 2 台做对照（无 root，只能测）、海外版华为手机几台（EMUI，麒麟芯片，能 root，
     用来在麒麟上测 Vulkan 层与系统参数的改动效果）。这是计划不是现状，
     不要按「已有这些机器」来写流程。
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
9. **版本控制与提交**：用户授权小批直接 commit/push，不新开 PR、不 force push。
   有远端并发变更先 pull --rebase 并重验。部署与提交分开；部署时原子替换产物，防止进程踩踏。
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
  refbench/            白盒靶子的驱动侧（靶子本身在同级 refbench 仓库）；判定链已知答案自检 phonefarm selftest 的用法也在这里
  auto/                自动挑旋钮：白名单探测 → 冻结规则 → 大模型选参 → 逐组对照
knobs/                 可改动面：黑档系统参数 + 灰档 Vulkan layer 注入（由 loop_v1 驱动，
                       说明见 https://github.com/HGamey/docs/blob/main/repos/phonefarm/knobs/README.md；原 HGamey/knobs 仓库已归档并入，shell 正在往 Rust 收）
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
- 核心架构契约见 `https://github.com/HGamey/docs/blob/main/repos/phonefarm/docs/DESIGN.md`；任何核心功能升级或协议重写，必须先做 SPEC 定义，并与既有架构保持一致。

## 代码仓库只放代码

- 不要在本仓库新增或修改说明文档、图片、PPT、PDF、实验报告。
- 这次改了什么、为什么改：写在 commit message 正文。
- 一个功能或修复的说明、验证与限制：写在 commit 正文；完整报告和截图归 HGamey/docs 或本机证据目录。
- 长期文档（设计、规格、架构图、进展汇报）：提交到 https://github.com/HGamey/docs ，本仓库的旧文档在那里的 `repos/<仓库名>/` 下。
- 允许的例外：根目录 README.md（40 行以内）、AGENTS.md、skills/、许可证、程序或测试要读的数据。CI 检查 `code-only` 会拦下其他文档类文件。

## 直接提交与团队开发

- 基于当前 main 分批开发，保留同事修改；先将同一提交推到 `verify/<批次>`，通过必需的 `code-only` 检查后直接推 main。
- 不关闭必需检查、禁止强推或删除规则；rebase 改变提交后重新检查。
- 主 Agent 聚焦系统结构、协议与整合；前端等独立模块可交具体 subagent，划分文件所有权。
- 每批记录已做、未做、测试层级和恢复义务；模拟通过不等于真机或全流程已可用。
