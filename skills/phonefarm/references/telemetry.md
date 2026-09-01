# phonefarm 遥测参考

## 采集形态

- 每步一条 `r=telemetry` 记录，统一进 log.jsonl（无独立 telemetry.log）
- 高频字段每步采；重量级明细按 `telemetry_interval`（默认 5）步并批
- 整批命令生成设备端 sh 脚本，开局一次性上载，每步一趟 shell 往返
- 全部限时、失败不 panic、不进模型上下文

## 十层字段

1. **系统层**：CPU% / load / 每核频率 / 内存 / 电池(含 µA) / 16 路温度 / GPU / 存储 / WiFi / 网络连接
2. **渲染层**：帧数累计→fps 差值 / Janky% / p50-99 分位 / MissedVsync / VSYNC 周期 / 图层数
3. **App 层**：pid / topActivity / meminfo 明细 / 线程数 / VmRSS / VmHWM / 冷启动耗时
4. **Root 层**：进程 IO / fd+socket / smaps 聚合 / 进程网络 / cgroup / dmesg / tombstone（开局 id 探测降级）
5. **PSI**：cpu/memory 压力 some/full avg10/60/300
6. **每 App 流量**：uid 级网络流量
7. **传感器/外设**：活动传感器 / 定位请求
8. **IPC**：IPC 统计
9. **Host 层**：每步耗时 / API 延迟 / 截图体积 / UI 节点数 / OCR 触发（树失效标注）
10. **事件层**：crash / anr / fd 增长 / 连接数变化（两次读数差 → `r=app_event`）

## 关键实现点

- Android：smaps 聚合用 `smaps_rollup`；冷启动用 `am start -W` 白拿 WaitTime
- OH：hidumper 服务冷启动首跑超时，heavy 限时 20s（每 5 步才付一次）
- config：`telemetry`（默认开）/ `telemetry_interval`
- 查看：`phonefarm show <局ID> --step N`（单步快照）/ `phonefarm stats <局ID>`（汇总）

