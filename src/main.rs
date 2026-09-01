//! phonefarm v0.2 — 记录契约 v1 运行时
//! 用法: phonefarm run --task <任务名> [--serial <设备>] [--endless] [--budget-calls N] [--app <包名>] "<目标>"
//!       phonefarm devices
//! --app: 任务的目标应用包名;开局若前台不是它(也不是桌面),先按HOME归位再进循环
//! --serial 带 "hdc:<connect key>" 前缀走 OpenHarmony/hdc 后端,不带前缀=Android/adb(devices 子命令两族并列)
mod brain;
mod cli;
mod parallel;
mod device;
mod fold;
mod runtime;
mod telemetry;
mod tree;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Deserialize, Serialize)]
pub struct HookCfg {
    pub on: String,
    #[serde(default)]
    pub prompt: Option<String>,
    /// 确定性时间点(无模型调用): "budget" | "heal"
    #[serde(default)]
    pub builtin: Option<String>,
    #[serde(default)]
    pub input: Option<Vec<String>>,
    #[serde(default)]
    pub output: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "d_data_dir")]
    pub data_dir: String,
    #[serde(default = "d_max_steps")]
    pub max_steps: u32,
    #[serde(default = "d_settle")]
    pub settle_ms: u64,
    #[serde(default = "d_els_to")]
    pub els_timeout_ms: u64,
    #[serde(default = "d_ban_radius")]
    pub ban_radius: i32,
    #[serde(default = "d_ban_strikes")]
    pub ban_strikes: u32,
    #[serde(default = "d_stall")]
    pub stall_limit: u32,
    #[serde(default = "d_heal_bt")]
    pub heal_ban_threshold: u32,
    #[serde(default = "d_note_max")]
    pub note_max_chars: usize,
    #[serde(default = "d_lesson_max")]
    pub lesson_max_items: usize,
    #[serde(default = "d_window")]
    pub window_pairs: usize,
    #[serde(default = "d_plan_max")]
    pub plan_max: usize,
    /// 设备复活用: 模拟器启动命令(空=只重启adb不重启模拟器)
    #[serde(default)]
    pub emulator_cmd: String,
    /// #21 done预检: 首次done不定局,注入事实给模型一次终态自查(默认开;false=旧行为)
    #[serde(default = "d_done_reflect")]
    pub done_reflect: bool,
    /// 遥测(Telemetry Spec v1.0): 每步只读采集性能账进 log.jsonl,不进模型上下文(默认开)
    #[serde(default = "d_telemetry")]
    pub telemetry: bool,
    /// 重量级遥测明细的采集间隔(步);高频字段每步采
    #[serde(default = "d_tele_interval")]
    pub telemetry_interval: u32,
    #[serde(default)]
    pub prompts: HashMap<String, String>,
    #[serde(default, rename = "hook")]
    pub hooks: Vec<HookCfg>,
    pub providers: Vec<brain::ProviderCfg>,
}
fn d_data_dir() -> String { ".".into() }
fn d_max_steps() -> u32 { 12 }
fn d_settle() -> u64 { 1200 }
fn d_els_to() -> u64 { 2500 }
fn d_ban_radius() -> i32 { 30 }
fn d_ban_strikes() -> u32 { 2 }
fn d_stall() -> u32 { 6 }
fn d_heal_bt() -> u32 { 5 }
fn d_note_max() -> usize { 200 }
fn d_lesson_max() -> usize { 20 }
fn d_window() -> usize { 5 }
fn d_plan_max() -> usize { 4 }
fn d_done_reflect() -> bool { true }
fn d_telemetry() -> bool { true }
fn d_tele_interval() -> u32 { 5 }

impl Config {
    pub fn hook_for(&self, on: &str) -> Option<&HookCfg> {
        self.hooks.iter().find(|h| h.on == on)
    }
    pub fn prompt_of(&self, hook: &HookCfg) -> Option<&str> {
        hook.prompt
            .as_deref()
            .and_then(|n| self.prompts.get(n).map(|s| s.as_str()))
    }
}

const USAGE: &str = "phonefarm v0.2 — 记录契约 v1 运行时
跑局:  run --task <T> [--serial S] [--endless] [--budget-calls N] [--app P] [--assert \"词1,词2\"] \"<目标>\"
评测:  benchmark --task <T> [--rounds N] [--app P] [--assert ..] [--json] \"<目标>\"
并行:  parallel --job \"任务|目标|serial[|app[|assert]]\" [--job ...] [--budget-calls N] [--endless]
设备:  devices | probe --serial <S> \"只读命令\" | exec --serial <S> \"命令\" --yes
后台:  run/benchmark 加 --detach 立即回报局ID后台跑;phonefarm status [<局ID>|--task T] 查 运行中/已结束/中断
查看:  last | runs [--task T] | show <局ID> [--step N|--raw|--hooks|--events|--crashes|--anr|--trace]
       cat <路径> [--head/--tail N] [--grep 词] | stats <局ID> | tasks | tree | lessons | campaign
       schema [--type r类型] | config [--key k]     (查看类全部支持 --json,只读盘不烧token)";

/// secrets.env 解析(Improve Spec): 只认 `export KEY="v"` / `KEY=v` 形态的行,
/// 等价 source 语义但绝不执行任何命令。纯函数供单测。
fn parse_secrets(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') { continue; }
        let l = l.strip_prefix("export ").unwrap_or(l).trim();
        let Some((k, v)) = l.split_once('=') else { continue };
        let k = k.trim();
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') { continue; }
        let v = v.trim().trim_matches('"').trim_matches('\'');
        if v.contains('$') || v.contains('`') { continue; } // 不做任何展开/执行
        out.push((k.to_string(), v.to_string()));
    }
    out
}

/// key 自举: provider 链的 key_env 有缺 → 自动读 ./secrets.env(仅本地);仍全缺 → 警告+格式说明+退出。
/// 用户已 export 的值最高,不覆盖。部分缺失只提示不拦(存活 provider 可接力)。
fn ensure_keys(cfg: &Config) {
    let mut envs: Vec<&str> = Vec::new();
    for p in &cfg.providers {
        if !envs.contains(&p.key_env.as_str()) { envs.push(p.key_env.as_str()); }
    }
    let missing = |es: &[&str]| -> Vec<String> {
        es.iter().filter(|e| std::env::var(e).map(|v| v.trim().is_empty()).unwrap_or(true))
            .map(|e| e.to_string()).collect()
    };
    let mut miss = missing(&envs);
    if !miss.is_empty() {
        if let Ok(text) = std::fs::read_to_string("secrets.env") {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = std::fs::metadata("secrets.env") {
                    if m.permissions().mode() & 0o037 != 0 {
                        println!("(提示: secrets.env 权限过宽,建议 chmod 600 secrets.env)");
                    }
                }
            }
            for (k, v) in parse_secrets(&text) {
                if miss.contains(&k) {
                    std::env::set_var(&k, &v);
                    println!("(secrets: 从 ./secrets.env 读到 {k})");
                }
            }
            miss = missing(&envs);
        }
    }
    if miss.len() == envs.len() {
        eprintln!("⚠ 未配置模型 key(缺: {})。", miss.join(", "));
        eprintln!("请在仓库根创建 secrets.env(格式见 secrets.env.example):");
        eprintln!("    export GLM_KEY=\"你的智谱key(Coding套餐)\"");
        eprintln!("或先 export {}=... 再重跑。", miss.first().map(String::as_str).unwrap_or("GLM_KEY"));
        std::process::exit(2);
    } else if !miss.is_empty() {
        println!("(提示: {} 未配置,对应 provider 将失效,链上其余接力)", miss.join(", "));
    }
}

/// detach 参数剥离(纯函数供单测): 子进程用同参重入,仅去掉 --detach 本身
fn strip_detach(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| a.as_str() != "--detach").cloned().collect()
}

/// 后台分离自进程: 同参重入(去 --detach),stdout/stderr 归 console 文件,
/// 新进程组免受调用方作业信号牵连(nohup 收编)。返回子进程 pid。
fn spawn_detached(console: &str, envs: &[(&str, &str)]) -> std::io::Result<u32> {
    let exe = std::env::current_exe()?;
    let args = strip_detach(&std::env::args().skip(1).collect::<Vec<_>>());
    let f = std::fs::File::create(console)?;
    let f2 = f.try_clone()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&args)
        .stdout(f).stderr(f2).stdin(std::process::Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    Ok(cmd.spawn()?.id())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("parallel") => {
            // 多设备并行(PARALLEL_SPEC): 自进程 fan-out,行级设备前缀,任一失败整体非0
            std::process::exit(parallel::run_parallel(&args[1..]));
        }
        Some("devices") => {
            // 两族并列,各自 best-effort(某族工具不在 PATH 就跳过):
            // hdc 目标直接以 "hdc:<key>" 形态给出,拷进 --serial 即用
            if let Some(adb) = device::locate_adb() {
                if let Ok(out) = std::process::Command::new(&adb).arg("devices").output() {
                    print!("{}", String::from_utf8_lossy(&out.stdout));
                }
            } else {
                eprintln!("(未找到 adb,仅列 hdc;ADB_BIN=/path/to/adb 可指定)");
            }
            if let Ok(out) = std::process::Command::new("hdc").args(["list", "targets"]).output() {
                for l in String::from_utf8_lossy(&out.stdout).lines() {
                    let l = l.trim();
                    if !l.is_empty() && l != "[Empty]" {
                        println!("hdc:{l}");
                    }
                }
            }
        }
        Some("run") => {
            let mut serial: Option<String> = None;
            let mut goal = String::new();
            let mut task = String::new();
            let mut endless = false;
            let mut budget: u32 = 40;
            let mut app: Option<String> = None;
            let mut asserts: Vec<String> = Vec::new();
            let mut detach = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--serial" => serial = it.next().cloned(),
                    "--task" => task = it.next().cloned().unwrap_or_default(),
                    "--endless" => endless = true,
                    "--detach" => detach = true,
                    "--budget-calls" => budget = it.next().and_then(|v| v.parse().ok()).unwrap_or(40),
                    "--app" => app = it.next().cloned(),
                    "--assert" => {
                        // 验收词(可逗号分隔多个,英文/中文逗号都认): 契约式到达断言
                        if let Some(v) = it.next() {
                            for w in v.split(|c| c == ',' || c == '、') {
                                let w = w.trim();
                                if !w.is_empty() { asserts.push(w.to_string()); }
                            }
                        }
                    }
                    _ => goal = a.clone(),
                }
            }
            if goal.is_empty() || task.is_empty() {
                eprintln!("用法: phonefarm run --task <任务名> [--serial <设备>] [--endless] [--budget-calls N] [--app <包名>] [--assert \"词1,词2\"] \"<目标>\"");
                std::process::exit(2);
            }
            let cfg_text = match std::fs::read_to_string("phonefarm.toml") {
                Ok(s) => s,
                Err(e) => { eprintln!("读不到 phonefarm.toml: {e}"); std::process::exit(2); }
            };
            let cfg: Config = match toml::from_str(&cfg_text) {
                Ok(c) => c,
                Err(e) => { eprintln!("phonefarm.toml 解析失败: {e}"); std::process::exit(2); }
            };
            if !cfg.prompts.contains_key("step") {
                eprintln!("phonefarm.toml 缺 [prompts].step");
                std::process::exit(2);
            }
            if detach {
                // 先起跑回头取结果: 预分配局ID→建目录→分离子进程→立即回报(取结果走 status/show)
                let id = runtime::alloc_run_id();
                let run_dir = format!("{}/tasks/{}/runs/{}", cfg.data_dir.trim_end_matches('/'), task, id);
                if std::fs::create_dir_all(&run_dir).is_err() {
                    eprintln!("✗ 建不了运行目录 {run_dir}");
                    std::process::exit(2);
                }
                let console = format!("{run_dir}/console.log");
                match spawn_detached(&console, &[("PF_RUN_ID", &id)]) {
                    Ok(pid) => {
                        println!("已后台起跑: run={id}\n目录: {run_dir}\n控制台: {console}\npid: {pid}\n取结果: phonefarm status {id} | show {id}");
                        std::process::exit(0);
                    }
                    Err(e) => { eprintln!("detach 失败: {e}"); std::process::exit(2); }
                }
            }
            ensure_keys(&cfg);
            let res = runtime::episode(&cfg, &task, &goal, serial, endless, budget, app, asserts);
            println!("summary: run={} stop={} steps={} calls={} tokens={} wall={:.1}s achieved={}",
                res.run_id, res.stop, res.steps, res.calls, res.tokens,
                res.wall_ms as f64 / 1000.0, res.achieved);
            std::process::exit(if res.achieved { 0 } else { 1 });
        }
        Some("benchmark") => {
            // 自闭环评测: 每轮 体检→复活→轮间清理→跑局→原生指标入 campaign.tsv;--json 出结构化报告
            let mut serial: Option<String> = None;
            let mut goal = String::new();
            let mut task = String::new();
            let mut rounds: u32 = 1;
            let mut budget: u32 = 40;
            let mut app: Option<String> = None;
            let mut asserts: Vec<String> = Vec::new();
            let mut as_json = false;
            let mut detach = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--serial" => serial = it.next().cloned(),
                    "--task" => task = it.next().cloned().unwrap_or_default(),
                    "--detach" => detach = true,
                    "--rounds" => rounds = it.next().and_then(|v| v.parse().ok()).unwrap_or(1),
                    "--budget-calls" => budget = it.next().and_then(|v| v.parse().ok()).unwrap_or(40),
                    "--app" => app = it.next().cloned(),
                    "--assert" => {
                        if let Some(v) = it.next() {
                            for w in v.split(|c| c == ',' || c == '、') {
                                let w = w.trim();
                                if !w.is_empty() { asserts.push(w.to_string()); }
                            }
                        }
                    }
                    "--json" => as_json = true,
                    _ => goal = a.clone(),
                }
            }
            if goal.is_empty() || task.is_empty() {
                eprintln!("用法: phonefarm benchmark --task <任务名> [--rounds N] [--budget-calls N] [--app <包名>] [--assert \"词1,词2\"] [--json] \"<目标>\"");
                std::process::exit(2);
            }
            let cfg_text = match std::fs::read_to_string("phonefarm.toml") {
                Ok(s) => s,
                Err(e) => { eprintln!("读不到 phonefarm.toml: {e}"); std::process::exit(2); }
            };
            let cfg: Config = match toml::from_str(&cfg_text) {
                Ok(c) => c,
                Err(e) => { eprintln!("phonefarm.toml 解析失败: {e}"); std::process::exit(2); }
            };
            if !cfg.prompts.contains_key("step") {
                eprintln!("phonefarm.toml 缺 [prompts].step");
                std::process::exit(2);
            }
            let task_root_early = format!("{}/tasks/{}", cfg.data_dir.trim_end_matches('/'), task);
            if detach {
                // 多轮评测后台化: console 沿用 campaign_<stamp>.out 命名惯例;进度用 runs/campaign/status --task 轮询
                let _ = std::fs::create_dir_all(&task_root_early);
                let stamp = runtime::alloc_run_id();
                let console = format!("{task_root_early}/campaign_{stamp}.out");
                match spawn_detached(&console, &[]) {
                    Ok(pid) => {
                        println!("已后台起跑 benchmark: 任务[{task}] {rounds}轮\n控制台: {console}\npid: {pid}\n进度: phonefarm status --task {task} | runs --task {task} | campaign --task {task}");
                        std::process::exit(0);
                    }
                    Err(e) => { eprintln!("detach 失败: {e}"); std::process::exit(2); }
                }
            }
            ensure_keys(&cfg);
            let task_root = task_root_early;
            let _ = std::fs::create_dir_all(&task_root);
            let tsv = format!("{task_root}/campaign.tsv");
            if std::fs::metadata(&tsv).is_err() {
                let _ = std::fs::write(&tsv,
                    "round\trun_id\texit\tachieved\tstop\tsteps\tcalls\ttokens\twall_s\n");
            }
            let append = |line: &str| {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&tsv) {
                    let _ = writeln!(f, "{line}");
                }
            };
            let tmp = std::env::temp_dir().join("phonefarm-bench").to_string_lossy().to_string();
            let _ = std::fs::create_dir_all(&tmp);
            let phone = device::Device::new(serial.clone(), tmp);
            let apps = phone.launchable_apps();
            let mut rows: Vec<serde_json::Value> = Vec::new();
            for r in 1..=rounds {
                println!("══ benchmark 第{r}/{rounds}轮 ══");
                // 体检,不行就地复活(原round.sh职责,已收编)
                if !phone.health_check(15000) {
                    phone.revive(&cfg.emulator_cmd);
                    if !phone.health_check(15000) {
                        eprintln!("✗ 设备无响应且复活失败,第{r}轮记EMULATOR_DEAD");
                        append(&format!("{r}\t-\t9\tfalse\tEMULATOR_DEAD\t0\t0\t0\t0"));
                        rows.push(serde_json::json!({"round": r, "exit": 9, "achieved": false, "stop": "EMULATOR_DEAD"}));
                        continue;
                    }
                }
                // 轮间清理: 强停→回桌面→冷启主Activity,统一起跑线
                if let Some(pkg) = &app {
                    phone.force_stop(pkg);
                    phone.home();
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    if let Some((_, comp)) = apps.iter().find(|(p, _)| p == pkg) {
                        let _ = phone.launch(pkg, comp); // 冷启动毫秒由 run 内的遥测层消费,benchmark 不重复记
                    }
                    std::thread::sleep(std::time::Duration::from_secs(6));
                }
                let t0 = std::time::Instant::now();
                let res = runtime::episode(&cfg, &task, &goal, serial.clone(), true, budget, app.clone(), asserts.clone());
                let wall = t0.elapsed().as_secs();
                append(&format!(
                    "{r}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{wall}",
                    res.run_id, if res.achieved { 0 } else { 1 }, res.achieved, res.stop,
                    res.steps, res.calls, res.tokens));
                rows.push(serde_json::json!({
                    "round": r, "run_id": res.run_id, "exit": if res.achieved { 0 } else { 1 },
                    "achieved": res.achieved, "stop": res.stop, "steps": res.steps,
                    "calls": res.calls, "tokens": res.tokens, "wall_s": wall
                }));
            }
            if as_json {
                println!("{}", serde_json::to_string_pretty(&rows).unwrap_or_default());
            }
            let last_ok = rows.last().and_then(|r| r["achieved"].as_bool()).unwrap_or(false);
            std::process::exit(if last_ok { 0 } else { 1 });
        }
        Some(other) => {
            // CLI 查看层(CLI Spec v1.0): 只读盘,不烧 token
            if let Some(code) = cli::dispatch(other, &args[1..]) {
                std::process::exit(code);
            }
            eprintln!("未知子命令 '{other}'");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
        None => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
