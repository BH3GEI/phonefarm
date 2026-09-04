# SPEC_SCRIPT_MODE: 确定性脚本与历史轨迹回放模式规格 (v1.0)

> **状态**: 定稿 (v1.0) · 2026-09-04
> **适用范围**: `phonefarm script` 子命令与 `phonefarm_script` MCP 工具

---

## 1. 目标与背景

### 痛点与需求
`phonefarm` 原生回路强依赖多模态视觉语言模型 (VLM) 做出下一步动作决策。但在很多自动化测试与端侧遥测场景中，VLM 驱动模式存在明显局限：
1. **重度 3D 游戏高负载压测**：游戏画面极速变化，逐帧或逐秒调用云端模型推理带来 1~3 秒的网络与推理延迟，且产生昂贵且非必要的 Token 开销。
2. **基准测试 (Benchmark) 的严格确定性**：性能评测对比（如不同温控、不同内核驱动、不同画质下的 FPS 与功耗）要求所有运行轮次遵循**完全一致的动作路径与驻留时长**，消除模型决策发散带来的干扰。
3. **历史对局复现与回放 (Replay)**：当 Agent 在某一局中发现了罕见的崩溃、ANR 或帧率断崖时，工程师需要将该局的动作序列离线 1:1 重放，进行反复抓包或打断点调试。

### 设计原则
- **零 Token 消耗，纯离线执行**：不检查、不依赖任何云端模型 Key（无需 `GLM_KEY`）。
- **完整保留 68 项全维度遥测**：每步无差别采集 FPS、Janky 占比、各 CPU 核心频率、内存 Pss 详情、SoC/电池温度、Root 层 `/proc/$PID/io`、FD 泄露监控等。
- **账本契约 100% 兼容**：产出标准 `log.jsonl`，原生支持 `phonefarm stats`、`phonefarm show`、`phonefarm last`、`phonefarm status`。
- **代码是给人看的，只是机器恰好可以运行**：简洁、直观、容错性强的格式设计。

---

## 2. CLI 命令交互契约

```bash
phonefarm script [--task <任务名>] [--serial <设备>] [--app <包名>] [--repeat N] [--settle-ms M] [--tele-interval K] [--no-screen] [--detach] <脚本文件或局ID>
```

### 参数定义

| 参数 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `<脚本文件或局ID>` | `String` | **必填** (位置参数) | 脚本文件路径 (`.json` / `.jsonl` / `.toml`)，或直接提供历史对局 ID / 前缀 |
| `--task <T>` | `String` | 脚本文件名 | 任务隔离命名空间，数据存盘于 `tasks/<T>/runs/<局ID>/` |
| `--serial <S>` | `String` | 自动检测 | 目标设备序号（支持 adb 序列号或 `hdc:<key>`） |
| `--app <P>` | `String` | 可选 | 目标监控应用包名（配置后开局上载遥测脚本并采集 Root 层与 App 层指标） |
| `--repeat <N>` | `u32` | `1` | 将脚本序列循环执行 N 轮 |
| `--settle-ms <M>` | `u64` | `500` | 每个动作发出后的界面稳定延时 (毫秒) |
| `--tele-interval <K>` | `u32` | `1` | 遥测采集步频间隔（默认每步采集，每 5 步自动触发一次重量级明细） |
| `--no-screen` | `flag` | `false` | 跳过截图落盘，适用于高帧率游戏性能压测 |
| `--detach` | `flag` | `false` | 后台静默起跑并立即返回局 ID，适配自动化 CI 流程 |

---

## 3. 脚本格式规范

### 3.1 JSON 数组格式 (`.json`)
最直观的人类可读动作流：

```json
[
  { "action": "launch", "pkg": "com.tencent.tmgp.projectc" },
  { "action": "sleep", "ms": 5000 },
  { "action": "tap", "x": 540, "y": 1200 },
  { "action": "sleep", "ms": 2000 },
  { "action": "loop", "count": 60, "steps": [
    { "action": "swipe", "x": 300, "y": 1500, "to_x": 300, "to_y": 1000 },
    { "action": "sleep", "ms": 1000 }
  ]},
  { "action": "back" }
]
```

### 3.2 动作类型表 (Action Primitives)

| 动作名称 (`action`/`a`) | 关键参数 | 底层映射 |
|---|---|---|
| `tap` / `click` | `x`, `y` | `phone.tap(x, y)` |
| `swipe` | `x`, `y`, `to_x` (或 `x2`), `to_y` (或 `y2`) | `phone.swipe(x1, y1, x2, y2)` |
| `scroll_down` | (可选无参) | `phone.scroll_down()` (屏幕中心下滑) |
| `scroll_up` | (可选无参) | `phone.scroll_up()` (屏幕中心上滑) |
| `type` / `input` | `text` (或 `t`) | `phone.type_text(text)` |
| `clear` | (可选无参) | `phone.clear_field()` (先聚焦后清空输入框) |
| `back` | (可选无参) | `phone.back()` (系统返回键) |
| `home` | (可选无参) | `phone.home()` (系统主屏幕键) |
| `sleep` / `wait` | `ms` 或 `sec` / `s` | 线程休眠 |
| `launch` / `start` | `pkg` (或 `app`), 可选 `comp` (Activity) | `phone.launch(pkg, comp)` 并自动绑定遥测监控 |
| `stop` / `force_stop`| `pkg` | `phone.force_stop(pkg)` |
| `shell` / `exec` | `cmd` | `phone.shell(cmd, 15000)` |
| `loop` / `repeat` | `count`, `steps` | 展开循环执行子动作序列 |

### 3.3 历史对局原样回放 (`.jsonl` 或对局 ID)
系统自动解析任何历史 `log.jsonl`，抽取全部 `r="act"` 记录按原坐标与顺序重放：
```bash
# 直接重放指定历史局
phonefarm script --task 游戏压测 20260831-213215
```

---

## 4. MCP 工具挂载

在 `phonefarm serve` 中，本能力以 `phonefarm_script` 对外暴露：
- 默认强制以 `--detach` 后台起跑，避免阻塞客户端 60 秒超时窗口。
- 外部 Agent 触发脚本压测后，通过 `phonefarm_status` 轮询活性，结束后通过 `phonefarm_stats` 获取分位数报告。
