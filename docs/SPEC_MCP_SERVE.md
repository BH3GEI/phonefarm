# SPEC: phonefarm serve — MCP stdio 工具服务 (L1 契约)

> 状态: 定稿 v1 · 2026-09-02
> 背景: 让 octos(及任何 MCP 客户端)把 phonefarm 当作外部工具源接入, phonefarm 不进城、octos 不改内核。
> 对接面: octos `crates/octos-agent/src/mcp.rs` — 官方 rmcp SDK, stdio 传输 = **换行分隔 JSON-RPC 2.0**;
> 握手 30s 上限, 单次 tools/call 60s 上限, 工具 schema ≤64KB/≤10 层, 子进程环境变量按白名单过滤(密钥走 secrets.env 自举, 不依赖环境继承)。

## 1. 命令形态

```
phonefarm serve [--root <目录>]
```

- `--root`: 启动时 chdir 到该目录(默认不动)。octos spawn 子进程时 CWD 不可控, 此参数把
  `phonefarm.toml` / `secrets.env` / `tasks/` 的相对解析钉死在仓库根。
- 进入循环后: stdin 每行一个 JSON-RPC 请求, stdout 每行一个响应; stderr 随便写日志(客户端 inherited, 不进协议)。
- **stdout 纯度**: serve 自身除响应行外不得向 stdout 写任何字节。
- 顺序处理(一次一个请求)。协议允许响应乱序, 顺序应答是合法子集; octos 侧 `concurrency_class = "exclusive"` 本来也会串行化。

## 2. 协议子集(自举, 不引依赖)

| 方法 | 处理 |
|---|---|
| `initialize` | 回 `{protocolVersion: <原样回显客户端所提>, capabilities: {tools: {}}, serverInfo: {name: "phonefarm", version}}`; 客户端没提版本时回 `"2025-06-18"` |
| `notifications/initialized` 及一切 notification(无 id) | 不应答 |
| `ping` | 回 `{}` |
| `tools/list` | 回第 3 节工具清单 |
| `tools/call` | 执行第 4 节分派, 回 `{content: [{type: "text", text}], isError?}` |
| 其余带 id 方法 | `-32601 Method not found` |
| 行不是合法 JSON | `-32700 Parse error` (id: null) |

`id` 无论数字还是字符串都原样回显(以 `Value` 透传, 不重解释)。

## 3. 工具清单(15 个)

全部工具 = **对现有 CLI 契约的自调用**(`std::env::current_exe()` + 子命令), 零重构 cli.rs/runtime.rs。
自调用 stdout→text 内容; 退出码非 0 → `isError: true` 并带上 stderr。

| 工具 | 自调用 | 说明 |
|---|---|---|
| `phonefarm_devices` | `devices` | adb + hdc 两族设备列表 |
| `phonefarm_tasks` | `tasks --json` | 任务清单(局数/双过数/token 账) |
| `phonefarm_runs` | `runs [--task T] [--limit N] --json` | 某任务的局列表 |
| `phonefarm_last` | `last [--task T] --json` | 最近一局结论 |
| `phonefarm_status` | `status [局ID] [--task T] --json` | 活性三态(running/died/finished) |
| `phonefarm_show` | `show <局ID> [--task T] [--step N \| --raw/--hooks/--events/--crashes/--anr/--trace] --json` | 局概要/单步下钻/分类记录 |
| `phonefarm_stats` | `stats <局ID> [--task T] --json` | 遥测汇总 |
| `phonefarm_lessons` | `lessons [--task T] --json` | 经验库 |
| `phonefarm_tree` | `tree [--task T] --json` | 交互网(v1 不暴露 --rebuild) |
| `phonefarm_campaign` | `campaign [--task T] --json` | 评测账 |
| `phonefarm_schema` | `schema [--type T]` | log.jsonl 记录契约 |
| `phonefarm_config` | `config [--key K] --json` | 生效配置(只读) |
| `phonefarm_cat` | `cat <路径> [--head N] [--tail N] [--grep 词]` | 万能查看器, **路径监狱见 §4** |
| `phonefarm_run` | `run --task T [--serial S] [--app P] [--budget-calls N] [--assert ...] --detach "<目标>"` | 起一局, **强制 --detach** 立即回报局 ID |
| `phonefarm_benchmark` | `benchmark --task T [--rounds N] ... --detach "<目标>"` | 起评测, 同样强制 detach |

**v1 不暴露**: `probe`/`exec`(裸 shell 绕过六步循环的三道拦截, 设备写操作的唯一入口必须是受检回路)、
`parallel`(fan-out 语义留给 octos swarm 层, 不在工具内复制)、`tree --rebuild`(写操作, 留给 CLI 人工)、
`run --endless`(无界烧钱; budget-calls 已是上限)。

## 4. 安全与资源护栏

1. **cat 路径监狱**: 请求路径 canonicalize 后必须在 canonicalize 后的 tasks 根
   (`PF_TASKS_ROOT` ‖ `phonefarm.toml:data_dir` + `/tasks`)之下; 越界一律拒绝。
   监狱根解析失败(目录不存在)时一律拒绝——宁可误杀不放开。
2. **输出截断**: 单次工具返回文本 >96KB 截断并附 `[截断标记: 原文 X 字节]`(防 cat 大 xml.gz 爆客户端上下文)。
3. **子调用超时**: 30s 墙钟, 超时 kill 并报 isError(查看类命令正常 <1s; run --detach 只起进程不等局)。
4. **烧 token 的只有 run/benchmark**: 工具描述里如实标注 "burns model tokens"; 客户端 agent 是否先问人是客户端的策略(octos 侧有 tool_policy 可再拦一层)。
5. **密钥**: serve 不读不碰; run 的 detached 子进程走既有 `ensure_keys` → `./secrets.env` 自举(chdir 已钉住根)。
   octos 白名单过滤环境变量不影响此链路。

## 5. octos 侧接法(交付物的一部分, 写进 README)

```jsonc
// config.json (或 profile 的 [[mcp_servers]])
{
  "mcp_servers": [
    {
      "command": "/path/to/phonefarm",
      "args": ["serve", "--root", "/path/to/phonefarm-repo"],
      "concurrency_class": "exclusive"   // 设备是独占资源, 串行化本服务的全部工具调用
    }
  ]
}
```

工具名在 octos 内按原名注册(无前缀), 与内置保护名(shell/read_file/...)无冲突。

## 6. 验收

1. `cd src && cargo test` 全绿(新增 serve 单测: 版本回显/工具清单形状/未知方法/解析错误/cat 越狱拒绝/截断/run 强制 detach 且剥 endless)。
2. 手工 NDJSON 会话: initialize→initialized→tools/list→tools/call(devices/tasks/last/cat 越狱)逐条验证。
3. **端到端**: 用 octos 真实 `McpClient`(临时测试, 不进 octos 仓库)握手 + 发现工具 + 调通一只只读工具。
4. 文档: README 加 serve 小节 + 上面的配置片段; SKILL.md 提及 serve 存在。
