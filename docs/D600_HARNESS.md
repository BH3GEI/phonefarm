# 作为 D600 harness 的设备层组件

> 状态：接入说明（2026-11-10）。下面写的命令与行为都对应本仓库现有代码；
> 「缺口」一节列的是还没有的东西，不要当成现成能力。

## 1. 分工

D600 集群：100 块 OpenHarmony 7.0.0.39 开发板，10 台 Mac mini，每台经 USB 挂 10 块，走 `hdc`；
另有若干 Android 对照机，走 `adb`。上层是 Westlake / AOSP→OH harness。

| 层 | 负责什么 | 谁来做 |
| :--- | :--- | :--- |
| 调度 | 设备号（m01-d01 … m10-d10）的分配、跨宿主板锁、排队、重试、任务派发、结果汇总 | 上层 harness |
| 宿主代理 | 在每台 Mac mini 上起 phonefarm 子进程、收集产物回传 | 上层 harness（或它的 per-host agent） |
| 设备层 | 本机设备发现、设备操作、截图 / UI 树 / 日志 / 遥测采集、XTS 执行、本机设备锁 | phonefarm |

phonefarm 不跨宿主：一份 phonefarm 只看见、只管理它所在 Mac mini 上 `hdc list targets` 能看到的板子。

## 2. 设备发现

```bash
./phonefarm devices                 # 第一列是可直接用的 serial；鸿蒙为 hdc:<connect key>，不可用的在后面标状态
./phonefarm fleet --json            # 每块板一张卡：在线、型号、电量、温度、亮屏、前台应用、锁状态；采不到的字段为 null 并写原因
./phonefarm fleet --json --serial hdc:<key> --screenshot-dir shots/   # 只看一块板，顺带截图（板子被占用时不截）
./phonefarm app-snapshot --serial hdc:<key>   # 只读：前台应用与正在运行的应用
```

- `hdc` 从 `PATH` 找；hdc server 不会被 phonefarm 的任务进程组带走（任务被取消时 server 继续跑）。
- serial 里的状态不是 serial 的一部分：`hdc list targets` 里带 `Offline` / `Unauthorized` 的目标会被单独标出，不会被当成可用设备。

## 3. 设备号登记（可选，推荐）

设备号按 octoloop 会后版的写法：`m07-d03` 表示第 7 单元（第 7 台 Mac mini）的第 3 块 D600，与 hub 的 USB 口固定对应；Android 对照机写作 `m10-a01`。

每台 Mac mini 的设备登记表最多 128 台，足够放 10 块板。登记做三件事：同一块板的多个别名
（USB connect key、网络地址）归到同一把锁；坏板可以停用；产物里记录 `device_id`。

```bash
./phonefarm lease devices --json      # 读当前登记，拿到 revision
cat > m07-d03.json <<'EOF'
{"expected_revision": 0,
 "device": {"device_id": "m07-d03", "label": "D600 m07-d03（m07 单元，hub 口 3）",
            "aliases": ["hdc:<connect key>"], "enabled": true}}
EOF
./phonefarm lease devices-set --request m07-d03.json --json   # revision 不对会返回 conflict，重读后再提交
```

登记**不改变命令的寻址方式**：命令里仍然写 `--serial hdc:<connect key>`。设备号 → serial 的映射
由上层维护（可以从 `lease devices --json` 读）。`enabled=false` 的板子会被普通加锁拒绝。

## 4. 板锁与 phonefarm 的设备锁

- **跨宿主的板锁由上层负责。** 上层先拿到板锁，再在对应的 Mac mini 上调用 phonefarm。
- phonefarm 自带的是本机 fd 锁：同一块板互斥（device EX + farm SH），不同板并行；进程退出、
  最后一个 fd 关闭即释放，不需要手工删锁文件。它只防同一台 Mac mini 上的进程互踩，
  在上层持有板锁的前提下通常不会发生排队。
- 多步流程想在本机也占住一块板：`./phonefarm lease --serial hdc:<key> -- <命令…>`。
  里面再调 phonefarm 会继承这把锁（`PF_LOCK_FDS`），Python 子进程需要 `pass_fds` 传下去。
- 等待上限：`--lock-wait 30m`（缺省会一直排到拿到锁）。
- 锁与登记的存放位置：`PHONEFARM_STATE_DIR`（缺省 `~/.local/state/phonefarm`），
  `DEVLOCK_DIR`、`PHONEFARM_LEASE_DIR` 分别覆盖。每台 Mac mini 本地一份，不要放在共享盘上。

## 5. 任务下发

phonefarm 是命令行程序，上层以子进程调用，读退出码和 stdout（大多数子命令支持 `--json`），
产物从 `--out` 目录或 `tasks/<任务>/runs/<局ID>/` 取。

| 任务 | 命令 |
| :--- | :--- |
| 确定性操作序列 | `script --serial hdc:<key> --task <T> [--repeat N] [--detach] steps.json` |
| 回放某次历史操作 | `script --serial hdc:<key> --task <T> <局ID>` |
| OH XTS / 测试套 | `test-batch --module oh:<bundle>/<module>/OpenHarmonyTestRunner --serial hdc:<key> --out <目录>` |
| 设备上已有的测试结果拉回 | `cts-fetch --remote <设备路径> --serial hdc:<key> --out <目录>` |
| 只读 / 写命令 | `probe --serial hdc:<key> "<只读命令>"`、`exec --serial hdc:<key> "<命令>" --yes` |
| 视觉模型驱动的探索（烧 Token） | `run --serial hdc:<key> --task <T> --budget-calls N "<目标>"` |
| 给 Agent 的工具服务 | `serve`（MCP stdio，`phonefarm_*` 工具） |

脚本格式见 [SPEC_SCRIPT_MODE.md](SPEC_SCRIPT_MODE.md)：`launch` / `tap` / `swipe` / `sleep` / `shell` /
`loop` 等动作；鸿蒙上点击、滑动、按键、输入走 `uitest uiInput`。

后台跑：`script` / `test-batch` 加 `--detach` 立即返回局 ID 或输出目录；
`./phonefarm status <局ID> --json` 判 running / finished / died，`test-batch` 轮询 `<out>/summary.json`。

## 6. 截图、UI 树、日志与遥测

| 产物 | 在哪 | 鸿蒙上的实现 |
| :--- | :--- | :--- |
| 每步截图 `stepN.jpg` | 局目录 | `snapshot_display` + `hdc file recv` |
| 每步 UI 树 `stepN.json` | 局目录 | `uitest dumpLayout` |
| 记录 `log.jsonl` | 局目录 | 记录契约见 `phonefarm schema` |
| 每步遥测 | `log.jsonl` 里的 `telemetry` 行 | 一次 shell 读 sysfs 等节点；含 GPU 频率与利用率（devfreq / gpu_utilisation 节点存在时），采不到的字段缺席 |
| 测试结果 | `test-batch` 的 `summary.json` + JUnit XML | 逐用例判定与断言栈；日志通道 `hilog -x` |
| 性能快照 | `perf --serial hdc:<key> [--app 包名] --json` | HiSmartPerf `SP_daemon` 落盘 CSV |
| 设备状态快照 | `fleet --json` | 一次 shell 读齐 |

查看：`last`、`show <局ID> [--step N]`、`stats <局ID>`、`cat <路径>`，全部离线读盘。

## 7. 并发模型

- 一块板一个 phonefarm 进程。不同板之间没有共享状态，靠本机 fd 锁保证同板互斥。
- 一台 Mac mini 10 块板，就是 10 个并行的 phonefarm 进程；`hdc` server 在宿主上共用一份。
- `parallel` 是 VLM 遍历（`run`）的现成 fan-out（自进程重入，每行输出带设备前缀，同任务名并发会被拒绝）；
  确定性脚本、XTS 的并行由上层自己起多个进程。
- 产物目录按任务名与局 ID 隔离；上层并发时给每块板不同的 `--task` 或 `--out`。

## 8. 配置

| 文件 / 变量 | 作用 |
| :--- | :--- |
| `phonefarm.toml`（仓库根，进程工作目录下读） | `data_dir`、步数与等待、规则阈值、遥测间隔、模型 Provider 回退链 |
| `secrets.env` | 只有 VLM 通路需要；从 `secrets.env.example` 复制，不入库 |
| `PATH` / `HDC_BIN` / `ADB_BIN` | 设备工具位置 |
| `PHONEFARM_STATE_DIR` / `DEVLOCK_DIR` / `PHONEFARM_LEASE_DIR` | 锁、登记、恢复记录 |
| `PF_TASKS_ROOT` | 局数据根目录 |

## 9. 接入前的缺口

以下是本仓库现在**没有**、D600 接入前需要上层或后续开发补上的：

1. **跨宿主调度**：phonefarm 只管一台宿主。10 台 Mac mini 的统一视图、任务路由与板锁不在本仓库
   （HGamey 的 `harness-ops/node` 里有 `pf-node` 的多节点路由，但它与游戏项目耦合，没有并入）。
2. **OpenHarmony 7.0.0.39 / D600 开发板未实测**：hdc 后端是在鸿蒙商用手机上验证的；
   开发板上 `snapshot_display`、`uitest`、`SP_daemon`、`hilog` 的可用性和输出格式要先逐项核对。
3. **独立的鸿蒙日志采集**：没有单独的 `hilog` 抓取子命令（Android 侧 `remote-control` 有 logcat，
   但它只支持 Android）。目前只能 `probe` / `exec` 或 XTS 结果里带出。
4. **HAP 安装**：`test-batch` 携带安装包的路径只支持 Android `pm install`；HAP 安装需要上层先装好，
   或用 `exec --serial hdc:<key> "bm install -p …" --yes`。
5. **板子身份核验**：登记是操作员声明（`physical_identity_verified=false`），USB 口变动后
   connect key 与设备号的对应要由上层重新核对。
6. **hdc 多实例**：一台 Mac mini 上 10 块板共用一个 hdc server；hdc server 崩溃会同时影响 10 块板，
   phonefarm 不负责重启它。
