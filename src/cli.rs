//! CLI 查看层 (CLI Spec v1.0): 万事万物一条命令可取——硬盘上任何 log/产物,CLI 打印;
//! 数据全在盘上,CLI 只是"发现路径"。只读原则: 查看命令只读盘,不跑设备不烧 token 不改状态
//! (probe 跑设备但只读约定;exec 高危,必须 --yes 显式确认)。
//! 局ID 模糊匹配: 前缀即可,多命中报候选;--task 默认=最近有局的任务;全命令支持 --json。
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const SCHEMA_MD: &str = r#"# log.jsonl 记录契约 (schema)

每行一个 JSON 对象,`r` 字段标记类型。一局 = tasks/<任务>/runs/<局ID>/log.jsonl。

| r | 含义 | 主要字段 |
|---|---|---|
| goal | 局目标原文 | t=目标文本 |
| screen | 每步观测 | n=步号 els=[{t,b}] pkg activity ime_shown img xml ocr(备胎标记) |
| act | 执行的动作 | n a=动作名 x y x2 y2 text what by ms |
| diff | 动作后的画面差异 | n d="+[新增] -[消失]"或rejected(驳回理由) |
| note | 模型自写便签 | t |
| raw | 模型回包原文 | n hook=step/done/verdict by ms t=原文 |
| hook | 系统判定/事件 | kind=orient/assert/verdict/ctx_stat/oscill/webview_back2/heal/budget/tree/done_reflect/... |
| probe | 探针问答 | n a=inspect/find/get_state/history q ans |
| lesson | 复盘写入的经验 | id t born win lose |
| telemetry | 每步性能快照(平铺字段) | n seq heavy + 系统/渲染/App/Root/PSI/流量/传感器/IPC/Host 各层(采不到即缺席) |
| app_event | 遥测差值事件 | kind=crash/anr/fd_growth/net_conn n from to pkg |
| trace | 深度产物指针(按需抓取) | kind n file=trace/stepN.*.txt |
| start | 局开跑标记(status 活性判定的地基) | pid ts |
| end | 局级收官结论 | stop achieved steps exec_steps calls tokens wall_ms done_claim |

跨局产物: tree.json(交互网:pages/edges/steps_total) lessons.jsonl(经验库) campaign*.tsv(评测账)。
截图 stepN.jpg 与原始UI树 stepN.xml.gz(Android)/stepN.json(OH) 同目录,是账本的地面真值。
"#;

// ══════════════ 路径与数据发现 ══════════════

/// 仓库根的 tasks 目录(尊重 phonefarm.toml 的 data_dir,读不到用当前目录)。
/// PF_TASKS_ROOT 环境变量可整体覆盖(单测注入用,避免 chdir 的进程级竞态)。
pub(crate) fn data_root() -> PathBuf {
    if let Ok(o) = std::env::var("PF_TASKS_ROOT") {
        return PathBuf::from(o);
    }
    let dir = std::fs::read_to_string("phonefarm.toml").ok()
        .and_then(|s| toml::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("data_dir").and_then(|d| d.as_str().map(String::from)))
        .unwrap_or_else(|| ".".into());
    PathBuf::from(dir.trim_end_matches('/')).join("tasks")
}

fn list_tasks() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(data_root()).ok().into_iter().flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("runs").is_dir() || e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| !n.starts_with('.') && !n.starts_with('_'))
        .collect();
    v.sort();
    v
}

fn runs_of(task: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(data_root().join(task).join("runs"))
        .ok().into_iter().flatten().filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| !n.starts_with('.'))
        .collect();
    v.sort();
    v
}

/// 最近有局的任务(--task 缺省值): 全任务里 run 目录名(时间戳)最大的
fn latest_task() -> Option<String> {
    list_tasks().into_iter()
        .filter_map(|t| runs_of(&t).pop().map(|r| (r, t)))
        .max()
        .map(|(_, t)| t)
}

/// 局ID 模糊解析: 前缀命中;唯一命中返回 (任务, 局ID, 局目录);多命中列候选。
fn resolve_run(id: &str, task: Option<&str>) -> Result<(String, String, PathBuf), String> {
    let tasks = match task {
        Some(t) => vec![t.to_string()],
        None => list_tasks(),
    };
    let mut hits: Vec<(String, String)> = Vec::new();
    for t in &tasks {
        for r in runs_of(t) {
            if r.starts_with(id) {
                hits.push((t.clone(), r));
            }
        }
    }
    match hits.len() {
        0 => Err(format!("没有以 '{id}' 开头的局{}", task.map(|t| format!("(任务 {t})")).unwrap_or_default())),
        1 => {
            let (t, r) = hits.remove(0);
            let p = data_root().join(&t).join("runs").join(&r);
            Ok((t, r, p))
        }
        _ => {
            let list: Vec<String> = hits.iter().take(10)
                .map(|(t, r)| format!("  {r}  (任务 {t})")).collect();
            Err(format!("'{id}' 命中 {} 局,补长前缀二选一:\n{}", hits.len(), list.join("\n")))
        }
    }
}

fn read_jsonl(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap_or_default()
        .lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// 局摘要: r=end 收官记录为权威源;旧局(无 end)从记录派生兜底,派生字段如实标注
fn run_summary(recs: &[Value]) -> Value {
    if let Some(e) = recs.iter().find(|r| r["r"] == "end") {
        let mut e = e.clone();
        e.as_object_mut().map(|o| o.remove("r"));
        return e;
    }
    let steps = recs.iter().filter(|r| r["r"] == "act").count();
    let calls = recs.iter().filter(|r| r["r"] == "raw").count();
    let verdict = recs.iter().rev().find(|r| r["r"] == "hook" && r["kind"] == "verdict");
    json!({
        "achieved": verdict.map(|v| v["achieved"].clone()).unwrap_or(Value::Bool(false)),
        "stop": "?(旧局无end记录,steps/calls为派生值)",
        "steps": steps, "calls": calls,
        "reason": verdict.map(|v| v["reason"].clone()).unwrap_or(Value::Null),
        "derived": true
    })
}

/// 局状态三态判定(活性探测注入供单测): 有end→finished;有start且pid活→running;
/// 有start且pid死→died(中断);都无→finished(旧局无标记,历史局必已结束)
fn status_of(recs: &[Value], pid_alive: impl Fn(i64) -> bool) -> (&'static str, Option<i64>) {
    if recs.iter().any(|r| r["r"] == "end") {
        return ("finished", None);
    }
    if let Some(st) = recs.iter().find(|r| r["r"] == "start") {
        let pid = st["pid"].as_i64().unwrap_or(-1);
        return if pid > 0 && pid_alive(pid) { ("running", Some(pid)) } else { ("died", Some(pid)) };
    }
    ("finished", None)
}

/// pid 活性(ps -p,macOS/Linux 通用,无依赖)
fn pid_alive_ps(pid: i64) -> bool {
    std::process::Command::new("ps").args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false)
}

fn cmd_status(a: &Args) -> Result<(), String> {
    let (task, run, dir) = match a.positional() {
        Some(id) => resolve_run(&id, a.opt("--task").as_deref())?,
        None => {
            let t = task_or_latest(a)?;
            let r = runs_of(&t).pop().ok_or(format!("任务 {t} 没有局"))?;
            let d = data_root().join(&t).join("runs").join(&r);
            (t, r, d)
        }
    };
    let recs = read_jsonl(&dir.join("log.jsonl"));
    let (st, pid) = status_of(&recs, pid_alive_ps);
    let max_n = recs.iter().filter_map(|r| r["n"].as_i64()).max().unwrap_or(0);
    if a.flag("--json") {
        println!("{}", json!({"task":task,"run":run,"status":st,"pid":pid,
            "steps_so_far":max_n,"summary":(st=="finished").then(|| run_summary(&recs))}));
        return Ok(());
    }
    match st {
        "running" => println!("局 {run} (任务 {task}): 运行中 pid={} 已至第{max_n}步
目录: {}", pid.unwrap_or(-1), dir.display()),
        "died" => println!("局 {run} (任务 {task}): 中断(进程已不在,无收官记录) 停在第{max_n}步
目录: {}", dir.display()),
        _ => println!("{}", fmt_summary(&task, &run, &dir, &run_summary(&recs))),
    }
    Ok(())
}

fn goal_of(recs: &[Value]) -> String {
    recs.iter().find(|r| r["r"] == "goal")
        .and_then(|r| r["t"].as_str()).unwrap_or("").to_string()
}

fn fmt_summary(task: &str, run: &str, dir: &Path, s: &Value) -> String {
    format!("局 {run}  (任务 {task})\n路径: {}\nstop={} achieved={} steps={} calls={} tokens={} wall={}s{}",
        dir.display(),
        s["stop"].as_str().unwrap_or("?"),
        s["achieved"], s["steps"], s["calls"],
        s.get("tokens").unwrap_or(&Value::Null),
        s["wall_ms"].as_i64().map(|m| (m as f64 / 1000.0).round()).unwrap_or(-1.0),
        if s["derived"].as_bool().unwrap_or(false) { "  [派生摘要]" } else { "" })
}

// ══════════════ 参数解析小件 ══════════════

struct Args {
    vals: Vec<String>,
}
impl Args {
    fn new(a: &[String]) -> Self {
        Args { vals: a.to_vec() }
    }
    fn flag(&self, name: &str) -> bool {
        self.vals.iter().any(|v| v == name)
    }
    fn opt(&self, name: &str) -> Option<String> {
        self.vals.iter().position(|v| v == name)
            .and_then(|i| self.vals.get(i + 1).cloned())
    }
    /// 位置参数: 第一个不带 -- 前缀且不是任何 --k 的值的项
    fn positional(&self) -> Option<String> {
        let mut skip = false;
        for v in &self.vals {
            if skip { skip = false; continue; }
            if let Some(s) = v.strip_prefix("--") {
                skip = !matches!(s, "json" | "raw" | "hooks" | "events" | "crashes" | "anr" | "trace" | "markdown" | "yes" | "rebuild");
                continue;
            }
            return Some(v.clone());
        }
        None
    }
}

fn task_or_latest(a: &Args) -> Result<String, String> {
    match a.opt("--task") {
        Some(t) => Ok(t),
        None => latest_task().ok_or_else(|| "tasks/ 下没有任何局;先跑一局或用 --task 指定".into()),
    }
}

// ══════════════ 各子命令 ══════════════

fn cmd_last(a: &Args) -> Result<(), String> {
    let (task, run) = match a.opt("--task") {
        Some(t) => {
            let r = runs_of(&t).pop().ok_or(format!("任务 {t} 没有局"))?;
            (t, r)
        }
        None => list_tasks().into_iter()
            .filter_map(|t| runs_of(&t).pop().map(|r| (r, t)))
            .max().map(|(r, t)| (t, r))
            .ok_or("tasks/ 下没有任何局")?,
    };
    let dir = data_root().join(&task).join("runs").join(&run);
    let recs = read_jsonl(&dir.join("log.jsonl"));
    let s = run_summary(&recs);
    if a.flag("--json") {
        println!("{}", json!({"task":task,"run":run,"path":dir.display().to_string(),"summary":s,"goal":goal_of(&recs)}));
    } else {
        let g: String = goal_of(&recs).chars().take(120).collect();
        println!("{}\ngoal: {}", fmt_summary(&task, &run, &dir, &s), g);
        if s["derived"].as_bool().unwrap_or(false) {
            let (st, pid) = status_of(&recs, pid_alive_ps);
            if st != "finished" {
                println!("状态: {}", if st == "running" { format!("运行中 pid={}", pid.unwrap_or(-1)) } else { "中断(无收官记录且进程已不在)".into() });
            }
        }
    }
    Ok(())
}

fn cmd_runs(a: &Args) -> Result<(), String> {
    let task = task_or_latest(a)?;
    let limit: usize = a.opt("--limit").and_then(|v| v.parse().ok()).unwrap_or(20);
    let mut rows = Vec::new();
    for run in runs_of(&task).into_iter().rev().take(limit) {
        let dir = data_root().join(&task).join("runs").join(&run);
        let recs = read_jsonl(&dir.join("log.jsonl"));
        let s = run_summary(&recs);
        rows.push(json!({"run":run,"summary":s}));
    }
    if a.flag("--json") {
        println!("{}", json!({"task":task,"runs":rows}));
    } else {
        println!("任务 {task}  ({} 局,最新在前)", rows.len());
        for r in &rows {
            let s = &r["summary"];
            println!("  {}  stop={:<10} achieved={:<5} steps={} calls={} tokens={}",
                r["run"].as_str().unwrap_or("?"),
                s["stop"].as_str().unwrap_or("?"), s["achieved"], s["steps"], s["calls"],
                s.get("tokens").unwrap_or(&Value::Null));
        }
    }
    Ok(())
}

fn cmd_show(a: &Args) -> Result<(), String> {
    let id = a.positional().ok_or("用法: phonefarm show <局ID前缀> [--task T] [--step N|--raw|--hooks|--events|--crashes|--anr|--trace] [--json]")?;
    let (task, run, dir) = resolve_run(&id, a.opt("--task").as_deref())?;
    let recs = read_jsonl(&dir.join("log.jsonl"));

    if let Some(nstr) = a.opt("--step") {
        let n: i64 = nstr.parse().map_err(|_| "--step 要数字")?;
        let pick = |ty: &str| recs.iter().find(|r| r["r"] == ty && r["n"] == n).cloned();
        let files: Vec<String> = ["jpg", "xml", "xml.gz", "json"].iter()
            .map(|e| dir.join(format!("step{n}.{e}")))
            .filter(|p| p.exists())
            .map(|p| p.display().to_string()).collect();
        let out = json!({
            "task": task, "run": run, "step": n,
            "screen": pick("screen"), "act": pick("act"), "diff": pick("diff"),
            "telemetry": pick("telemetry"), "files": files
        });
        if a.flag("--json") { println!("{out}"); return Ok(()); }
        println!("局 {run} 第{n}步");
        for f in &files { println!("文件: {f}"); }
        for k in ["screen", "act", "diff", "telemetry"] {
            match &out[k] {
                Value::Null => println!("── {k}: (本步无此记录)"),
                v => println!("── {k}:\n{}", serde_json::to_string_pretty(v).unwrap_or_default()),
            }
        }
        return Ok(());
    }
    for (flag, ty) in [("--raw", "raw"), ("--hooks", "hook"), ("--events", "app_event")] {
        if a.flag(flag) {
            let hits: Vec<&Value> = recs.iter().filter(|r| r["r"] == ty).collect();
            if a.flag("--json") {
                println!("{}", json!(hits));
            } else if hits.is_empty() {
                println!("局 {run}: 无 {ty} 记录{}", if ty == "app_event" { "(整局无崩溃/ANR/fd/网络事件)" } else { "" });
            } else {
                for h in &hits { println!("{}", serde_json::to_string(h).unwrap_or_default()); }
            }
            return Ok(());
        }
    }
    for (flag, kind, human) in [("--crashes", "crash", "崩溃栈"), ("--anr", "anr", "ANR trace"), ("--trace", "trace", "系统 trace")] {
        if a.flag(flag) {
            // 深度产物: 账本指针(r=trace) + runs/<局>/trace/ 落盘文件;没有即"本局未触发"
            let ptrs: Vec<&Value> = recs.iter()
                .filter(|r| r["r"] == "trace" && (kind == "trace" || r["kind"] == kind)).collect();
            let tdir = dir.join("trace");
            let mut files: Vec<PathBuf> = std::fs::read_dir(&tdir).ok().into_iter().flatten()
                .filter_map(|e| e.ok()).map(|e| e.path())
                .filter(|p| kind == "trace" || p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.contains(kind)))
                .collect();
            files.sort();
            if ptrs.is_empty() && files.is_empty() {
                println!("局 {run}: 本局未触发{human}采集");
                return Ok(());
            }
            for p in &ptrs { println!("指针: {}", serde_json::to_string(p).unwrap_or_default()); }
            for f in &files {
                println!("═══ {} ═══", f.display());
                println!("{}", std::fs::read_to_string(f).unwrap_or_else(|_| "(读不出,可能是二进制,用路径自行处理)".into()));
            }
            return Ok(());
        }
    }
    // 默认: 局概要
    let s = run_summary(&recs);
    let mut counts: Vec<(String, usize)> = {
        let mut m = std::collections::HashMap::new();
        for r in &recs {
            *m.entry(r["r"].as_str().unwrap_or("?").to_string()).or_insert(0) += 1;
        }
        m.into_iter().collect()
    };
    counts.sort();
    let mut files: Vec<String> = std::fs::read_dir(&dir).ok().into_iter().flatten()
        .filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().map(String::from)).collect();
    files.sort();
    if a.flag("--json") {
        println!("{}", json!({"task":task,"run":run,"path":dir.display().to_string(),
            "summary":s,"goal":goal_of(&recs),"record_counts":counts,"files":files}));
    } else {
        println!("{}", fmt_summary(&task, &run, &dir, &s));
        println!("goal: {}", goal_of(&recs));
        println!("记录: {}", counts.iter().map(|(k, c)| format!("{k}×{c}")).collect::<Vec<_>>().join(" "));
        println!("文件({}个): {}", files.len(), files.join(" "));
    }
    Ok(())
}

/// cat 的行过滤(纯函数供单测): --grep 后 --head/--tail
fn filter_lines(text: &str, grep: Option<&str>, head: Option<usize>, tail: Option<usize>) -> String {
    let mut lines: Vec<&str> = text.lines()
        .filter(|l| grep.map_or(true, |g| l.contains(g))).collect();
    if let Some(h) = head { lines.truncate(h); }
    if let Some(t) = tail {
        if lines.len() > t { lines = lines.split_off(lines.len() - t); }
    }
    lines.join("\n")
}

fn cmd_cat(a: &Args) -> Result<(), String> {
    let p = a.positional().ok_or("用法: phonefarm cat <路径> [--raw] [--head N] [--tail N] [--grep 词]")?;
    let path = PathBuf::from(&p);
    if !path.exists() { return Err(format!("文件不存在: {p}")); }
    let grep = a.opt("--grep");
    let head = a.opt("--head").and_then(|v| v.parse().ok());
    let tail = a.opt("--tail").and_then(|v| v.parse().ok());
    let name = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let raw_mode = a.flag("--raw");
    // 智能识别: gz 解压;jsonl 逐行美化;json 缩进;图片报尺寸;其余原样
    let text = if name.ends_with(".gz") {
        let out = std::process::Command::new("gzip").args(["-dc", &p]).output()
            .map_err(|e| format!("gzip 解压失败: {e}"))?;
        String::from_utf8_lossy(&out.stdout).to_string()
    } else if name.ends_with(".jpg") || name.ends_with(".jpeg") || name.ends_with(".png") {
        let out = std::process::Command::new("sips")
            .args(["-g", "pixelWidth", "-g", "pixelHeight", &p]).output()
            .map_err(|e| format!("sips: {e}"))?;
        println!("{}", String::from_utf8_lossy(&out.stdout).trim());
        println!("路径: {}", path.display());
        return Ok(());
    } else {
        std::fs::read_to_string(&path).map_err(|e| format!("读取失败: {e}"))?
    };
    let text = if !raw_mode && (name.ends_with(".jsonl") || name.contains(".jsonl")) {
        text.lines().map(|l| {
            serde_json::from_str::<Value>(l)
                .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| l.into()))
                .unwrap_or_else(|_| l.to_string())
        }).collect::<Vec<_>>().join("\n")
    } else if !raw_mode && name.ends_with(".json") {
        serde_json::from_str::<Value>(&text)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| text.clone()))
            .unwrap_or(text)
    } else {
        text
    };
    println!("{}", filter_lines(&text, grep.as_deref(), head, tail));
    Ok(())
}

fn cmd_schema(a: &Args) -> Result<(), String> {
    if a.flag("--markdown") { println!("{SCHEMA_MD}"); return Ok(()); }
    match a.opt("--type") {
        Some(t) => {
            let hit: Vec<&str> = SCHEMA_MD.lines()
                .filter(|l| l.starts_with(&format!("| {t} ")))
                .collect();
            if hit.is_empty() { return Err(format!("未知 r 类型 '{t}';phonefarm schema 看全部")); }
            for l in hit { println!("{l}"); }
        }
        None => println!("{SCHEMA_MD}"),
    }
    Ok(())
}

fn cmd_tree(a: &Args) -> Result<(), String> {
    let task = task_or_latest(a)?;
    if a.flag("--rebuild") {
        // 手动重建(TREE_RUST_SPEC 任务3): 纯离线,复用局末同一 rebuild(),不烧 token
        let dir = data_root().join(&task);
        let text = crate::tree::rebuild(dir.to_str().unwrap_or("."))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        println!("已重建 {}/tree.json: {} 步材料 → {} 页 {} 边 (熟路 {} 条, 无家可归画面 {} 张)",
            dir.display(), v["steps_total"],
            v["pages"].as_array().map(|x| x.len()).unwrap_or(0),
            v["edges"].as_array().map(|x| x.len()).unwrap_or(0),
            v["edges"].as_array().map(|x| x.iter().filter(|e| e["ripe"] == true).count()).unwrap_or(0),
            v["orphan_screens"]);
    }
    let p = data_root().join(&task).join("tree.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p)
        .map_err(|_| format!("任务 {task} 没有 tree.json(还没跑出交互网)"))?)
        .map_err(|e| format!("tree.json 解析失败: {e}"))?;
    if a.flag("--json") { println!("{v}"); return Ok(()); }
    let pages = v["pages"].as_array().cloned().unwrap_or_default();
    let edges = v["edges"].as_array().cloned().unwrap_or_default();
    println!("交互网 {task}: {} 页 {} 边 (steps_total={})", pages.len(), edges.len(), v["steps_total"]);
    let mut ps: Vec<&Value> = pages.iter().collect();
    ps.sort_by_key(|p| -p["visits"].as_i64().unwrap_or(0));
    for p in ps.iter().take(15) {
        println!("  P{}[{}] visits={}", p["id"], p["name"].as_str().unwrap_or("?"), p["visits"]);
    }
    if pages.len() > 15 { println!("  …另有 {} 页(--json 看全量)", pages.len() - 15); }
    let ripe = edges.iter().filter(|e| e["ripe"].as_bool().unwrap_or(false)).count();
    println!("熟路边(ripe): {ripe}/{}", edges.len());
    Ok(())
}

fn cmd_lessons(a: &Args) -> Result<(), String> {
    let task = task_or_latest(a)?;
    let p = data_root().join(&task).join("lessons.jsonl");
    let recs: Vec<Value> = read_jsonl(&p).into_iter().filter(|r| r["r"] == "lesson").collect();
    if a.flag("--json") { println!("{}", json!(recs)); return Ok(()); }
    if recs.is_empty() { println!("任务 {task}: 无经验(lessons.jsonl 空或不存在)"); return Ok(()); }
    println!("任务 {task} 经验库({}条):", recs.len());
    for r in &recs {
        println!("  #{} [win{} lose{} born局{}] {}", r["id"], r["win"], r["lose"], r["born"],
            r["t"].as_str().unwrap_or(""));
    }
    Ok(())
}

fn cmd_campaign(a: &Args) -> Result<(), String> {
    let task = task_or_latest(a)?;
    let dir = data_root().join(&task);
    let mut found = false;
    let mut names: Vec<String> = std::fs::read_dir(&dir).ok().into_iter().flatten()
        .filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n.starts_with("campaign") && n.ends_with(".tsv")).collect();
    names.sort();
    for n in names {
        found = true;
        let text = std::fs::read_to_string(dir.join(&n)).unwrap_or_default();
        if a.flag("--json") {
            let mut lines = text.lines();
            let headers: Vec<&str> = lines.next().unwrap_or("").split('\t').collect();
            let rows: Vec<Value> = lines.map(|l| {
                let m: serde_json::Map<String, Value> = headers.iter().zip(l.split('\t'))
                    .map(|(h, v)| (h.to_string(), Value::String(v.to_string()))).collect();
                Value::Object(m)
            }).collect();
            println!("{}", json!({"file": n, "rows": rows}));
        } else {
            println!("═══ {n} ═══\n{}", text.trim_end());
        }
    }
    if !found { println!("任务 {task}: 无 campaign 账(benchmark 才写)"); }
    Ok(())
}

fn cmd_tasks(a: &Args) -> Result<(), String> {
    let mut rows = Vec::new();
    for t in list_tasks() {
        let runs = runs_of(&t);
        let mut achieved = 0u32;
        let mut tokens = 0u64;
        for r in &runs {
            let recs = read_jsonl(&data_root().join(&t).join("runs").join(r).join("log.jsonl"));
            let s = run_summary(&recs);
            if s["achieved"].as_bool().unwrap_or(false) { achieved += 1; }
            tokens += s["tokens"].as_u64().unwrap_or(0);
        }
        rows.push(json!({"task":t,"runs":runs.len(),"latest":runs.last(),
            "achieved":achieved,"tokens":tokens}));
    }
    if a.flag("--json") { println!("{}", json!(rows)); return Ok(()); }
    println!("{} 个任务:", rows.len());
    for r in &rows {
        println!("  {}  {}局 双过{} tokens{} 最近{}",
            r["task"].as_str().unwrap_or("?"), r["runs"], r["achieved"], r["tokens"],
            r["latest"].as_str().unwrap_or("-"));
    }
    Ok(())
}

fn cmd_config(a: &Args) -> Result<(), String> {
    let text = std::fs::read_to_string("phonefarm.toml").map_err(|e| format!("读不到 phonefarm.toml: {e}"))?;
    let cfg: crate::Config = toml::from_str(&text).map_err(|e| format!("解析失败: {e}"))?;
    let full = serde_json::to_value(&cfg).map_err(|e| e.to_string())?;
    if let Some(key) = a.opt("--key") {
        // 点路径取值: 如 --key prompts.step / --key telemetry_interval
        let mut cur = &full;
        for seg in key.split('.') {
            cur = cur.get(seg).ok_or(format!("没有配置项 '{key}'"))?;
        }
        match cur {
            Value::String(s) => println!("{s}"),
            v => println!("{}", serde_json::to_string_pretty(v).unwrap_or_default()),
        }
        return Ok(());
    }
    if a.flag("--json") { println!("{full}"); return Ok(()); }
    // 人读摘要: 标量全列;提示词只列名+长度;provider 列名+模型
    let o = full.as_object().unwrap();
    println!("生效配置(含默认值;--key <k> 看单项全文,--json 全量):");
    for (k, v) in o {
        match k.as_str() {
            "prompts" => {
                let names: Vec<String> = v.as_object().map(|m| m.iter()
                    .map(|(n, t)| format!("{n}({}字)", t.as_str().map(|s| s.chars().count()).unwrap_or(0)))
                    .collect()).unwrap_or_default();
                println!("  prompts: {}", names.join(" "));
            }
            "hook" | "hooks" => println!("  hooks: {} 个", v.as_array().map(|a| a.len()).unwrap_or(0)),
            "providers" => {
                let names: Vec<String> = v.as_array().map(|a| a.iter()
                    .map(|p| format!("{}[{}]", p["name"].as_str().unwrap_or("?"), p["model"].as_str().unwrap_or("?")))
                    .collect()).unwrap_or_default();
                println!("  providers: {}", names.join(" → "));
            }
            _ => println!("  {k} = {v}"),
        }
    }
    Ok(())
}

/// f64 序列的统计三件套(均值/p50/p95),空序列 None
fn stat3(mut v: Vec<f64>) -> Option<(f64, f64, f64)> {
    if v.is_empty() { return None; }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let pick = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    Some(((mean * 10.0).round() / 10.0, pick(0.5), pick(0.95)))
}

fn cmd_stats(a: &Args) -> Result<(), String> {
    let id = a.positional().ok_or("用法: phonefarm stats <局ID前缀> [--task T] [--json]")?;
    let (task, run, dir) = resolve_run(&id, a.opt("--task").as_deref())?;
    let recs = read_jsonl(&dir.join("log.jsonl"));
    let s = run_summary(&recs);
    let tele: Vec<&Value> = recs.iter().filter(|r| r["r"] == "telemetry").collect();
    let series = |k: &str| tele.iter().filter_map(|t| t[k].as_f64()).collect::<Vec<f64>>();
    let minmax = |k: &str| {
        let v = series(k);
        (v.iter().cloned().fold(f64::NAN, f64::min), v.iter().cloned().fold(f64::NAN, f64::max))
    };
    let events = recs.iter().filter(|r| r["r"] == "app_event").count();
    let ledger_bytes = std::fs::metadata(dir.join("log.jsonl")).map(|m| m.len()).unwrap_or(0);
    let mut counts: Vec<(String, usize)> = {
        let mut m = std::collections::HashMap::new();
        for r in &recs { *m.entry(r["r"].as_str().unwrap_or("?").to_string()).or_insert(0) += 1; }
        m.into_iter().collect()
    };
    counts.sort();
    let out = json!({
        "task": task, "run": run, "summary": s, "record_counts": counts,
        "ledger_bytes": ledger_bytes, "telemetry_samples": tele.len(), "app_events": events,
        "fps": stat3(series("fps")), "cpu_pct": stat3(series("cpu_total_pct")),
        "step_ms": stat3(series("step_ms")), "api_ms": stat3(series("api_ms")),
        "mem_avail_kb": {"min": minmax("mem_avail_kb").0, "max": minmax("mem_avail_kb").1},
        "vm_rss_kb": {"min": minmax("vm_rss_kb").0, "max": minmax("vm_rss_kb").1},
        "batt_temp_c": {"min": minmax("batt_temp_c").0, "max": minmax("batt_temp_c").1},
    });
    if a.flag("--json") { println!("{out}"); return Ok(()); }
    println!("{}", fmt_summary(&task, &run, &dir, &s));
    println!("记录: {}", out["record_counts"].as_array().unwrap().iter()
        .map(|kv| format!("{}×{}", kv[0].as_str().unwrap_or("?"), kv[1])).collect::<Vec<_>>().join(" "));
    println!("账本体积: {}KB | 遥测样本 {} 条 | 事件 {} 条", ledger_bytes / 1024, tele.len(), events);
    for (k, label) in [("fps", "帧率"), ("cpu_pct", "CPU%"), ("step_ms", "步耗时ms"), ("api_ms", "API延迟ms")] {
        match &out[k] {
            Value::Null => println!("  {label}: 无数据"),
            v => println!("  {label}: 均值{} p50={} p95={}", v[0], v[1], v[2]),
        }
    }
    if let Some(c) = tele.iter().find_map(|t| t["cold_start_ms"].as_i64()) {
        println!("  冷启动: {c}ms");
    }
    for (k, label) in [("mem_avail_kb", "可用内存kB"), ("vm_rss_kb", "AppRSSkB"), ("batt_temp_c", "电池℃")] {
        let v = &out[k];
        if v["min"].as_f64().is_some_and(|f| !f.is_nan()) {
            println!("  {label}: {}~{}", v["min"], v["max"]);
        }
    }
    Ok(())
}

fn cmd_device_shell(a: &Args, readonly: bool) -> Result<(), String> {
    let serial = a.opt("--serial").ok_or("需要 --serial <设备>(hdc 目标用 hdc:<key> 前缀)")?;
    let cmd = a.positional().ok_or("需要一条设备命令(整体加引号)")?;
    if !readonly && !a.flag("--yes") {
        return Err("exec 会改设备状态,确认无误请加 --yes(只读查询请改用 probe)".into());
    }
    let tmp = std::env::temp_dir().join("phonefarm-cli").to_string_lossy().to_string();
    let _ = std::fs::create_dir_all(&tmp);
    let phone = crate::device::Device::new(Some(serial), tmp);
    print!("{}", phone.shell(&cmd, 15000));
    Ok(())
}

/// CLI 子命令分发。命中返回退出码;未命中返回 None(主程序打用法)。
pub fn dispatch(cmd: &str, rest: &[String]) -> Option<i32> {
    let a = Args::new(rest);
    let r = match cmd {
        "last" => cmd_last(&a),
        "runs" => cmd_runs(&a),
        "show" => cmd_show(&a),
        "cat" => cmd_cat(&a),
        "schema" => cmd_schema(&a),
        "tree" => cmd_tree(&a),
        "lessons" => cmd_lessons(&a),
        "campaign" => cmd_campaign(&a),
        "tasks" => cmd_tasks(&a),
        "config" => cmd_config(&a),
        "stats" => cmd_stats(&a),
        "status" => cmd_status(&a),
        "probe" => cmd_device_shell(&a, true),
        "exec" => cmd_device_shell(&a, false),
        _ => return None,
    };
    Some(match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pf_cli_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tasks/演示/runs/20260831-100000")).unwrap();
        std::fs::create_dir_all(d.join("tasks/演示/runs/20260831-110000")).unwrap();
        d
    }

    #[test]
    fn run_summary_prefers_end_record_and_derives_for_old() {
        // 新局: r=end 权威;旧局: act/raw/verdict 派生 + 如实标注 derived
        let recs: Vec<Value> = vec![
            json!({"r":"act","n":1}), json!({"r":"raw","n":1}),
            json!({"r":"end","stop":"done","achieved":true,"steps":5,"calls":7,"tokens":123,"wall_ms":9000}),
        ];
        let s = run_summary(&recs);
        assert_eq!(s["stop"], "done");
        assert_eq!(s["tokens"], 123);
        assert!(s.get("derived").is_none());
        let old: Vec<Value> = vec![
            json!({"r":"act","n":1}), json!({"r":"act","n":2}),
            json!({"r":"raw","n":1}), json!({"r":"raw","n":2}), json!({"r":"raw","n":3}),
            json!({"r":"hook","kind":"verdict","achieved":true,"reason":"ok"}),
        ];
        let s2 = run_summary(&old);
        assert_eq!(s2["steps"], 2);
        assert_eq!(s2["calls"], 3);
        assert_eq!(s2["achieved"], true);
        assert_eq!(s2["derived"], true, "旧局派生要如实标注");
    }

    #[test]
    fn fuzzy_run_resolution_in_scratch_dir() {
        // env 注入数据根,不 chdir(chdir 是进程级状态,会与并行测试的相对路径互相打翻)
        let d = scratch("fuzzy");
        std::env::set_var("PF_TASKS_ROOT", d.join("tasks").as_os_str());
        // 前缀多命中 → 报候选;补长唯一 → 命中;无命中 → 报错
        let many = resolve_run("20260831", None);
        assert!(many.is_err() && many.unwrap_err().contains("命中 2 局"));
        let one = resolve_run("20260831-11", None).unwrap();
        assert_eq!(one.1, "20260831-110000");
        assert_eq!(one.0, "演示");
        assert!(resolve_run("2027", None).is_err());
        // 空数据降级: 指到空 tasks 目录,latest_task 如实 None 不 panic
        let empty = d.join("empty_tasks");
        std::fs::create_dir_all(&empty).unwrap();
        std::env::set_var("PF_TASKS_ROOT", empty.as_os_str());
        assert!(latest_task().is_none());
        std::env::remove_var("PF_TASKS_ROOT");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cat_filter_and_schema_cover_all_types() {
        let t = "a1\nb2\na3\nb4\na5";
        assert_eq!(filter_lines(t, Some("a"), None, None), "a1\na3\na5");
        assert_eq!(filter_lines(t, Some("a"), Some(2), None), "a1\na3");
        assert_eq!(filter_lines(t, None, None, Some(2)), "a5".to_string().replace("a5", "b4\na5"));
        // schema 覆盖账本里全部 r 类型(含遥测三件)
        for ty in ["goal", "screen", "act", "diff", "raw", "hook", "probe", "lesson",
                   "telemetry", "app_event", "end", "trace", "note"] {
            assert!(SCHEMA_MD.contains(&format!("| {ty} ")), "schema 缺 {ty}");
        }
    }

    #[test]
    fn status_three_states() {
        // end→finished;start+活pid→running;start+死pid→died;无标记旧局→finished
        let fin = vec![json!({"r":"end","stop":"done"})];
        assert_eq!(status_of(&fin, |_| true).0, "finished");
        let me = std::process::id() as i64;
        let run = vec![json!({"r":"start","pid":me})];
        assert_eq!(status_of(&run, |p| p == me).0, "running");
        assert_eq!(status_of(&run, |_| false), ("died", Some(me)));
        let old: Vec<Value> = vec![json!({"r":"goal"})];
        assert_eq!(status_of(&old, |_| true).0, "finished", "旧局无标记按已结束");
        // detach 参数剥离: 只去 --detach,其余原样(含顺序)
        let args: Vec<String> = ["run","--task","T","--detach","目标"].iter().map(|s| s.to_string()).collect();
        assert_eq!(crate::strip_detach(&args), vec!["run","--task","T","目标"]);
    }

    #[test]
    fn stat3_percentiles() {
        let (mean, p50, p95) = stat3(vec![1.0, 2.0, 3.0, 4.0, 100.0]).unwrap();
        assert_eq!(p50, 3.0);
        assert_eq!(p95, 100.0);
        assert!((mean - 22.0).abs() < 0.1);
        assert!(stat3(vec![]).is_none(), "空序列如实 None");
    }
}
