# 旋钮接口契约

与 loop_v1 的 `knob_ddr_boost.sh` 同构。harness 只认这三态，不认实现是 shell 还是 layer。

## 三态

```
<knob> apply      # 施加。幂等：已施加再调不报错。把原值存进 state 文件
<knob> restore    # 回滚。读 state 回写并回读比对；成功后删除 state。无 state 时空转
<knob> status     # 只读打印当前值 + 是否处于施加态
```

## 可逆纪律（loop_v1 判据 4 会验）

- `restore` 之后，`device_snapshot.sh` 的 38 行快照必须与 `apply` 之前**逐行相等**。
- state 文件存在 = 「处于施加态」。它既是回滚依据，也是崩溃恢复依据。
- 每次写 sysfs / settings 后**回读比对**，写不进就如实报 `KNOB_FAIL`，不假装成功。

## 自报输出（stdout 行标记 + 可选 JSON）

行标记（供人读与 grep）：

```
KNOB_APPLIED <项>: <旧值> -> <新值>
KNOB_FAIL    <项>: 想写 <目标>, 回读 <实际> (原值 <旧值>)
KNOB_SKIP    <项>: <原因>          # 该项在本设备不可用
KNOB_RESTORED <项>: -> <值>
KNOB_ALREADY_APPLIED / KNOB_NOT_APPLIED
```

灰档 layer 另写一份 JSON（应用外部 files 目录，harness 用 root pull）：

```json
{
  "knob": "gray_loadop_dontcare",
  "layer_loaded": true,
  "effective": [{"pass": 3, "field": "loadOp", "from": "LOAD", "to": "DONT_CARE"}],
  "failed": [],
  "unavailable_reason": null,
  "readonly_stats": {"frames": 3000, "render_pass_begins": 24000, "per_frame_avg": 8.0}
}
```

`effective` 为空且 `unavailable_reason` 非空 = 旋钮本次没生效（例如层没挂上）。
harness 据此判断该轮是否算数——**层不判断改得好不好**。
