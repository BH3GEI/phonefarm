# 实验与结论资格合同 v1

目标：在应用候选或提名模型之前回答「这个设备、负载和目标是否具备实验条件」，
在统计保留之前回答「证据是否有资格支持这个目标」。资格通过不是优化成功。

## 冻结目标

收端在候选数据之前生成 `qualification_policy.json`，阈值由本仓库固定。
`qualification.goal` 可显式选择目标，缺省 `render_efficiency`：

| 目标 | 必需指标 | 能支持的结论 |
| --- | --- | --- |
| render_efficiency | frame_p95、fps_mean、power_w_mean | 渲染提交节奏和电池侧能耗 |
| render_cadence | frame_p95、fps_mean | 渲染提交节奏；不宣称节能 |
| display_smoothness | frame_p95、fps_mean | 实际显示帧流畅度，要求 display/verified 来源 |

不得因功耗采不到而在候选数据出来后删掉必需指标。灰档在 conclusion 另需 `quality_verified`；
当前 gray 收端没有连续帧质量链路，因此保持未验证。未来可接独立质量报告。
alpha 由接收端限制为 (0,.05]；请求可更严，不可放宽。回包记录 alpha_actual。

## 三个阶段

- probe：只读 root 和 trace 能力。root 检查必须在任何白名单差异写探测之前。
  每次实际能力探测原子创建独立私有临时目录（Unix 0700），本调用的 root/trace/
  measurement 顺序只共享该目录，禁止使用宿主全局 `_cmd.out` 或 `_hdc_cli.out`。
  创建失败返回 unknown 能力，不得授权写入；结束只删除本调用已知输出和空目录。
  并发不同设备的输出不能相互作为 root/trace 证据，此隔离不改变任何资格阈值，
  也不是设备来源认证。
- experiment：要求控制面已验证、负载自检通过、冷机成功、基线恢复成功；至少 3 轮完整
  基线。各必需指标 `(max-min)/median < 5%`，上升或下降的单调比例均不得达到 90%。
- conclusion：重复上述检查，并要求两臂各达到冻结轮数、每个必需指标齐全、每臂负载退出码为0、热区有效、
  供电/来源有效、候选恢复成功、灰档质量独立验证。资格不通过不得 KEEP/PASS。

Policy 同时冻结 `temp_cap_c / cool_c / baseline_min_runs / dispersion_max_pct /
monotonic_max_ratio / minimum_pairs_for_conclusion / alpha`。eval 请求温度为 null 时保留 unknown，
不代填温度；autoloop 在候选前按既有公式派生温度并写入 Policy。每轮热区采样须完整且
最高值不超过冻结 cap；缺温度或部分采样不能视为安全。

UNKNOWN 不等于 true。输入默认 evidence_origin=unknown；只有 live_device/recorded_device
和允许的实际采集来源能取得真实实验资格。synthetic/static_estimate 不合格。输出 `ready / measure_only / blocked / unverified`，分别带
`probe_allowed / experiment_allowed / conclusion_allowed` 和逐项原因。机器消费 `eligible`
等同于 conclusion_allowed：只表示证据资格，仍须统计 win 才能保留，probe/experiment 一律 false。
无 root 鸿蒙即使能采 SP_daemon，也只能 `measure_only`，不会变成完整优化能力。

## 测量语义

KGSL `adreno_cmdbatch_submitted` 分组得到的是 `gpu_submit_proxy`，不是 present/display。
输出保留来源、`submits_per_frame`、`inferred/unknown` 置信；无自相关峰而默认一次提交
一帧时置信为 unknown，不据此裁决。只有 raw_ftrace_kgsl + gpu_submit_proxy + 可验证SPF/ACF分组才支持渲染资格。
当前没有直接 display collector，因此 display_smoothness 始终不能满足，不能由输入字符串
伪装 verified。每轮保存原始 trace hash，来源字段是采集元数据，不是硬件身份认证。

功耗必需电池轨、前后非充电状态、每条有效采样电流为负、每条 V×I 在共享可信区间内，
并且解析完整。采样须完成 marker、成功退出、DUR/IVL/逐条uptime/n完整且覆盖 ftrace
实际 capture_start/end_uptime；一条合法数据不能补成整窗。功耗均值的范围明确是含
lead/tail 的整个固定采样窗，覆盖 trace 窗，并非恰好 trace 时段的平均。
只有一个均值或采样前状态不算整窗放电证据。
旧 ENV 记录缺少采样后状态时仍保留原始 V×I 供复盘，`power_w_mean` 为 null。

## 离线入口

`phonefarm qualify --from evidence.json --json` 完全不访问设备；输入包含 version=1、
stage、evidence_origin、policy、device、workload_verified、cold_ok、restored、quality_verified、baseline、
control、candidate。运行记录使用现有 `{label,summary,env}`，另带 `frame_evidence`。
`examples/qualification_ready.json` 是明确标注的构造夹具，不是真机验证；CLI 返回 blocked/eligible=false。
正例单测使用显式假设的canonical元数据，仅验证逻辑，不证明真机性能。

`phonefarm eval --request request.json --qualify-from evidence.json --json` 复用实际 eval
请求校验与目标冻结，再进行同一资格判读，返回 `qualification_only=true`；不应用候选，
不输出候选 PASS。可离线核验 eval 入口确实接入门禁。

退出码：0=当前阶段资格通过；1=blocked/unverified/measure_only；2=输入或参数错误。
live eval 的 v2 回包在 `qualification` 中记录资格结果，来源在 `metric_provenance` 中，
兼容现有信封。控制验证范围是差异写探测/施加 readback，不证明全程持续生效。
gray 尚无独立控制面与质量证据；可采基线，但挂层候选实验会明确 ABORT。
v1 shader/gpu-op 的专用裁决暂不迁移，仍按其既有合同；本迭代不声称覆盖全部上层入口。

## 验证与边界

离线测试需证明：缺 root 时没有写探测；probe 恢复失败不报 REVERTED；必需指标缺失
即使另一个指标显著改善也不能 KEEP；冷机/基线/供电/来源/恢复未知或失败阻断结论。
每轮基线、每对候选与会话收尾等冷都必须通过；收尾失败撤销已有 KEEP 并重写资格证据。
不新增 present 采集，不做真机性能宣称，不把「没有显著退化」写成非劣效证明。
