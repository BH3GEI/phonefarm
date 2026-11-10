# phonefarm

移动端 / 鸿蒙设备的并行自动化、测试与采集工具（同事口中的「鸿蒙 use」）。

一个 Rust 二进制，三种设备后端：Android `adb`、OpenHarmony `hdc`、iOS（WebDriverAgent）。
在同一套「设备抽象 + 设备锁 + 记录契约 + 遥测」之上，并列提供互不依赖的上层能力：

| 能力 | 命令 | 烧 Token |
| :--- | :--- | :--- |
| 设备发现、保活、只读快照 | `devices` `keepalive` `fleet` `app-snapshot` `probe` `exec` | 否 |
| 设备锁与设备登记 | `lease` `tools/devlock` | 否 |
| 确定性脚本与历史回放 | `script` | 否 |
| 一致性测试（CTS / XTS，Android 与 OH 双协议） | `test-batch` `cts-fetch` | 否 |
| 采集：截图、UI 树、遥测、日志、原始帧、有界滚动 | `run`/`script` 的局目录、`capture` `collect` `remote-control` `perf` | 否 |
| 屏幕观战窗口 | `screen` | 否 |
| 多设备并行 | 每台设备一个 phonefarm 进程（设备锁保证同板互斥、异板并行）；`parallel` 是 VLM 遍历的现成 fan-out | 视通路 |
| 自主 UI 遍历（视觉模型驱动） | `run` `benchmark` `quest` `plugins` | **是** |
| 给 Agent 的工具服务 | `serve`（MCP stdio，JSON-RPC 2.0） | 否 |
| 只读下钻 | `last` `runs` `show` `status` `stats` `cat` `schema` … | 否 |
| 可选：图形性能研究通路 | `perf` `eval` `gpu-op` `gray-*` `autoloop` `loop_v1/` `knobs/` | 部分 |

视觉模型（VLM）只是其中一条通路：`script` / `test-batch` / `cts-fetch` / `capture` / `collect` /
`keepalive` / `fleet` 全程零 Token，不碰模型也各自成立。

## 构建

```bash
cp secrets.env.example secrets.env     # 只有 VLM 通路需要密钥；secrets.env 已在 .gitignore 里
cd src && cargo build --release && cp target/release/phonefarm .. && cd ..
codesign --force --sign - ./phonefarm  # Apple Silicon 必须重新签名，否则会静默卡在 _dyld_start
./phonefarm devices                    # 列出 adb / hdc / iOS 三族设备（只代表传输可见）
```

- `adb` 查找顺序：`ADB_BIN` → 仓库根 `platform-tools/` → `PATH` → 常见 SDK 目录。`hdc` 从 `PATH` 找。
- 游戏资源部署（`game-deploy`）是可选特性，默认不编译：`cargo build --release --features game`。

## 快速上手

```bash
./phonefarm devices                                      # 第一列就是可用的 serial；鸿蒙是 hdc:<connect key>
./phonefarm fleet --json                                 # 每台设备的在线/型号/电量/温度/亮屏/前台应用/锁状态
./phonefarm script --serial hdc:<key> --task 冒烟 examples/<脚本>.json   # 确定性脚本，每步截图 + UI 树 + 遥测
./phonefarm test-batch --module oh:<bundle>/<module>/OpenHarmonyTestRunner --serial hdc:<key> --out out/xts
./phonefarm parallel --job "任务A|目标A|hdc:<key1>" --job "任务B|目标B|hdc:<key2>"   # VLM 遍历，烧 Token
./phonefarm last                                         # 最近一局的结论与性能概要
```

每一局落在 `tasks/<任务>/runs/<局ID>/`：`log.jsonl`（记录契约，`phonefarm schema` 可查）、
`stepN.jpg` 截图、`stepN.xml.gz`（Android）/ `stepN.json`（OH）原始 UI 树。

## 作为 D600 harness 设备层组件

D600 集群：100 块 OpenHarmony 7.0.0.39 开发板，10 台 Mac mini 各挂 10 块，走 `hdc`；
另有 Android 对照机走 `adb`。phonefarm 在其中只做**单台宿主机上的设备层**：

- **设备池**：每台 Mac mini 上跑一份 phonefarm，`devices` / `fleet --json` 枚举本机 `hdc` 目标，
  serial 形如 `hdc:<connect key>`。
- **设备号 m07-d03**：上层用 `lease devices-set` 把设备号登记成本机的设备身份（`device_id` 如 `m07-d03`，
  `aliases` 里放 `hdc:<connect key>`）。登记只用于把别名归到同一把锁、停用坏板（`enabled=false`）
  和在产物里记下设备号；命令里仍然写真实 serial `hdc:<key>`，设备号 → serial 的映射由上层维护
  （可从 `lease devices --json` 读）。
- **板锁由上层负责**：跨宿主的板锁、排队、任务派发归 harness。phonefarm 自带的设备锁是本机
  fd 锁（同一块板互斥、不同板并行），只防同一台 Mac mini 上的进程互踩，不替代上层的板锁。
- **调用方式**：上层拿到板锁后，以子进程调用 `phonefarm <子命令> --serial hdc:<key> … --json`，
  读退出码与 stdout 的 JSON，产物从 `--out` 目录或 `tasks/` 里取。

完整说明、调用示例与已知缺口见 [docs/D600_HARNESS.md](docs/D600_HARNESS.md)。

## 设备锁与并发

- 碰设备的子命令自己拿锁：同一台设备互斥，不同设备并行；被占时排队等待（`--lock-wait` 设上限）。
- 锁是内核 fd 锁，进程退出、最后一个 fd 关闭即释放，不留需要手工清理的锁文件。
- `phonefarm lease --serial S -- <命令…>` 可以把一段多步流程包在同一把锁里，子进程里再调 phonefarm 会继承这把锁。
- 锁和登记的存放位置：`PHONEFARM_STATE_DIR`（缺省 `~/.local/state/phonefarm`），`DEVLOCK_DIR`、`PHONEFARM_LEASE_DIR` 可分别覆盖。

## 配置

| 文件 / 变量 | 作用 |
| :--- | :--- |
| `phonefarm.toml` | 运行参数：数据目录 `data_dir`、步数与等待、规则阈值、遥测间隔、模型 Provider 回退链 |
| `secrets.env`（从 `secrets.env.example` 复制） | 模型 API 密钥，只有 VLM 通路需要；不入库 |
| `ADB_BIN` / `HDC_BIN` / `PATH` | 设备工具位置 |
| `PHONEFARM_STATE_DIR` / `DEVLOCK_DIR` / `PHONEFARM_LEASE_DIR` | 设备锁、设备登记、恢复记录的存放位置 |
| `PF_TASKS_ROOT` | 覆盖局数据根目录（缺省 `<data_dir>/tasks`） |

## 可选：图形性能研究通路

`knobs/`（系统参数与 Vulkan layer 注入）、`loop_v1/`（帧时序 / 功耗采集与统计判定闭环）、
`eval` / `gpu-op` / `gray-*` / `autoloop` / `onboard` 等子命令，是为移动 GPU 能效研究做的一组工具，
上游客户端之一是 [HGamey/game_opt_loop](https://github.com/HGamey/game_opt_loop)。
它们不是设备层的必需部分，不用就不会碰到；相关的实验记录、运行数据与设计文档在
[HGamey/docs · repos/phonefarm](https://github.com/HGamey/docs/tree/main/repos/phonefarm)。

## 文档

- 给 AI 编程助手：[AGENTS.md](AGENTS.md)、[skills/phonefarm/SKILL.md](skills/phonefarm/SKILL.md)
  （另有 `skills/phonefarm-cts`、`skills/a2oh-diagnose`）
- 设备层：[D600 harness 接入](docs/D600_HARNESS.md) · [设备后端与平台差异](docs/DEVICES.md) ·
  [保活](docs/SPEC_KEEPALIVE.md) · [确定性脚本](docs/SPEC_SCRIPT_MODE.md) · [有界采集](docs/SPEC_OBSERVATION_BUDGET.md) ·
  [屏幕窗口](docs/SPEC_SCREEN_WIDGET.md) · [决策原语](docs/SPEC_DECISION_PATH.md)
- 测试：[CTS / XTS harness](docs/SPEC_CTS_HARNESS.md) · Agent 接口：[MCP 工具服务](docs/SPEC_MCP_SERVE.md)
- 架构：[DESIGN](docs/DESIGN.md) · [GOLDEN_RULES](docs/GOLDEN_RULES.md) · [假设—实验—证据](docs/SPEC_EVOLUTION.md)
- 性能研究：[性能数据来源](docs/SPEC_PERF_SOURCE.md) · [GPU 算子](docs/SPEC_GPU_OP.md) · [loop_v1](loop_v1/README.md) ·
  [knobs](knobs/README.md) · [移动 GPU 优化路线](docs/MOBILE_GPU_OPT_ROUTES.md)

第三方来源登记见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
