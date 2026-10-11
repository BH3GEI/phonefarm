# Shader v2 条件与设备租约

状态：离线实现与验证；尚未真机验收。关联 HGamey/.github#17、#11。

## 请求与事实

shader v2 不再启另一个 phonefarm 进程。纯函数生成冻结执行计划，gpu-op 在同一
进程消费。支持 headless 的 seconds；支持 refbench 的 frames/intensity 与内置
postfx 候选算子路线。serial 的 CLI 覆盖优先于请求，随后解析唯一在线设备并固定。
quality_reference 优先 payload，再 conditions；同时给不同值拒绝。
`examples/gol_shader_native_request.json` 由 game_opt_loop 的真实公开 Request 类型序列化
生成（GOL 76a9f183d990，Conditions.fan 与 ShaderPayload.inject 的类型来自 src/optimize/eval.rs），
用于跨仓接口回归；假设的候选/缺失 SPIR-V 不包含任何实测或成功部署证据。

| 条件 | 支持范围 |
|---|---|
| carrier | headless、refbench；game_replay 明确 ABORT |
| seconds | headless 必须正整数；refbench 不消费 seconds，非 null 明确 ABORT |
| frames / intensity | refbench frames 为 60..100000、intensity 为 1..64（显式拒绝载体 clamp 范围外值）；headless 非 null 明确 ABORT |
| script | headless 仅 null/空；refbench 仅 null/空/sr_pipeline |
| cool_c | 明确给出 (0,100)，逐臂等冷采样 |
| require_root | shader 总是要求 root；false 无法执行，明确 ABORT |
| require_discharging | true 强制逐臂前后电池放电证据；不等于整窗认证 |
| charging_suspend | enabled=true 转 --suspend-charging；仅自动已知节点，指定 node 拒绝 |
| payload.inject | 支持 GOL 的对象合同 `{via:"carrier_native",layer:null/省略}`，仅 refbench 的 sr_pipeline/sr postfx；headless、frame_gen、vulkan_layer 及非 null layer 明确 ABORT。兼容旧 false/null；true 明确 ABORT |
| conditions.inject | 兼容旧 false/null；true 无实现，明确 ABORT |
| fan | GOL 必需 String 的 unknown（以及省略/null）只声明未知，无风扇控制，resolved/observed 均 null；on/off 等非 unknown 请求明确 ABORT |
| temp_cap_c / lock 等额外条件 | 尚无完整 shader 实现，非 null 明确 ABORT |

三个字段不混用：`conditions_requested` 原始意图（payload 注入另记 `injection_requested`）；`resolved_config` 代码确实消费的
配置，不能作为实测；`conditions_observed` 只装手机读回/采样，包括逐臂起跑温度、
电池前后状态、锁频控制读回、恢复读回。未知留 null；失败没有事实则空列表。
供电前后检查不能证明整个测量窗始终放电，报告明确 `power_window_verified=false`。
没有 display/present 采集，不把 refbench 的 KGSL proxy 说成真实显示帧率。
不支持的请求在设备操作前给 v2 ABORT；非 PASS gpu-op 的合法报告仍保留。
refbench 返回帧数、params.frames/intensity、scene/run_id 必须对应请求，且 postfx.active 必须逐臂对应；缺字段留 null 并 ABORT。

refbench 原生 LR/HR 导出的 run 带 `capture_mode="quality_only"` 和
`performance_eligible=false`：额外 HR draw、readback 和同步会改变负载。
gpuop 的 legacy/v2 路径都必须在纳入性能样本前拒绝此类 run。
当前普通性能合同只接受这两个字段均缺省的旧/默认 refbench 回包；
任一字段出现（包括 null、类型错误、未识别模式或单独的 true）都 ABORT，
不能自行推断新模式是正常性能路径。原始字段保留在 observed 回读中。
频率控制读回缺失/不符、前后供电门槛未满足、停充恢复失败也 ABORT。

`measurement_scope` = `operator_benchmark_only` / `refbench_measurement_only`。
这两条标尺均不能证明完整游戏结论，因此所有 v2 shader 报告显式给出
`qualification.stage=experiment, eligible=false, can_claim_improvement=false`。
PASS 只表示旧算子预算/静态质量/统计筛选通过；上层不得晋级为游戏级优化成果。
整窗功耗、真实 display、配对连续帧质量/部署证据仍属真机后续门禁。

## 自动租约

设备写操作入口 eval(sysparam/gray)、gpu-op、bench、capture、autoloop、gray-ab、
gray-layer(除 status)、perf(除离线 CSV) 在设备操作前拿租约，完整实验、A/B 和恢复都在同一个 guard
作用域。shader 的 eval 由 gpu-op 拿一次；内层 script 的回放由外层保护，无重复锁。gray-ab 的 perf 采样改为同进程显式借用父 Lease，scoped thread 在父恢复/释放前 join；没有环境变量绕过锁。
解析后设备号写回执行参数，禁止先锁一台再自动选择另一台。

`PHONEFARM_LEASE_DIR` 默认系统临时目录下 `phonefarm-device-leases`，不依赖仓库/cwd。
按完整 backend+serial SHA256 命名文件，内核 `File::try_lock` 排他锁，文件永不删除，
避免等待者持有旧 inode。等待有上限，默认 600 秒，可用
`PHONEFARM_LEASE_TIMEOUT_MS` 调整。5 秒 owner 心跳。返回/异常展开/进程退出时
内核释放文件锁；正常返回同时清理本次 owner。需要 Rust >=1.89。

legacy `devlock.d` 位于 `DEVLOCK_DIR`（默认 /private/tmp/claude-501）。自动租约
用同样的原子 mkdir 协议桥接并维护 owner，另持有共享 bridge 内核锁。
为兼容这份旧的全局锁，不同设备也会保守串行；将来 devlock 迁到按设备命名后才可并行。
手动 devlock 持锁不能凭 label 假认领。必须释放外层手动锁，再由入口自动持有；
占用超时给明确错误。自动产生的遗留目录在 parent 退出后标为 ABANDONED，默认禁止新实验，即使内核锁已经释放。
不能把 parent PID 不存在推成设备空闲：本地 child/远端采集/系统设置可能仍未恢复。
使用 `phonefarm lease --serial S --recover-abandoned --confirm-device-idle` 前，操作者须
独立确认负载已停止、手机状态已恢复；此确认作为 operator_assertion 落日志，device_verified=false。
仅在 marker 的 producer/serial/token/完整 owner 匹配且 parent 和记录的 child PID 均已不存在
时接受确认。PID=0、ps 错误/权限错误、活 child、未带可验证 marker 的他人目录
（即使心跳老）绝不删除。不同 serial 不能回收该目录。wrapper 记录 child PID。
`phonefarm lease --serial S --timeout-ms N -- COMMAND ...` 可保护其它本地工具，
不要在此 wrapper 里再次调用自动租约的 phonefarm 命令，否则明确等待超时。

wrapper 在 `spawn` 前持久化 pending launch。只有本次 `Command::spawn()` 明确
返回 `Err`（没有建立 child），且本次 owner/token、pending operation 完整匹配、
无 child PID/PGID 时，才可持久化撤销该 pending，随后释放本次 legacy marker。
撤销写入或身份核验失败仍保留屏障。成功 spawn、绑定失败、等待/进程组状态未知、
父进程中断及恢复要求不走此撤销路径；它不证明设备状态或远端工作已恢复。

只保护同一宿主上合作入口。直接 adb/hdc、旧 shell 工具、另一台 Mac、UI/VLM/裸
script 仍可能绕过；真机验收必须确认没有这些并发负载。SIGKILL 释放内核互斥但保留 abandoned 屏障，
不会自动恢复手机 sysfs，也不能证明远端工作已停止；先按既有流程恢复，确认空闲后才可解除屏障。
本轮不把离线互斥测试作为物理实验没有污染的证明。
## Persistent marker validation

Every host marker consumer validates the recovery state before device commands,
ownership transfer or deletion. Present `command_pending`/`recovery_required`
fields must be JSON booleans. Missing booleans remain compatible with the older
format-1 lease that did not emit them; null/string/number values are UNKNOWN.
`child_pid` and `child_pgid` must be absent together or both positive integers
representing the same process-group leader, within the signed platform PID range.
A dead leader with a missing group cannot establish local quiescence.

Malformed state must be retained by Drop, normal admission, explicit driver
reconciliation and manual idle recovery. An operator idle assertion cannot fill
missing local group identity or clear a malformed/new driver barrier. Marker
validation itself is neither remote quiescence nor device restoration evidence.

Format-1 recovery is a closed top-level schema: only `format`, `producer`, `pid`,
`token`, `serial`, `label`, `owner`, `command_pending`, `pending_operation`,
`child_pid`, `child_pgid`, `recovery_required`, `recovery_context` and
`previous_owner` are accepted. Unknown fields or another format require an
upgraded consumer; they must not be discarded by normal admission, Drop,
manual idle recovery or recovery ownership transfer. This prevents an older
consumer from silently ignoring a newer recovery barrier. Historical
`previous_owner` is retained for audit, not used to authorize recovery.
