# 单轮确认观测生产合同（v1，离线入口）

状态：已实现的离线验收入口；不采手机，不授予优化结论或部署资格。
依赖实验资格合同 PR18；对接 game_opt_loop confirmation PR29。

## 目的和边界

`confirmation-observation --bundle B.json --out R.json` 重读并校验一轮
`loop_v1/tools/run_once.sh` 的原始 trace/env/workload_exit 和采样 driver 留下的上下文。
每臂是独立观测，不借用包含完整 A/B 的 conclusion 报告。`VALID_OBSERVATION`
仅表示本轮记录符合冻结的采样合同；`qualification.stage=experiment` 且
`qualification.eligible=false` 永远不变。完整配对次数、顺序、置信界和多指标裁决在
上层独立确认 judge；本入口不做 KEEP、晋级或真实游戏优化声明。

v1 只验收 Android root + sysparam 白名单控制 + raw KGSL 提交节奏 + 电池 sysfs。
提交节奏是 `gpu_submit_proxy`，不是 present/display 真帧率；不支持 display、shader、
gray 注入、质量改变或未实现的控制。所有来源是保存记录的声明及字节绑定，未做可信
设备身份/时钟采集证明，因此 `real_game_optimization_proven` 和 `promotion_allowed`
均为 false。离线输出来源为 `recorded_device`；synthetic/unknown 保持 BLOCKED。

## 输入：文件证据，不接受手填 gates

本入口新增 JSON schema 拒绝未知字段；嵌套既有 Policy 用现有 validate 验版本/规则，
whitelist 是被忽略的工件 metadata，不能提供任何门禁。每个文件引用是 `{path,sha256}`，SHA 为小写 64 位。
path 只允许相对 manifest 所在目录的普通组成部分，拒绝绝对路径、`..`、符号链接和
非普通文件。Unix fd 以 O_NONBLOCK/O_NOFOLLOW 打开，open 后检查 regular，避免
stat/open 间换成 FIFO 或符号链接；top bundle 同样使用 take(limit+1) 有界读取。
JSON 2 MiB、raw 128 MiB、累计缓存证据字节 512 MiB 上限，读一次后对同一字节做
哈希、解析和裁决。该累计字节门槛不是 RSS 上限（还含解析结构/临时文本）；
哈希不是 filesystem 所有权证明。输出不覆盖旧文件。

顶层 `phonefarm/confirmation_bundle@1` 包含三份引用：`run`、`baseline_admission`、
`frozen_plan`。后两者不能替代原始 run 记录。

### conditions

`phonefarm/confirmation_conditions@1`：`device_serial`、`device_boot_id`、
`snapshot_cpu_policies`（已知非空 policyN 列表）、`render_comm`、`capture_seconds`、
`frozen_at`（RFC3339），及现有 qualification `policy`。
policy 在基线之前冻结，阈值由收端现有合同验证，未知温度/冷机 cap 不能通过。
Frozen plan 的轮数和 family_alpha 必须与 policy 一致；目标与 required_metrics
保持收端规定。v1 不支持 require_quality=true 或 display_smoothness。

### selected artifact

沿 v2 sysparam 形状 `{kind:"sysparam",payload:{params,plan}}`。params 是 id→值字符串
映射，plan 沿 GOL SysparamPayload 是 `{kind,path,value}` 数组，deny_keywords 及可选
why/whitelist 保留在工件哈希中，不能替代收端规则或原始 probe。非空候选必须从原始
probe 重建 whitelist，再调用现有 validate_candidate 和 plan_text，转三列文本后字节
完全匹配。基线允许空 params/空 plan 数组。
快照的 touched keys 必须可读，起/末施加态都与期望值相同，未触碰 key 不得变化。
这只验证采样边界读回，不证明整窗频率恒定或 driver 没有重写；回包明确限制。

### run

`phonefarm/run_one_bundle@1`：`evidence_origin`、`phase`（admission/confirmation）、
`run_id`、`pair_id`（admission 用 null）、`sample_role`（baseline/candidate）、
`device_serial`、`started_at`、`finished_at`，及 `conditions`、`artifact`、`raw` 引用。

raw 必须有以下保存文件，不可用 summary.json 或调用方布尔值替代：

| 字段 | 内容 / 检验 |
| --- | --- |
| trace | run_once 的 trace.txt；用现有 looptrace 重算 summary 与 SPF/ACF |
| env | sample_env.txt；用现有 env_stats/sample_window 验整窗、热区、放电 |
| workload_exit / sampler_exit / capture_exit / restore_exit | 各子进程真实退出码，必须 0 |
| root_id | root 探测标准输出，必须恰好有 uid=0 token |
| boot_id | `/proc/sys/kernel/random/boot_id` 保存值，须与冻结 conditions 相同 |
| cold | driver 保存的 `{checked_at,zone,c,waited_s}`；有效 SoC zone、已知温度低于 cool_c，时间在本轮开始前 60 秒内 |
| probe | probe_sysparam 原始文本，用于重建白名单 |
| snapshot_before | 施加前设备全量快照 |
| snapshot_at_run / snapshot_after_run | 原 run_once 两份采样边界快照，验证施加态一致 |
| snapshot_restored | 回滚后设备全量快照，须与施加前逐字节相等 |

快照需完整 `# device_snapshot v1` / `# end` 和 driver 保存的 `#adb_exit=0` 尾部 receipt，
有唯一 key，device_snapshot.sh 的 31 个固定 key（GPU/bus/wm/settings/ftrace）均存在。
冻结的每个 CPU policy 都须有 governor/scaling_min/scaling_max，且原始 probe 的
cpu.policies inventory 与该冻结清单一致。未触碰固定/CPU 域可保留 `<unreadable>`，
不将其说成已观测值；触碰和波及兄弟域必须可读。ftrace 的四个恢复字段可读
（set_event 空字符串表示无事件，允许）。报告恢复 scope 是这份明确键域的字节相等，
不授未列设备节点或不可读域的恢复证明。
触碰字段及波及兄弟字段必须可读。复用 existing snapshots_identical，并额外拒绝截断、
重复 key 和缺恢复域快照。raw trace 的 capture uptime 与 env uptime 须覆盖同一窗口，
采样与 capture 子进程均成功；忙碌工作负载提交跨度至少 capture_seconds-1 秒，
且不超过 capture metadata 窗长度，防止仅修改 duration metadata 给短片段授整窗资格。
`trace_window_verified` 的准确范围是 `/proc/uptime` capture metadata 和 ENV 采样
覆盖，再加忙碌提交跨度检查；没有 trace_clock receipt，**不跨时钟声称事件位于该窗**。
ftrace event 时钟位置未知明确进入 limitations，不能把跨度匹配当定位证明。
电池均值来自完整 sampler 窗（覆盖 capture），不冒称按 event 时钟裁剪的窗内均值。
规则不适用于睡眠/低活动任务，故它们目前 BLOCKED，不猜测缺数据。

host 时间、boot_id 和来源均是 driver 记录声明，不是密码学采集认证。

### baseline admission

`phonefarm/confirmation_baseline_admission@1`：`conditions`、`baseline_artifact`、
`runs`（至少 3 份原始 admission run bundle 引用）。不接收 Evidence、conclusion 或
预填 baseline_admission_verified。原始 bundles 每轮按上述规则重算，role 必须 baseline、
phase 必须 admission、artifact/conditions/serial/boot 均相同。run ID、trace/env hash
不得复用，host 时间及 uptime 不重叠且均早于 GOL 注册；它们的 ID/hash 必须在冻结
exploration 排除清单中。用现有 Stage::Experiment qualification 重新计算重复性、
漂移及采样资格，只有 experiment_allowed=true 才准入。同 boot 确认 capture 的
start uptime 必须大于最后 admission capture 的 end uptime，不能仅凭 host 时间允许
设备时钟倒退。admission 文件自己的 SHA
必须等于 frozen.plan.baseline_admission_sha256。

ENV 协议 duration 和实际 uptime 跨度都不得超 driver 的本轮 host elapsed（仅比较
时长，不跨 host/device 绝对时钟）。admission 每轮 **整 ENV 窗**也必须不重叠，不能
让不同 SHA 的共享长遥测覆盖多个短 capture 冒充独立基线。确认整 ENV 窗也必须晚于
最后 admission ENV 窗。所有确认臂间的独立性由 GOL 按冻结 schedule 检查这些窗。

### frozen plan

严格对接 `game_opt_loop/confirmation_plan@1`，镜像其 typed 序列化字段顺序验证
SHA-256(JSON([schema,registered_at,plan]))，并验证角色、artifact/conditions、固定
schedule、原始 exploration 排除和 registered_at < started_at < finished_at。上层 frozen
metrics 必须包含收端 goal 的所有 required_metrics（frame_p95 两种别名只能选一个），
不能采三项却裁决时静默只留一项。
v1 收端最多 32 pairs；不接受上层通过增大 alpha 或改阈值放宽规则。

## 输出

始终输出 `phonefarm/confirmation_observation@1`、`source=phonefarm`、
`status=VALID_OBSERVATION|BLOCKED`。输入有错也提供 BLOCKED JSON（退出码 1）；
输出已有文件或不可写时失败且不覆盖。

`sample_qualification.schema=phonefarm/confirmation_sample_qualification@1`，
`valid_observation` 只有以下 7 个由原始文件重建的 gates 全为 true、所需指标与
来源完整、冻结绑定合法且无其他错误才为 true：root_verified、control_verified、
cold_verified、trace_window_verified、workload_succeeded、restore_verified、
baseline_admission_verified。同步输出 baseline_admission_sha256。

confirmation_binding 包含 pair_id/run_id/sample_role、aggregation_level=single_run、
artifact_sha256/conditions_sha256/frozen_plan_sha256、started_at/finished_at 和
raw_artifacts.trace_sha256/env_sha256。metrics 用 GOL 名称 fps_mean、frame_p95_ms、
fps_p95_ms、power_watt；缺值为 null。metric_provenance 保留 KGSL inferred/proxy/
display_verified=false；power 只有原始整窗确为电池放电时 window_verified/discharging
才为 true。报告保存单轮和 admission 资格详细 checks、文件哈希与范围说明。

measurement_window 固定形状：`{schema:"phonefarm/measurement_window@1",device_serial,
boot_id,capture_start_uptime_s,capture_end_uptime_s,env_start_uptime_s,env_end_uptime_s,
env_duration_s,env_interval_s,clock_scope:"capture_metadata_and_env_proc_uptime"}`。
单位均秒；VALID 时数值有限且非负、start<end、ENV 覆盖 capture，BLOCKED 可为 null。
它只属于 capture metadata/ENV `/proc/uptime` 时钟，不能当 ftrace event 时钟位置证明。
GOL 按 frozen 顺序检查同 serial/boot、capture 与整 ENV 窗都单调不重叠。

## 当前 driver 缺口与验收

现有 run_once 已保存 trace、两份轮内快照、workload_exit；现有 eval 保存 env。
它们尚不持久化统一 cold/root/boot/采样退出与恢复 receipts，以及冻结角色/host 时间/
admission 排除清单。旧目录不能自动升级成 VALID；需要后续 heldout driver 在设备
lease 内按合同采集全部记录。此轮不添加设备 driver，不碰 ADB/HDC/模型。

离线验收：真实 CLI 重算原 trace 格式；合成来源始终 BLOCKED；逐一删除/改错 root、
控制读回、冷机温度、采样完成/退出、负载退出、恢复快照、admission、冻结字段、角色、
raw hash、chronology，皆 BLOCKED。拒绝 aggregate admission 和手填 gates。
假 adb/hdc/model 哨兵验证入口零外部调用。与 GOL 新消费端实际 shape 互通，仍不自动晋级。
