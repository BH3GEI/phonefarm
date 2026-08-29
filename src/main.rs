//! phonefarm v0.2 — 记录契约 v1 运行时
//! 用法: phonefarm run --task <任务名> [--serial <设备>] [--endless] [--budget-calls N] "<目标>"
//!       phonefarm devices
mod brain;
mod device;
mod runtime;

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
            let out = std::process::Command::new("adb").arg("devices").output().unwrap();
            print!("{}", String::from_utf8_lossy(&out.stdout));
        }
        Some("run") => {
            let mut serial: Option<String> = None;
            let mut goal = String::new();
            let mut task = String::new();
            let mut endless = false;
            let mut budget: u32 = 40;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--serial" => serial = it.next().cloned(),
                    "--task" => task = it.next().cloned().unwrap_or_default(),
                    "--endless" => endless = true,
                    "--budget-calls" => budget = it.next().and_then(|v| v.parse().ok()).unwrap_or(40),
                    _ => goal = a.clone(),
                }
            }
            if goal.is_empty() || task.is_empty() {
                eprintln!("用法: phonefarm run --task <任务名> [--serial <设备>] [--endless] [--budget-calls N] \"<目标>\"");
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
            let ok = runtime::episode(&cfg, &task, &goal, serial, endless, budget);
            std::process::exit(if ok { 0 } else { 1 });
        }
        _ => {
            eprintln!("phonefarm v0.2 — 记录契约 v1 运行时\n  phonefarm run --task <任务名> \"<目标>\"\n  phonefarm devices");
            std::process::exit(2);
        }
    }
}
