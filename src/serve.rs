//! phonefarm serve — MCP stdio 工具服务 (SPEC_MCP_SERVE v1)
//! 换行分隔 JSON-RPC 2.0 over stdin/stdout, 自举实现零新依赖。
//! 所有工具 = 对既有 CLI 契约的自调用(current_exe + 子命令), stdout 保持协议纯道。
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 子调用墙钟上限(查看类正常 <1s; run --detach 只起进程不等局)
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// 单次工具返回文本上限(防 cat 大文件爆客户端上下文)
const MAX_OUT: usize = 96 * 1024;

// ══════════════ JSON-RPC 行循环 ══════════════

pub fn run_serve(args: &[String]) -> i32 {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--root" {
            if let Some(d) = it.next() {
                if let Err(e) = std::env::set_current_dir(d) {
                    eprintln!("serve: chdir {d} 失败: {e}");
                    return 2;
                }
            }
        }
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // stdin 关闭/损坏: 安静退场
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle_line(&line) {
            // 响应单行无内嵌换行(serde_json 序列化保证), 写坏即客户端已走, 退场
            if writeln!(out, "{resp}").and_then(|_| out.flush()).is_err() {
                break;
            }
        }
    }
    0
}

/// 处理一行请求; 返回 None = 通知(不应答)
fn handle_line(line: &str) -> Option<String> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(rpc_error(Value::Null, -32700, &format!("Parse error: {e}"))),
    };
    if req.is_array() {
        return Some(rpc_error(Value::Null, -32600, "Invalid Request: batch 不支持"));
    }
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    match id {
        None => None, // notification: initialized/cancelled 等一律静默
        Some(id) => Some(match method {
            "initialize" => rpc_result(id, initialize_result(&req)),
            "ping" => rpc_result(id, json!({})),
            "tools/list" => rpc_result(id, json!({"tools": tools_list()})),
            "tools/call" => call_tool(id, &req),
            _ => rpc_error(id, -32601, &format!("Method not found: {method}")),
        }),
    }
}

fn rpc_result(id: Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn rpc_error(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// 协议版本原样回显客户端所提(rmcp 只接受它认识的版本, 回显即必然可接受);
/// 客户端没提时回一个广泛支持的基线
fn initialize_result(req: &Value) -> Value {
    let v = req.pointer("/params/protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("2025-06-18");
    json!({
        "protocolVersion": v,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "phonefarm", "version": env!("CARGO_PKG_VERSION")}
    })
}

// ══════════════ 工具清单 ══════════════

fn tool(name: &str, desc: &str, schema: Value) -> Value {
    json!({"name": name, "description": desc, "inputSchema": schema})
}

fn tools_list() -> Vec<Value> {
    let obj = |props: Value, req: &[&str]| json!({
        "type": "object", "properties": props, "required": req,
        "additionalProperties": false
    });
    let s = |d: &str| json!({"type": "string", "description": d});
    let n = |d: &str| json!({"type": "integer", "description": d});
    vec![
        tool("phonefarm_devices",
            "List connected devices: Android (adb) and OpenHarmony (hdc:<key>) targets. Read-only, free.",
            obj(json!({}), &[])),
        tool("phonefarm_tasks",
            "List traversal tasks with run counts, achieved counts and token totals. Read-only, free.",
            obj(json!({}), &[])),
        tool("phonefarm_runs",
            "List run IDs of a task, newest first. Read-only, free.",
            obj(json!({"task": s("task name; default = task with the most recent run"),
                       "limit": n("max runs, default 20")}), &[])),
        tool("phonefarm_last",
            "Verdict of the most recent run (stop reason, achieved, steps, calls, tokens). Read-only, free.",
            obj(json!({"task": s("task name; default = latest")}), &[])),
        tool("phonefarm_status",
            "Liveness of a run: running / died / finished, with summary when finished. Read-only, free.",
            obj(json!({"run": s("run ID prefix; default = latest run"),
                       "task": s("task name (narrows prefix match)")}), &[])),
        tool("phonefarm_show",
            "Run overview, or drill into one step / one record section. Read-only, free.",
            obj(json!({"run": s("run ID prefix (ambiguous prefixes list candidates)"),
                       "task": s("task name (narrows prefix match)"),
                       "step": n("step number → screen/act/diff/telemetry + artifact paths"),
                       "section": {"type": "string", "enum": ["raw", "hooks", "events", "crashes", "anr", "trace"],
                                   "description": "record section instead of overview"}}),
                &["run"])),
        tool("phonefarm_stats",
            "Telemetry summary of a run: fps/cpu/step_ms/api_ms percentiles, mem/battery ranges. Read-only, free.",
            obj(json!({"run": s("run ID prefix"), "task": s("task name")}), &["run"])),
        tool("phonefarm_lessons",
            "Experience library accumulated by past runs (win/lose counters). Read-only, free.",
            obj(json!({"task": s("task name; default = latest")}), &[])),
        tool("phonefarm_tree",
            "Interaction graph of a task (pages/edges explored so far). Read-only, free.",
            obj(json!({"task": s("task name; default = latest")}), &[])),
        tool("phonefarm_campaign",
            "Benchmark ledger (campaign.tsv rows) of a task. Read-only, free.",
            obj(json!({"task": s("task name; default = latest")}), &[])),
        tool("phonefarm_schema",
            "The log.jsonl record contract (all record types and fields). Read-only, free.",
            obj(json!({"record_type": s("single record type; default = all")}), &[])),
        tool("phonefarm_config",
            "Effective phonefarm.toml config (read-only; prompts listed by name, providers by model).",
            obj(json!({"key": s("dotted key, e.g. prompts.step; default = summary")}), &[])),
        tool("phonefarm_cat",
            "Print a run artifact (.gz auto-decompressed, .jsonl prettified, image dimensions). \
             Path MUST be inside the tasks data root — escapes are refused. Read-only, free.",
            obj(json!({"path": s("file path under the tasks root"),
                       "head": n("first N lines"), "tail": n("last N lines"),
                       "grep": s("keep lines containing this word")}), &["path"])),
        tool("phonefarm_run",
            "Start ONE traversal session in the background and return its run ID immediately \
             (poll with phonefarm_status / phonefarm_show). BURNS MODEL TOKENS (real money) — \
             ask the user before calling. budget_calls caps model spend.",
            obj(json!({"task": s("task name (isolates lessons/tree data)"),
                       "goal": s("goal text for the session"),
                       "serial": s("device: adb serial or hdc:<key>; default = auto"),
                       "app": s("target app package name"),
                       "budget_calls": n("model-call budget, default 40"),
                       "assert": s("comma-separated acceptance words that must appear")}),
                &["task", "goal"])),
        tool("phonefarm_benchmark",
            "Start a multi-round benchmark in the background (round-robin episodes + campaign.tsv). \
             BURNS MODEL TOKENS (real money) — ask the user before calling.",
            obj(json!({"task": s("task name"), "goal": s("goal text"),
                       "rounds": n("rounds, default 1"),
                       "serial": s("device serial or hdc:<key>"),
                       "app": s("target app package name"),
                       "budget_calls": n("per-round model-call budget, default 40"),
                       "assert": s("comma-separated acceptance words")}),
                &["task", "goal"])),
        tool("phonefarm_script",
            "Deterministic script/macro execution or historical run replay without LLM tokens. \
             Collects full 68-metric telemetry (FPS, CPU, PSS, Battery, Temp). Runs detached.",
            obj(json!({"task": s("task name for storing data"),
                       "script": s("path to script file (.json/.jsonl/.toml) or past run ID to replay"),
                       "serial": s("device: adb serial or hdc:<key>"),
                       "app": s("target app package for telemetry"),
                       "repeat": n("repeat count (default 1)"),
                       "settle_ms": n("settle delay in ms after actions (default 500)"),
                       "no_screen": json!({"type": "boolean", "description": "skip screenshot capture for speed"})}),
                &["task", "script"])),
    ]
}

// ══════════════ tools/call 分派 ══════════════

fn call_tool(id: Value, req: &Value) -> String {
    let name = req.pointer("/params/name").and_then(|v| v.as_str()).unwrap_or("");
    let args = req.pointer("/params/arguments").cloned().unwrap_or_else(|| json!({}));
    match build_argv(name, &args) {
        Ok(argv) => {
            let (ok, text) = self_call(&argv);
            rpc_result(id, json!({
                "content": [{"type": "text", "text": text}],
                "isError": !ok
            }))
        }
        Err(e) => rpc_result(id, json!({
            "content": [{"type": "text", "text": e}],
            "isError": true
        })),
    }
}

fn arg_str<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn arg_bool(args: &Value, k: &str) -> Option<bool> {
    args.get(k).and_then(|v| v.as_bool())
}

fn arg_u64(args: &Value, k: &str) -> Option<u64> {
    args.get(k).and_then(|v| v.as_u64())
}

fn push_opt(v: &mut Vec<String>, flag: &str, val: Option<&str>) {
    if let Some(x) = val { v.push(flag.to_string()); v.push(x.to_string()); }
}

/// 工具名+参数 → CLI argv(纯函数供单测)。Err = 参数/权限拒绝(不回设备不烧 token)
fn build_argv(name: &str, args: &Value) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    match name {
        "phonefarm_devices" => v.push("devices".into()),
        "phonefarm_tasks" => v.extend(["tasks".into(), "--json".into()]),
        "phonefarm_runs" => {
            v.push("runs".into());
            push_opt(&mut v, "--task", arg_str(args, "task"));
            push_opt(&mut v, "--limit", arg_u64(args, "limit").map(|n| n.to_string()).as_deref());
            v.push("--json".into());
        }
        "phonefarm_last" => {
            v.push("last".into());
            push_opt(&mut v, "--task", arg_str(args, "task"));
            v.push("--json".into());
        }
        "phonefarm_status" => {
            v.push("status".into());
            if let Some(r) = arg_str(args, "run") { v.push(r.to_string()); } // 位置参数
            push_opt(&mut v, "--task", arg_str(args, "task"));
            v.push("--json".into());
        }
        "phonefarm_show" => {
            let run = arg_str(args, "run").ok_or("缺参数 run(局ID前缀)")?;
            v.push("show".into());
            v.push(run.to_string());
            push_opt(&mut v, "--task", arg_str(args, "task"));
            if let Some(n) = arg_u64(args, "step") {
                v.push("--step".into());
                v.push(n.to_string());
            } else if let Some(sec) = arg_str(args, "section") {
                let flag = match sec {
                    "raw" => "--raw", "hooks" => "--hooks", "events" => "--events",
                    "crashes" => "--crashes", "anr" => "--anr", "trace" => "--trace",
                    other => return Err(format!("未知 section '{other}'(可选 raw/hooks/events/crashes/anr/trace)")),
                };
                v.push(flag.into());
            }
            v.push("--json".into());
        }
        "phonefarm_stats" => {
            let run = arg_str(args, "run").ok_or("缺参数 run(局ID前缀)")?;
            v.push("stats".into());
            v.push(run.to_string());
            push_opt(&mut v, "--task", arg_str(args, "task"));
            v.push("--json".into());
        }
        "phonefarm_lessons" => {
            v.push("lessons".into());
            push_opt(&mut v, "--task", arg_str(args, "task"));
            v.push("--json".into());
        }
        "phonefarm_tree" => {
            v.push("tree".into());
            push_opt(&mut v, "--task", arg_str(args, "task"));
            v.push("--json".into());
        }
        "phonefarm_campaign" => {
            v.push("campaign".into());
            push_opt(&mut v, "--task", arg_str(args, "task"));
            v.push("--json".into());
        }
        "phonefarm_schema" => {
            v.push("schema".into());
            push_opt(&mut v, "--type", arg_str(args, "record_type"));
        }
        "phonefarm_config" => {
            v.push("config".into());
            push_opt(&mut v, "--key", arg_str(args, "key"));
            v.push("--json".into());
        }
        "phonefarm_cat" => {
            let p = arg_str(args, "path").ok_or("缺参数 path")?;
            let jailed = cat_jail(p)?; // 路径监狱: 必须在 tasks 根之下
            v.push("cat".into());
            v.push(jailed.display().to_string());
            push_opt(&mut v, "--head", arg_u64(args, "head").map(|n| n.to_string()).as_deref());
            push_opt(&mut v, "--tail", arg_u64(args, "tail").map(|n| n.to_string()).as_deref());
            push_opt(&mut v, "--grep", arg_str(args, "grep"));
            v.push("--raw".into()); // serve 侧统一要原文, 不做二次美化(截断护栏在 execute)
        }
        "phonefarm_run" => {
            let task = arg_str(args, "task").ok_or("缺参数 task")?;
            let goal = arg_str(args, "goal").ok_or("缺参数 goal")?;
            v.push("run".into());
            v.push("--task".into()); v.push(task.to_string());
            push_opt(&mut v, "--serial", arg_str(args, "serial"));
            push_opt(&mut v, "--app", arg_str(args, "app"));
            push_opt(&mut v, "--budget-calls",
                arg_u64(args, "budget_calls").map(|n| n.to_string()).as_deref());
            push_opt(&mut v, "--assert", arg_str(args, "assert"));
            v.push("--detach".into()); // 强制后台: 立即回报局ID, 轮询走 status/show
            v.push(goal.to_string());
        }
        "phonefarm_benchmark" => {
            let task = arg_str(args, "task").ok_or("缺参数 task")?;
            let goal = arg_str(args, "goal").ok_or("缺参数 goal")?;
            v.push("benchmark".into());
            v.push("--task".into()); v.push(task.to_string());
            push_opt(&mut v, "--serial", arg_str(args, "serial"));
            push_opt(&mut v, "--app", arg_str(args, "app"));
            push_opt(&mut v, "--budget-calls",
                arg_u64(args, "budget_calls").map(|n| n.to_string()).as_deref());
            push_opt(&mut v, "--rounds", arg_u64(args, "rounds").map(|n| n.to_string()).as_deref());
            push_opt(&mut v, "--assert", arg_str(args, "assert"));
            v.push("--detach".into());
            v.push(goal.to_string());
        }
        "phonefarm_script" => {
            let task = arg_str(args, "task").ok_or("缺参数 task")?;
            let script = arg_str(args, "script").ok_or("缺参数 script")?;
            v.push("script".into());
            v.push("--task".into()); v.push(task.to_string());
            push_opt(&mut v, "--serial", arg_str(args, "serial"));
            push_opt(&mut v, "--app", arg_str(args, "app"));
            push_opt(&mut v, "--repeat", arg_u64(args, "repeat").map(|n| n.to_string()).as_deref());
            push_opt(&mut v, "--settle-ms", arg_u64(args, "settle_ms").map(|n| n.to_string()).as_deref());
            if arg_bool(args, "no_screen").unwrap_or(false) {
                v.push("--no-screen".into());
            }
            v.push("--detach".into());
            v.push(script.to_string());
        }
        other => return Err(format!("未知工具 '{other}'(tools/list 看全量; probe/exec/parallel 不在 MCP 面内)")),
    }
    Ok(v)
}

/// cat 路径监狱: canonicalize 后必须在 tasks 根之下; 根或目标解析失败一律拒绝
fn cat_jail(p: &str) -> Result<PathBuf, String> {
    let root = crate::cli::data_root()
        .canonicalize()
        .map_err(|e| format!("tasks 根不可解析({e}), 拒绝 cat"))?;
    let target = Path::new(p)
        .canonicalize()
        .map_err(|e| format!("路径不可解析 {p}: {e}"))?;
    if target.starts_with(&root) {
        Ok(target)
    } else {
        Err(format!("路径越狱被拒: {p} 不在 tasks 根 {} 之下", root.display()))
    }
}

// ══════════════ 自调用执行(带墙钟与截断) ══════════════

/// 自调用 current_exe 跑 CLI 子命令; 返回 (是否成功, 文本)。
/// 管道双线程读取防 >64KB 输出卡死; 30s 墙钟超时即 kill。
fn self_call(argv: &[String]) -> (bool, String) {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return (false, format!("current_exe 失败: {e}")),
    };
    let mut child = match Command::new(exe)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("spawn 失败: {e}")),
    };
    let mut out_pipe = child.stdout.take().expect("stdout 已 pipe");
    let mut err_pipe = child.stderr.take().expect("stderr 已 pipe");
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out_pipe.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err_pipe.read_to_end(&mut b);
        b
    });
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if t0.elapsed() < CALL_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                break None;
            }
            Err(e) => {
                let _ = child.kill();
                return (false, format!("wait 失败: {e}"));
            }
        }
    };
    let out = t_out.join().unwrap_or_default();
    let err = t_err.join().unwrap_or_default();
    let mut text = String::from_utf8_lossy(&out).to_string();
    let err_text = String::from_utf8_lossy(&err).trim().to_string();
    if !err_text.is_empty() {
        if !text.is_empty() { text.push('\n'); }
        text.push_str(&err_text);
    }
    let mut timed_out = false;
    if status.is_none() {
        timed_out = true;
        text.push_str(&format!("\n[serve: 子调用超过 {}s 被强杀]", CALL_TIMEOUT.as_secs()));
    }
    let (text, truncated) = truncate(text);
    let ok = !timed_out && status.map(|s| s.success()).unwrap_or(false) && !truncated;
    (ok, text)
}

/// 截断到 MAX_OUT(按字符边界), 附原文体积标记; 返回 (文本, 是否截断)
fn truncate(mut s: String) -> (String, bool) {
    if s.len() <= MAX_OUT {
        return (s, false);
    }
    let total = s.len();
    let mut end = MAX_OUT;
    while !s.is_char_boundary(end) { end -= 1; }
    s.truncate(end);
    s.push_str(&format!("\n[serve: 输出截断, 原文 {total} 字节; 用 head/tail/grep 收窄]"));
    (s, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: Value) -> Value {
        let line = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}}).to_string();
        let resp = handle_line(&line).expect("tools/call 必须有响应");
        serde_json::from_str(&resp).unwrap()
    }

    #[test]
    fn initialize_echoes_client_version() {
        let line = json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"x","version":"1"}}}).to_string();
        let resp: Value = serde_json::from_str(&handle_line(&line).unwrap()).unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(resp["result"]["serverInfo"]["name"], "phonefarm");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        // 客户端没提版本 → 基线
        let line2 = json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}).to_string();
        let resp2: Value = serde_json::from_str(&handle_line(&line2).unwrap()).unwrap();
        assert_eq!(resp2["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn notifications_get_no_response() {
        let n = json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string();
        assert!(handle_line(&n).is_none(), "通知不应答");
    }

    #[test]
    fn unknown_method_and_parse_error() {
        let line = json!({"jsonrpc":"2.0","id":9,"method":"resources/list"}).to_string();
        let resp: Value = serde_json::from_str(&handle_line(&line).unwrap()).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["id"], 9);
        let resp2: Value = serde_json::from_str(&handle_line("{oops").unwrap()).unwrap();
        assert_eq!(resp2["error"]["code"], -32700);
        assert_eq!(resp2["id"], Value::Null);
    }

    #[test]
    fn tools_list_shape_and_surface() {
        let line = json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}).to_string();
        let resp: Value = serde_json::from_str(&handle_line(&line).unwrap()).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 16, "工具面数量变了要有意为之");
        for t in tools {
            assert!(t["name"].as_str().unwrap().starts_with("phonefarm_"));
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert_eq!(t["inputSchema"]["type"], "object");
        }
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        // 高危面不出现在 MCP: 裸 shell 与并行 fan-out
        for banned in ["probe", "exec", "parallel"] {
            assert!(!names.iter().any(|n| n.contains(banned)), "{banned} 不得暴露");
        }
    }

    #[test]
    fn build_argv_run_forces_detach_and_no_endless() {
        let a = build_argv("phonefarm_run", &json!({"task":"T","goal":"G"})).unwrap();
        assert!(a.contains(&"--detach".to_string()), "run 必须强制 detach");
        assert!(!a.contains(&"--endless".to_string()), "run 不得有 endless");
        assert_eq!(a.last().unwrap(), "G", "goal 是位置参数收尾");
        // schema 里根本没有 endless 字段, 传了也被 additionalProperties 语义忽略:
        let a2 = build_argv("phonefarm_run", &json!({"task":"T","goal":"G","endless":true})).unwrap();
        assert!(!a2.contains(&"--endless".to_string()));
        // 缺必填
        assert!(build_argv("phonefarm_run", &json!({"goal":"G"})).is_err());
        assert!(build_argv("phonefarm_benchmark", &json!({"task":"T"})).is_err());
    }

    #[test]
    fn build_argv_script_forces_detach() {
        let a = build_argv("phonefarm_script", &json!({"task":"T","script":"test.json"})).unwrap();
        assert!(a.contains(&"--detach".to_string()), "script 必须强制 detach");
        assert_eq!(a.last().unwrap(), "test.json", "script 路径是位置参数收尾");
        // 缺必填
        assert!(build_argv("phonefarm_script", &json!({"task":"T"})).is_err());
        assert!(build_argv("phonefarm_script", &json!({"script":"s.json"})).is_err());
    }

    #[test]
    fn cat_jail_refuses_escape() {
        // 监狱根: 临时 tasks 目录
        let d = std::env::temp_dir().join(format!("pf_serve_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let inside = d.join("任务A/runs/局1");
        std::fs::create_dir_all(&inside).unwrap();
        let f = inside.join("log.jsonl");
        std::fs::write(&f, "{}").unwrap();
        std::env::set_var("PF_TASKS_ROOT", &d);
        assert!(cat_jail(f.to_str().unwrap()).is_ok(), "监狱内放行");
        assert!(cat_jail("/etc/passwd").is_err(), "/etc/passwd 必须拒");
        let escape = d.join("..").join("pf_serve_escape.txt");
        std::fs::write(&escape, "x").unwrap();
        assert!(cat_jail(escape.to_str().unwrap()).is_err(), ".. 逃逸必须拒");
        assert!(cat_jail("/nonexistent/x").is_err(), "不存在路径拒(宁可误杀)");
        std::env::remove_var("PF_TASKS_ROOT");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_file(&escape);
        // 拒绝发生在自调用之前, 不耗设备不耗 token
        let resp = call("phonefarm_cat", json!({"path": "/etc/passwd"}));
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn truncate_marks_original_size() {
        let big = "x".repeat(MAX_OUT + 1000);
        let (t, hit) = truncate(big);
        assert!(hit && t.len() < MAX_OUT + 200 && t.contains("输出截断"));
        let (t2, hit2) = truncate("小".into());
        assert!(!hit2 && t2 == "小");
    }

    #[test]
    fn unknown_tool_is_error_result() {
        let resp = call("phonefarm_probe", json!({"serial":"x","cmd":"y"}));
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("未知工具"));
    }
}
