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
use serde_json::Value;
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
fn launch_to_world_layered(serial: &str) -> Result<(), String> {
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

#[allow(clippy::too_many_arguments)]
fn arm(
    serial: &str,
    gl: &GrayLayer,
    label: &str,
    rewrite: bool,
    outdir: &Path,
    workload: &Path,
    seconds: u64,
    pf_bin: &str,
) -> Result<(), String> {
    eprintln!("── 臂 {label} (rewrite={}) ──", rewrite as u8);
    let _ = gl.off();
    let _ = adb(serial, &["shell", "am force-stop com.miHoYo.Yuanshen"], 30);
    let _ = su(
        serial,
        "rm -f /storage/emulated/0/Android/data/com.miHoYo.Yuanshen/files/knobs_layer_out.json",
        15,
    );
    let mode = if rewrite { "loadop" } else { "target" };
    gl.mount_mode(mode, "com.miHoYo.Yuanshen", false)?;
    launch_to_world_layered(serial)?;
    // 功耗与帧数据同窗口采: run_once 的采集窗口是 t=6..36s, 这里 t=10 起采约 12s
    let pf = pf_bin.to_string();
    let power_path = outdir.join("power.json");
    let s2 = serial.to_string();
    let sampler = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(10));
        spawn_perf(&pf, &s2, &power_path);
    });
    let _ = run_one(serial, label, outdir, seconds, workload, "com.miHoYo.Yuanshen");
    let _ = sampler.join();
    // 层自报落盘
    let marker = su(
        serial,
        "cat /storage/emulated/0/Android/data/com.miHoYo.Yuanshen/files/knobs_layer_out.json",
        30,
    );
    std::fs::write(outdir.join("knobs_layer_out.json"), marker.trim()).ok();
    eprintln!(
        "  层自报: {}",
        crate::graylayer::GrayLayer::adb_stdout(&marker).chars().take(320).collect::<String>()
    );
    // 轮次有效性自检: 原神每帧发 2 次 cmdbatch, spf 必须是 2; 帧时按游戏内帧率
    // 设置分两档判 (60 帧档 ~16.7ms, 30 帧档 ~33.3ms)
    if let Ok(s) = std::fs::read_to_string(outdir.join("summary.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            let spf = v["submits_per_frame"].as_i64().unwrap_or(0);
            let p50 = v["frame_p50"].as_f64().unwrap_or(0.0);
            let ok = spf == 2 && ((15.5..=18.0).contains(&p50) || (31.0..=36.0).contains(&p50));
            eprintln!(
                "  自检: spf={spf} p50={p50}ms fps={} -> {}",
                v["fps_mean"],
                if ok { "有效" } else { "可疑, 该轮建议丢弃" }
            );
        }
    }
    Ok(())
}

fn spawn_perf(pf_bin: &str, serial: &str, out: &Path) {
    let f = std::fs::File::create(out).ok();
    let _ = std::process::Command::new(pf_bin)
        .args([
            "perf",
            "--serial",
            serial,
            "--app",
            "com.miHoYo.Yuanshen",
            "--rounds",
            "8",
            "--power-rail",
            "battery",
            "--suspend-charging",
            "--json",
        ])
        .stdout(f.map(|f| f.into()).unwrap_or(std::process::Stdio::null()))
        .stderr(std::process::Stdio::null())
        .status();
}

/// 无层臂: 不推 .so 干净基线。进世界判定用截帧亮度差 (与"门"页比, >40% 判离开)。
fn arm_none(serial: &str, label: &str, outdir: &Path, workload: &Path, pf_bin: &str) -> Result<(), String> {
    let _ = std::fs::create_dir_all(outdir);
    eprintln!("── 臂 {label} (无层) ──");
    let _ = gl_off_for(serial);
    let _ = adb(serial, &["shell", "am force-stop com.miHoYo.Yuanshen"], 30);
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

fn gl_off_for(serial: &str) -> String {
    crate::graylayer::GrayLayer::new(serial).off()
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
fn screencap_raw_bin(serial: &str) -> Vec<u8> {
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
    let _ = gl.off();
    let _ = adb(serial, &["shell", "am force-stop com.miHoYo.Yuanshen"], 30);
    let _ = su(
        serial,
        "rm -f /storage/emulated/0/Android/data/com.miHoYo.Yuanshen/files/knobs_layer_out.json",
        15,
    );
    gl.mount_mode("copyprobe", "com.miHoYo.Yuanshen", false)?;
    launch_to_world_layered(serial)?;
    let _ = run_one(serial, label, outdir, 30, workload, "com.miHoYo.Yuanshen");
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
    let _ = run_one(serial, label, outdir, 30, workload, "com.miHoYo.Yuanshen");
    std::thread::sleep(std::time::Duration::from_secs(3));
    for k in 1..=3 {
        let f = outdir.join(format!("shot{k}.raw"));
        std::fs::write(&f, screencap_raw_bin(serial)).ok();
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    let _ = pf_bin;
    Ok(())
}

pub fn run_gray_ab(args: &[String]) -> i32 {
    let mut pairs = 5u32;
    let mut mode = "loadop".to_string();
    let mut serial = std::env::var("SERIAL").unwrap_or_else(|_| "91253241019A".into());
    let mut out = PathBuf::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pairs" => pairs = it.next().and_then(|v| v.parse().ok()).unwrap_or(5),
            "--mode" => mode = it.next().cloned().unwrap_or_else(|| "loadop".into()),
            "--serial" => serial = it.next().cloned().unwrap_or(serial),
            "--out" => out = it.next().map(PathBuf::from).unwrap_or_default(),
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
    let _ = std::fs::create_dir_all(&out);
    if mode == "loadop" {
        for i in 1..=pairs {
            if let Err(e) = arm(&serial, &gl, &format!("ctrl{i}"), false, &out.join(format!("ctrl{i}")), &workload, 30, &pf_bin) {
                eprintln!("ctrl{i} 作废: {e}");
            }
            if let Err(e) = arm(&serial, &gl, &format!("knob{i}"), true, &out.join(format!("knob{i}")), &workload, 30, &pf_bin) {
                eprintln!("knob{i} 作废: {e}");
            }
        }
    } else {
        // copyprobe 验收: none1 none2 (无层控制对) + probe1..N (探针)。
        // 验收 ② 画面逐像素的比对交给 `phonefarm shot-diff`。
        let _ = gl.off();
        if let Err(e) = arm_none(&serial, "none1", &out.join("none1"), &workload, &pf_bin) {
            eprintln!("none1 作废: {e}");
        }
        if let Err(e) = arm_none(&serial, "none2", &out.join("none2"), &workload, &pf_bin) {
            eprintln!("none2 作废: {e}");
        }
        for i in 1..=probes {
            let label = format!("probe{i}");
            if let Err(e) = arm_copyprobe(&serial, &gl, &label, &out.join(&label), &workload, &pf_bin) {
                eprintln!("{label} 作废: {e}");
            }
        }
        let _ = gl.off();
        let _ = adb(&serial, &["shell", "am force-stop com.miHoYo.Yuanshen"], 30);
        if let Ok(t) = shotdiff::report(&out) {
            println!("{t}");
        }
    }
    let _ = gl.off();
    println!("全部完成, 证据: {}", out.display());
    0
}
