//! 灰档 A/B 采集编排 (knobs/gray/genshin_loadop_ab.sh 与 genshin_copyprobe_ab.sh 的移植)。
//!
//! 两臂**都挂层**, 唯一差别是属性: ctrl = 层+改写关 (只读档), knob = 层+改写开。
//! 要判的是**这条改写**值不值, 不是"挂层这件事"值不值; 两臂都挂层才能把层本身的
//! 开销消掉, 差值才干净地对应改写这一个改动。
//!
//! 每臂都要重启原神 —— loadOp 是 render pass **创建期**烘进去的, 属性也是
//! CreateInstance 时读的, 跑起来之后切不动。
//!
//! 原神里**不按 A 键、不按 BACK**: 全程只在登录页点一次"门", 负载只在画面空白处拖视角。

use crate::eval::{adb, adb_ok, run_one, su};
use crate::graylayer::GrayLayer;
use crate::shotdiff;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 登录页那扇"门" (画面坐标, 点一次进游戏)
const GATE_X: u32 = 1342;
const GATE_Y: u32 = 651;
/// 大世界判定: 每帧 render pass 数 >= 25 (登录/加载 ≈ 12-17, 大世界 ≈ 30, 余量很大)
const IN_WORLD_PPF: i64 = 25;

fn mark_field(serial: &str, field: &str) -> Option<i64> {
    let out = su(
        serial,
        "cat /storage/emulated/0/Android/data/com.miHoYo.Yuanshen/files/knobs_layer_out.json 2>/dev/null",
        30,
    );
    let body = crate::graylayer::GrayLayer::adb_stdout(&out);
    // Python 版用 sed 提取 "field":<digits> —— 只认整数字段
    let pat = format!("\"{field}\":[0-9]*");
    let re = regex::Regex::new(&pat).ok()?;
    let m = re.captures(body)?;
    m[0]
        .split_once(':')
        .and_then(|(_, v)| v.parse::<i64>().ok())
}

/// 进没进大世界: 用**每帧 render pass 数**判, 不用帧率
/// (本机原神登录页和大世界都被限在 30fps)。
fn in_world(serial: &str) -> bool {
    let (f1, r1) = (
        mark_field(serial, "frames").unwrap_or(0),
        mark_field(serial, "render_pass_begins").unwrap_or(0),
    );
    std::thread::sleep(std::time::Duration::from_secs(10));
    let (f2, r2) = (
        mark_field(serial, "frames").unwrap_or(0),
        mark_field(serial, "render_pass_begins").unwrap_or(0),
    );
    let df = f2 - f1;
    let dr = r2 - r1;
    if df <= 0 {
        return false;
    }
    let ppf = dr / df;
    eprintln!("    passes/帧={ppf} (>= {IN_WORLD_PPF} 判为大世界)");
    ppf >= IN_WORLD_PPF
}

/// 等登录页的门出现再点; 点早了会落空。然后等进大世界 (passes/帧 >= 25)。
pub(crate) fn launch_to_world_layered(serial: &str) -> Result<(), String> {
    let mut i = 0u32;
    while i < 40 {
        if mark_field(serial, "frames").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        i += 1;
    }
    if mark_field(serial, "frames").is_none() {
        return Err("层没挂上, 本臂作废".into());
    }
    std::thread::sleep(std::time::Duration::from_secs(30));
    let _ = adb(serial, &["shell", &format!("input tap {GATE_X} {GATE_Y}")], 30);
    eprintln!("    已点门, 等进大世界...");
    for _ in 0..18 {
        if in_world(serial) {
            eprintln!("    已进大世界");
            return Ok(());
        }
    }
    Err("等不到大世界, 本臂作废".into())
}

/// 无层臂: 不推 .so 干净基线。进世界判定用截帧亮度差 (与"门"页比, >40% 判离开)。
fn arm_none(serial: &str, label: &str, outdir: &Path, workload: &Path, pf_bin: &str) -> Result<(), String> {
    let _ = std::fs::create_dir_all(outdir);
    eprintln!("── 臂 {label} (无层) ──");
    let act = adb(serial, &["shell", "cmd package resolve-activity --brief com.miHoYo.Yuanshen"], 30);
    let act = act.lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string()).unwrap_or_default();
    if act.contains('/') {
        let _ = adb(serial, &["shell", &format!("am start -n {act}")], 30);
    }
    // 等渲染起稳
    for _ in 0..30 {
        let p = adb(serial, &["shell", "pidof com.miHoYo.Yuanshen"], 15);
        if adb_ok(&p) && !crate::graylayer::GrayLayer::adb_stdout(&p).trim().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    std::thread::sleep(std::time::Duration::from_secs(25));
    // 记"门"页参考帧
    let gate_ref = outdir.join("gate_ref.raw");
    std::fs::write(&gate_ref, screencap_raw_bin(serial)).map_err(|e| e.to_string())?;
    let _ = adb(serial, &["shell", &format!("input tap {GATE_X} {GATE_Y}")], 30);
    eprintln!("    已点门, 等进大世界...");
    for _ in 0..24 {
        if left_gate(serial, &gate_ref) {
            eprintln!("    已离开登录页, 固等 60s 进大世界");
            std::thread::sleep(std::time::Duration::from_secs(60));
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    run_workload_and_shots(serial, label, outdir, workload, pf_bin)
}


/// 与"门"页参考帧比像素差, >40% = 已离开登录页 (采样步长 61, 阈值 16/通道)。
fn left_gate(serial: &str, gate_ref: &Path) -> bool {
    let Ok(a) = std::fs::read(gate_ref) else { return false };
    let cur = screencap_raw_bin(serial);
    if a.len() < 12 || cur.len() < 12 {
        return false;
    }
    let (aw, ah) = (
        u32::from_le_bytes(a[0..4].try_into().unwrap()) as usize,
        u32::from_le_bytes(a[4..8].try_into().unwrap()) as usize,
    );
    let (cw, ch) = (
        u32::from_le_bytes(cur[0..4].try_into().unwrap()) as usize,
        u32::from_le_bytes(cur[4..8].try_into().unwrap()) as usize,
    );
    if (aw, ah) != (cw, ch) {
        return false;
    }
    let (pa, pc) = (&a[12..], &cur[12..]);
    let n = pa.len().min(pc.len()) / 4;
    let (mut diff, mut tot) = (0usize, 0usize);
    let mut i = 0usize;
    while i < n {
        let o = i * 4;
        if (0..3).any(|c| (pa[o + c] as i32 - pc[o + c] as i32).abs() > 16) {
            diff += 1;
        }
        tot += 1;
        i += 61;
    }
    if tot == 0 {
        return false;
    }
    let r = diff as f64 / tot as f64;
    eprintln!("    与门页面差异={:.1}% (>40 判为已离开)", r * 100.0);
    r > 0.40
}

/// 二进制安全的 `adb exec-out screencap` (PNG 之外的原始 RGBA)。
pub(crate) fn screencap_raw_bin(serial: &str) -> Vec<u8> {
    std::process::Command::new(crate::eval::adb_path())
        .args(["-s", serial, "exec-out", "screencap"])
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default()
}

/// copyprobe 臂: 挂探针层 → 进大世界 → 负载 → 三张截帧 (离线比对用中间那张)。
fn arm_copyprobe(
    serial: &str,
    gl: &GrayLayer,
    label: &str,
    outdir: &Path,
    workload: &Path,
    pf_bin: &str,
) -> Result<(), String> {
    let _ = std::fs::create_dir_all(outdir);
    eprintln!("── 臂 {label} (copyprobe) ──");
    let _ = su(
        serial,
        "rm -f /storage/emulated/0/Android/data/com.miHoYo.Yuanshen/files/knobs_layer_out.json",
        15,
    );
    gl.mount_mode("copyprobe", "com.miHoYo.Yuanshen", false)?;
    launch_to_world_layered(serial)?;
    run_one(serial, label, outdir, 30, workload, "com.miHoYo.Yuanshen")?;
    // 负载结束人物站定后连拍三张, 离线比对用中间那张
    std::thread::sleep(std::time::Duration::from_secs(3));
    for k in 1..=3 {
        let f = outdir.join(format!("shot{k}.raw"));
        std::fs::write(&f, screencap_raw_bin(serial)).ok();
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    if let Ok(marker) = std::fs::read_to_string(outdir.join("knobs_layer_out.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&marker) {
            eprintln!(
                "  层自报: frames={} copyprobe={}",
                v["readonly_stats"]["frames"],
                serde_json::to_string(&v["copyprobe"]).unwrap_or_default()
            );
        }
    } else {
        eprintln!("  层自报: 解析失败");
    }
    let _ = pf_bin;
    Ok(())
}

/// 无层臂的负载 + 三张截帧。
fn run_workload_and_shots(
    serial: &str,
    label: &str,
    outdir: &Path,
    workload: &Path,
    pf_bin: &str,
) -> Result<(), String> {
    run_one(serial, label, outdir, 30, workload, "com.miHoYo.Yuanshen")?;
    std::thread::sleep(std::time::Duration::from_secs(3));
    for k in 1..=3 {
        let f = outdir.join(format!("shot{k}.raw"));
        std::fs::write(&f, screencap_raw_bin(serial)).ok();
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    let _ = pf_bin;
    Ok(())
}

const LOADOP_PROTOCOL: &str = "gray_ab_loadop_whole_round_charge_v1";
const LOADOP_SECONDS: u64 = 30;

fn save_json(path: &Path, value: &Value) -> Result<(), String> {
    crate::confirmation_transport::new_file(path, &serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?)
}

fn recover_loadop(args: &[String]) -> Result<(Value, i32), String> {
    let (mut dir, mut serial) = (None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--session" => dir = it.next(),
            "--serial" => serial = it.next(),
            "--json" => {},
            _ => return Err(format!("unknown gray-ab recover argument: {arg}")),
        }
    }
    let parent = crate::gray_eval_session::Parent::resume_loadop(
        Path::new(dir.ok_or("missing --session")?), serial.ok_or("missing --serial")?)?;
    let report = parent.finish(Some("explicit same-context recovery; original collection is unchanged"))?;
    let code = if report["status"] == "reported_complete" { 0 } else { 2 };
    Ok((report, code))
}

fn owned_screen(parent: &crate::gray_eval_session::Parent, path: &Path, plan: &crate::gray_script::Plan) -> Result<(u32, u32), String> {
    parent.base.screenshot(path)?;
    let raw = crate::frameaudit::png_raw(path)?;
    let (w, h, _) = crate::framecheck::parse_screencap(&raw)?;
    plan.validate_screen(w, h)?;
    Ok((w, h))
}

fn scene_counts(marker: &Value) -> Option<(u64, u64)> {
    // Current layer evidence uses readonly_stats; older artifacts used the root.
    let stats = if marker["readonly_stats"].is_object() { &marker["readonly_stats"] } else { marker };
    Some((stats["frames"].as_u64()?, stats["render_pass_begins"].as_u64()?))
}

fn loadop_world(parent: &crate::gray_eval_session::Parent, out: &Path, plan: &crate::gray_script::Plan) -> Result<(), String> {
    use std::time::{Duration, Instant};
    let start = Instant::now();
    loop {
        if !parent.base.target_alive()? { return Err("target exited before layer readiness".into()); }
        if parent.base.marker()?.as_ref().and_then(scene_counts).is_some() { break; }
        if start.elapsed().as_secs() >= 80 { return Err("current layer marker unavailable".into()); }
        std::thread::sleep(Duration::from_secs(2));
    }
    parent.base.narrow()?;
    std::thread::sleep(Duration::from_secs(30));
    let (w, h) = owned_screen(parent, &out.join("gate.png"), plan)?;
    if GATE_X >= w || GATE_Y >= h { return Err("scene entry coordinate outside actual screenshot".into()); }
    parent.base.tap(GATE_X, GATE_Y)?;
    for _ in 0..18 {
        let before = parent.base.marker()?.as_ref().and_then(scene_counts).ok_or("scene counters absent")?;
        std::thread::sleep(Duration::from_secs(10));
        if !parent.base.target_alive()? { return Err("target exited while waiting for world".into()); }
        let after = parent.base.marker()?.as_ref().and_then(scene_counts).ok_or("scene counters absent")?;
        if let (Some(df), Some(dr)) = (after.0.checked_sub(before.0), after.1.checked_sub(before.1)) {
            if df > 0 && dr / df >= IN_WORLD_PPF as u64 { return Ok(()); }
        }
    }
    Err("world scene counter condition not reached".into())
}

fn loadop_power(run: &Value, charge: &Value) -> Result<Value, String> {
    let env = &run["env"];
    let valid = env["on_battery"] == true && env["power_valid"] == true
        && env["power_window_verified"] == true && env["trace_window_verified"] == true
        && env["n_samples"].as_u64().is_some_and(|n| n > 0);
    let mut snap = crate::perfsrc::PerfSnapshot::empty("android_sysfs");
    snap.sample_count = env["n_samples"].as_u64().unwrap_or(0) as usize;
    snap.power_watt = if valid { env["power_w_mean"].as_f64() } else { None };
    if env["thermal_window_verified"] == true { snap.soc_temp_c = env["soc_temp_mean_c"].as_f64(); }
    for (field, reason) in [
        ("fps", "frame timing is recorded separately in summary.json"),
        ("frame_time_mean_ms", "frame timing is recorded separately in summary.json"),
        ("fps_p95_ms", "frame timing is recorded separately in summary.json"),
        ("gpu_temp_c", "not sampled by the owned environment sampler"),
        ("battery_temp_c", "not sampled by the owned environment sampler"),
    ] { snap.unavailable.push(crate::perfsrc::Unavailable { field: field.into(), reason: reason.into() }); }
    if snap.soc_temp_c.is_none() { snap.unavailable.push(crate::perfsrc::Unavailable { field: "soc_temp_c".into(), reason: "complete thermal window unavailable".into() }); }
    if snap.power_watt.is_none() { snap.unavailable.push(crate::perfsrc::Unavailable { field: "power_watt".into(), reason: "battery discharge or complete sampler/trace window unverified".into() }); }
    snap.meta = Some(std::collections::BTreeMap::from([
        ("power_rail".into(), "battery".into()), ("protocol".into(), LOADOP_PROTOCOL.into()),
        ("on_battery".into(), (env["on_battery"] == true).to_string()),
        ("battery_status".into(), env["battery_status"].as_str().unwrap_or("unknown").into()),
        ("scope".into(), "whole sampler window including lead/tail; covers trace capture window".into()),
    ]));
    let mut report = serde_json::to_value(snap).map_err(|e| e.to_string())?;
    report["protocol"] = json!(LOADOP_PROTOCOL);
    report["window"] = json!({"duration_s":env["sample_duration_s"],"interval_s":env["sample_interval_s"],
        "first_uptime":env["sample_first_uptime"],"last_uptime":env["sample_last_uptime"],
        "trace_window_verified":env["trace_window_verified"],"power_window_verified":env["power_window_verified"]});
    report["charging"] = charge.clone();
    report["legacy_charge_header_used"] = json!(false);
    Ok(report)
}

fn collect_loadop(lease: crate::devlease::Lease, out: &Path, pairs: u32, plan: &crate::gray_script::Plan, origin: &str) -> Result<(Value, i32), String> {
    let serial = lease.serial.clone();
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let out = out.canonicalize().map_err(|e| e.to_string())?;
    let out = out.as_path();
    // Refuse old arm directories before any device mutation, even for a fresh session.
    for i in 1..=pairs { for prefix in ["ctrl", "knob"] {
        match std::fs::symlink_metadata(out.join(format!("{prefix}{i}"))) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e.to_string()),
            Ok(_) => return Err(format!("existing {prefix}{i} evidence; use a new output directory")),
        }
    }}
    let base = crate::gray_session::Session::new_loadop(lease, out, crate::plugins::genshin::PACKAGE, origin)?;
    let parent = crate::gray_eval_session::Parent::new_loadop(base, &json!({"protocol":LOADOP_PROTOCOL,"pairs":pairs}), plan)?;
    let mut arms = Vec::new();
    let collected = (|| -> Result<(), String> {
        let caps = parent.capabilities()?;
        if caps.root != Some(true) || caps.trace_available != Some(true) { return Err("root and isolated ftrace are required".into()); }
        parent.base.prepare()?;
        parent.base.install_loadop(None)?;
        parent.base.eval_stop_target()?;
        parent.deploy()?;
        parent.prepare_charging()?;
        for i in 1..=pairs { for (prefix, rewrite) in [("ctrl", false), ("knob", true)] {
            let label = format!("{prefix}{i}"); let dir = out.join(&label);
            let result = (|| -> Result<Value, String> {
                std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
                parent.admission_ready()?;
                parent.stage(&label)?;
                parent.base.restart_loadop(&label, rewrite)?;
                loadop_world(&parent, &dir, plan)?;
                owned_screen(&parent, &dir.join("scene.png"), plan)?;
                let before = parent.charging_observation()?;
                let run = crate::eval::run_owned_gray_script(&parent, plan, &label, &dir)?;
                let after = parent.charging_observation()?;
                let marker = parent.base.marker()?.ok_or("current arm layer evidence absent")?;
                save_json(&dir.join("knobs_layer_out.json"), &marker)?;
                let power = loadop_power(&run, &json!({"before":before,"after":after}))?;
                save_json(&dir.join("power.json"), &power)?;
                if power["power_watt"].is_null() { return Err("complete battery power window unavailable; raw evidence retained".into()); }
                parent.admission_ready()?;
                let spf = run["summary"]["submits_per_frame"].as_i64();
                let p50 = run["summary"]["frame_p50"].as_f64();
                let advisory = spf == Some(2) && p50.is_some_and(|v| (15.5..=18.0).contains(&v) || (31.0..=36.0).contains(&v));
                Ok(json!({"run":dir.join("run.json"),"power":dir.join("power.json"),"layer":dir.join("knobs_layer_out.json"),
                    "scene_counter_check":true,"legacy_timing_advisory_ok":advisory,"rewrite_effective":marker["effective"]}))
            })();
            arms.push(json!({"arm":label,"status":if result.is_ok(){"collected"}else{"failed"},"rewrite":rewrite,"passdump":0,
                "evidence":result.as_ref().ok(),"error":result.as_ref().err()}));
            result?;
        }}
        Ok(())
    })();
    let collection = json!({"schema":"phonefarm/gray-ab-loadop-attempt@1","session":parent.base.dir,
        "protocol":LOADOP_PROTOCOL,"collection_ok":collected.is_ok(),"arms":arms,"error":collected.as_ref().err()});
    let persistence_error = save_json(&parent.base.dir.join("gray-ab-collection.json"), &collection).err();
    let collection_ok = collected.is_ok();
    let error = match (collected.err(), persistence_error.as_deref()) {
        (Some(e), Some(p)) => Some(format!("{e}; collection persistence: {p}")),
        (None, Some(p)) => Some(format!("collection persistence: {p}")),
        (e, None) => e,
    };
    let recovery = parent.finish(error.as_deref()).unwrap_or_else(|e| json!({"status":"required","error":e,"session":parent.base.dir}));
    let mut code = if persistence_error.is_some() || recovery["status"] != "reported_complete" { 2 } else if error.is_some() { 1 } else { 0 };
    let path = out.join(format!("gray-ab-{}-{}.json", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    let mut report = json!({"schema":"phonefarm/gray-ab-collection@1","serial":serial,"mode":"loadop","protocol":LOADOP_PROTOCOL,
        "session":parent.base.dir,"evidence_origin":origin,"collection_ok":collection_ok,"arms":arms,"error":error,"persistence_error":persistence_error,
        "collection":parent.base.dir.join("gray-ab-collection.json"),"recovery":recovery,"recovery_status":recovery["status"],
        "job_recovery_complete":false,"game_state_restored":false,"report_path":path});
    if let Err(error) = save_json(&path, &report) {
        code = 2;
        report["report_path"] = Value::Null;
        report["report_write_error"] = json!({"path":path,"error":error});
    }
    Ok((report, code))
}

fn emit_owned(result: Result<(Value, i32), String>) -> i32 {
    match result {
        Ok((report, code)) => { println!("{}", serde_json::to_string_pretty(&report).unwrap()); code },
        Err(error) => { eprintln!("{error}"); println!("{}", json!({"error":error,"recovery_status":"not_started_or_unknown"})); 2 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loadop_power_requires_discharge_and_a_bound_window() {
        let mut run = json!({"env":{"on_battery":true,"power_valid":true,"power_window_verified":true,
            "trace_window_verified":true,"n_samples":20,"power_w_mean":6.0,
            "sample_duration_s":40,"sample_interval_s":2,"sample_first_uptime":98,"sample_last_uptime":136}});
        let good = loadop_power(&run, &json!({"mode":"observed_no_mutation"})).unwrap();
        assert_eq!(good["power_watt"], 6.0);
        assert_eq!(good["sample_count"], 20);
        assert_eq!(good["window"]["duration_s"], 40);
        assert!(good["fps"].is_null());
        run["env"]["trace_window_verified"] = json!(false);
        assert!(loadop_power(&run, &Value::Null).unwrap()["power_watt"].is_null());
        run["env"]["trace_window_verified"] = json!(true);
        run["env"]["on_battery"] = json!(false);
        assert!(loadop_power(&run, &Value::Null).unwrap()["power_watt"].is_null());
    }
}

pub fn run_gray_ab(args: &[String]) -> i32 {
    if args.first().is_some_and(|a| a == "recover") { return emit_owned(recover_loadop(&args[1..])); }
    let mut pairs = 5u32;
    let mut mode = "loadop".to_string();
    let mut serial = std::env::var("SERIAL").unwrap_or_else(|_| "91253241019A".into());
    let mut out = PathBuf::new();
    let mut origin = "device".to_string();
    let mut owned_options = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pairs" => match it.next().and_then(|v| v.parse().ok()) {
                Some(n) => pairs = n,
                None => { eprintln!("--pairs requires a positive integer"); return 2; },
            },
            "--mode" => mode = it.next().cloned().unwrap_or_else(|| "loadop".into()),
            "--serial" => serial = it.next().cloned().unwrap_or(serial),
            "--out" => out = it.next().map(PathBuf::from).unwrap_or_default(),
            "--json" => owned_options = true,
            "--evidence-origin" => { owned_options = true; origin = it.next().cloned().unwrap_or_default(); },
            other => {
                eprintln!("不认识的参数 {other}\n用法: phonefarm gray-ab [--pairs N] [--mode loadop|copyprobe] [--serial S] --out <目录>");
                return 2;
            }
        }
    }
    if !matches!(mode.as_str(), "loadop" | "copyprobe") {
        eprintln!("--mode 只认 loadop|copyprobe");
        return 2;
    }
    if out.as_os_str().is_empty() || pairs == 0 || !matches!(origin.as_str(), "device" | "synthetic") {
        eprintln!("gray-ab requires --out, positive --pairs and valid --evidence-origin"); return 2;
    }
    if mode == "copyprobe" && owned_options {
        eprintln!("copyprobe has not migrated to the owned loadop protocol; --json/--evidence-origin are loadop-only"); return 2;
    }
    if mode == "loadop" {
        return emit_owned((|| {
            let path = crate::eval::resolve_path("knobs/gray/workload_spin_touch_v1.json").ok_or("finite spin-touch workload unavailable")?;
            let input: Value = serde_json::from_slice(&crate::confirmation_transport::bounded(&path, 1024*1024)?).map_err(|e| e.to_string())?;
            let plan = crate::gray_script::parse(&input, LOADOP_SECONDS)?;
            let lease = crate::devlease::Lease::target(Some(&serial), "gray-ab")?;
            collect_loadop(lease, &out, pairs, &plan, &origin)
        })());
    }
    let _lease = match crate::devlease::Lease::target(Some(&serial), "gray-ab") {
        Ok(v) => v, Err(e) => { eprintln!("{e}"); return 2; }
    };
    serial = _lease.serial.clone();
    let probes = std::env::var("PROBES").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let gl = GrayLayer::new(&serial);
    let workload = match crate::eval::resolve_path("knobs/gray/workload_spin_touch_v1.json") {
        Some(p) => p,
        None => {
            eprintln!("找不到触控负载 knobs/gray/workload_spin_touch_v1.json");
            return 2;
        }
    };
    let pf_bin = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("phonefarm"))
        .to_string_lossy()
        .into_owned();
    if out.as_os_str().is_empty() || pairs == 0 || probes == 0 {
        eprintln!("gray-ab requires --out and positive --pairs/PROBES"); return 2;
    }
    if let Err(e)=std::fs::create_dir_all(&out) {eprintln!("{e}");return 2;}
    let mut arms:Vec<(String,bool)>=Vec::new();
    arms.extend([("none1".into(),false),("none2".into(),false)]);
    for i in 1..=probes {arms.push((format!("probe{i}"),true));}
    let mut records=Vec::new();
    let mut code=0;
    for (label,rewrite) in arms {
        let arm_dir=out.join(&label);
        if let Err(e)=std::fs::create_dir_all(&arm_dir) {eprintln!("{e}");code=2;break;}
        let before=gl.off(Some(crate::plugins::genshin::PACKAGE));
        let before_record=match crate::graylayer::record_deactivation(&arm_dir,"before",&before) {
            Ok(v)=>v,Err(e)=>{eprintln!("{e}");code=2;break;}
        };
        if let Err(e)=before {
            eprintln!("{label} 前置摘层失败，停止后续操作: {e}");
            records.push(json!({"arm":label,"status":"not_started","deactivation_before":before_record}));
            code=2;break;
        }
        let measured=if rewrite {arm_copyprobe(&serial,&gl,&label,&arm_dir,&workload,&pf_bin)}
        else {arm_none(&serial,&label,&arm_dir,&workload,&pf_bin)};
        // A failed experiment still retires its target. A retirement failure
        // ends this loop; no extra force-stop or next-arm writes follow it.
        let after=gl.off(Some(crate::plugins::genshin::PACKAGE));
        let after_record=match crate::graylayer::record_deactivation(&arm_dir,"after",&after) {
            Ok(v)=>v,Err(e)=>{eprintln!("{e}");code=2;break;}
        };
        records.push(json!({"arm":label,"status":if measured.is_ok(){"collected"}else{"failed"},
            "error":measured.as_ref().err(),"deactivation_before":before_record,"deactivation_after":after_record}));
        if let Err(e)=after {eprintln!("{label} 摘层失败，停止后续操作: {e}");code=2;break;}
        if let Err(e)=measured {eprintln!("{label} 作废，停止后续臂: {e}");code=1;break;}
    }
    if code==0 && mode=="copyprobe" {
        match shotdiff::report(&out) {Ok(t)=>println!("{t}"),Err(e)=>{eprintln!("{e}");code=1;}}
    }
    if let Err(e)=_lease.check() {eprintln!("{e}");code=2;}
    let report=json!({"schema":"phonefarm/gray-ab-collection@1","serial":serial,"mode":mode,
        "collection_ok":code==0,"arms":records,"job_recovery_complete":false,
        "game_state_restored":false,"recovery_status":"not_verified",
        "recovery_note":"Layer deactivation does not verify sampler, input, ftrace or charging recovery."});
    let path=out.join(format!("gray-ab-{}-{}.json",std::process::id(),chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    if let Err(e)=crate::confirmation_transport::new_file(&path,report.to_string().as_bytes()) {eprintln!("{e}");code=2;}
    println!("采集结束: exit={code}, 完整恢复尚未核验；证据 {}",path.display());
    code
}
