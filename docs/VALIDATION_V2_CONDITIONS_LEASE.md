# #17 离线验证记录

2026-10-11；base origin/main e3b7c35；codex/v2-conditions-lease。
没有手机操作、模型/API 调用或付费操作。输入帧数、温度、电池、PID 等夹具不能当实测结果。

## 执行

```
cd src && cargo test
cargo clippy --all-targets
```

结果：513 unit tests + 11 integration tests，通过（含真实 GOL 类型序列化合同回归）；clippy 完成（仓库既有警告仍存在，
新增 devlease/shaderplan/integration test 模块无警告）；git diff --check 通过。
测试直接执行 Cargo 构建的 phonefarm 二进制，使用隔离临时宿主目录与 fake serial，
没有借助真实 adb/hdc。一次约 8 秒。

## 逐项证据

| 验收点 | 可复现测试 |
|---|---|
| 真正消费 shader plan，不复刻旧映射 | eval::tests::shader_v2_maps_to_v1_request；实际 GPU args parser 核对 refbench/frames/intensity |
| carrier/frames/intensity/停充/放电/参考帧/serial | shaderplan::tests::maps_every_consumed_refbench_field |
| seconds 显式、禁止默认时长 | shaderplan::tests::headless_seconds_are_explicit_no_frames_no_defaults |
| unsupported 注入/温控/风扇/载体/自定义节点，拒绝设备 clamp | shaderplan::tests::rejects_conditions_without_implementation |
| 帧数、载体配置、postfx unknown 或不符不得通过 | gpuop::tests::v2_refbench_actual_unknown_or_mismatch_cannot_pass_conditions |
| unknown 供电不成为 true | gpuop::tests::unknown_battery_observation_is_null_not_request_echo；hwcond::tests::unknown_battery_status_never_certifies_discharge |
| 新真实 CLI 不冒充完整游戏资格 | actual_eval_route_writes_consumed_plan_and_honest_abort_without_phone；unsupported_injection_returns_specific_abort_before_artifact_or_device |
| GOL 必需 fan unknown + Inject 对象通过计划；carrier_native 限白盒 SR，layer 注入拒绝 | typed_gol_native_route_does_not_turn_unknown_fan_into_observation；actual_typed_gol_request_reaches_native_plan_and_blocks_unimplemented_layer_without_device |
| 两个进程/不同工作目录共享租约，无交错 | two_real_processes_do_not_interleave_across_cwds |
| 超时与受控错误释放，文件 inode 保留 | timeout_preserves_other_owner_and_failure_releases |
| kill 父后内核释放，但活 child/未知手机状态仍阻断 | crash_releases_kernel_but_abandoned_owner_blocks_live_child_and_unknown_device |
| 5 秒心跳、owner 被替换不能覆盖/删除 | heartbeat_renews_and_live_or_replaced_legacy_owner_is_not_reclaimed |
| 手动旧锁、无效 PID、活 PID 不回收 | unknown_manual_lock_is_never_deleted_even_when_old；invalid_pid_marker_and_live_owner_cannot_be_recovered |
| 实际 eval/sysparam、shader/gpu-op、perf 写入口尊重旧锁 | eval_sysparam_entry_respects_existing_manual_lease_before_device_access；gpuop_adapter_and_perf_cannot_write_through_a_manual_lock |
| 嵌套 perf 只能借用相同目标的父租约 | perfsrc::tests::nested_perf_requires_exact_parent_target_before_any_device_call |

## 限制与后续真机验收

- 此分支与 PR #18 独立。集成时需保留该 PR 的资格门禁，特别是离线评估路由先于
  设备租约；main.rs 新 mod/lease match、eval/autoloop 的入口 guard 可能需要人工对齐。
- 所有 v2 shader 报告 qualification.eligible=false，scope 仅 operator_benchmark_only /
  refbench_measurement_only。不能由旧 PASS 自动晋级成完整游戏优化证据。
- power_window_verified=false，只记录起止供电。真实 display、连续帧质量/部署尚缺证据。
- 本轮不是新 present 采集器、黑盒游戏注入器或鸿蒙 root 支持。unsupported 必须 ABORT。
- 旧 devlock 是全局目录，为兼容会串行不同手机。等待为有上限轮询，尚无 FIFO 公平性保证。
- Abandoned 的 --confirm-device-idle 是操作者确认，日志 device_verified=false；没有假装
  自动证明远端负载停止。真机恢复和空闲确认仍由操作者完成。
- 原始 adb/hdc、另一宿主、VLM/裸 script/旧 shell 可绕开合作锁，真实实验前仍需排除。
- 上游 GOL 发 v2、两台设备/并发任务真机测试、恢复与供电读回验收由系统 issue #17 跟踪。
