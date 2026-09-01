//! phonefarm v0.2 — 记录契约 v1 运行时
//! 用法: phonefarm run --task <任务名> [--serial <设备>] [--endless] [--budget-calls N] [--app <包名>] "<目标>"
//!       phonefarm devices
//! --app: 任务的目标应用包名;开局若前台不是它(也不是桌面),先按HOME归位再进循环
//! --serial 带 "hdc:<connect key>" 前缀走 OpenHarmony/hdc 后端,不带前缀=Android/adb(devices 子命令两族并列)
mod brain;
mod device;
mod fold;
mod runtime;
mod telemetry;
mod tree;

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
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

#[derive(Deserialize)]
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("devices") => {
            // 两族并列,各自 best-effort(某族工具不在 PATH 就跳过):
            // hdc 目标直接以 "hdc:<key>" 形态给出,拷进 --serial 即用
            if let Ok(out) = std::process::Command::new("adb").arg("devices").output() {
                print!("{}", String::from_utf8_lossy(&out.stdout));
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
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--serial" => serial = it.next().cloned(),
                    "--task" => task = it.next().cloned().unwrap_or_default(),
                    "--endless" => endless = true,
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
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--serial" => serial = it.next().cloned(),
                    "--task" => task = it.next().cloned().unwrap_or_default(),
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
            let task_root = format!("{}/tasks/{}", cfg.data_dir.trim_end_matches('/'), task);
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
        _ => {
            eprintln!("phonefarm v0.2 — 记录契约 v1 运行时\n  phonefarm run --task <任务名> [--assert \"词1,词2\"] \"<目标>\"\n  phonefarm benchmark --task <任务名> [--rounds N] [--app <包名>] [--assert \"词1,词2\"] [--json] \"<目标>\"\n  phonefarm devices");
            std::process::exit(2);
        }
    }
}
