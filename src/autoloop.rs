//! 系统参数全自动闭环的编排 (原神实测)。
//!
//! 由 `loop_v1/auto/autoloop.py` 逐段搬过来, 产物 (probe.txt / whitelist.json /
//! rule.json / candidates.json / result.json / metrics.json / report.json) 的形状与
//! 落盘口径保持一致。判定口径 (白名单 / 规则 / 命题 / 命中) 全部复用
//! `src/sysparam.rs` 与 `src/llm.rs` —— 口径只有一份, 编排这里不重复实现。
//!
//! 流程
//! ----
//! 推送脚本 → 进入前快照 → 真机探白名单 (探测零残留核对) → 负载自检 (视角在转)
//! → 基线 (冻结温度上限/等冷目标/功耗可用性) → **在看到候选数据之前**冻结判定规则
//! → 逐代: 提名 (大模型, 挂了退回本地变异器) → 校验 → ABBA 真机评测 → 判定
//! → 收尾: 强制还原 + 等冷 + 全局快照按「我们写过什么」判留痕。

use crate::eval::{adb, adb_ok, push_tools, run_one, snapshot, su, DEV_TMP};
use crate::framecheck;
use crate::llm::{self, Candidate};
use crate::loopreport::snapshot_diff;
use crate::loopstat::{self, Run};
use crate::pyjson::{dumps_indent, dumps_indent_sorted, py_round, PyVal};
use crate::sysparam::{self, DecideCtx};
use crate::pyobj;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 等冷门槛的下界。真正的门槛在基线之后定: 目标是「回到基线是在什么热态下量的」。
const COOL_C_FLOOR: f64 = 40.0;
const COOL_TIMEOUT_S: u64 = 420;
/// 温度上限是**安全上限** (「别把机器烤坏」), 不是「保证两臂热态一样」——
/// 热态可比性由组内 ABBA + 等冷目标负责。骁龙结温保护 ~95C, 70C 留足余量;
/// +10C 失控护栏只在真热失控时触发 (正常漂移不会)。
const TEMP_CAP_FLOOR_C: f64 = 70.0;
const TEMP_CAP_MARGIN_C: f64 = 10.0;

pub struct Args {
    pub out: PathBuf,
    pub generations: u32,
    pub children: u32,
    pub pairs: u32,
    pub no_llm: bool,
    pub probe_only: bool,
    pub baseline_runs: u32,
    pub power_in_verdict: String,
    pub serial: String,
}

const USAGE: &str = "用法: phonefarm autoloop --out <证据目录> [--generations N] [--children N] [--pairs N]\n\
                     [--no-llm] [--probe-only] [--baseline-runs N] [--power-in-verdict auto|on|off]\n\
                     [--serial S]  (SERIAL 环境变量亦可)";

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut it = args.iter();
    let (mut out, mut generations, mut children, mut pairs) =
        (None, 2u32, 3u32, 5u32);
    let (mut no_llm, mut probe_only) = (false, false);
    let mut baseline_runs = 2u32;
    let mut power_in_verdict = "auto".to_string();
    let mut serial = std::env::var("SERIAL").unwrap_or_else(|_| "91253241019A".into());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--generations" => generations = it.next().and_then(|v| v.parse().ok()).unwrap_or(2),
            "--children" => children = it.next().and_then(|v| v.parse().ok()).unwrap_or(3),
            "--pairs" => pairs = it.next().and_then(|v| v.parse().ok()).unwrap_or(5),
            "--no-llm" => no_llm = true,
            "--probe-only" => probe_only = true,
            "--baseline-runs" => baseline_runs = it.next().and_then(|v| v.parse().ok()).unwrap_or(2),
            "--power-in-verdict" => {
                power_in_verdict = it.next().cloned().unwrap_or_else(|| "auto".into())
            }
            "--serial" => serial = it.next().cloned().unwrap_or(serial),
            other => return Err(format!("不认识的参数 {other}\n{USAGE}")),
        }
    }
    if !matches!(power_in_verdict.as_str(), "auto" | "on" | "off") {
        return Err(format!("--power-in-verdict 只认 auto|on|off, got {power_in_verdict}"));
    }
    Ok(Args {
        out: out.ok_or(format!("缺少 --out\n\n{USAGE}"))?,
        generations,
        children,
        pairs,
        no_llm,
        probe_only,
        baseline_runs,
        power_in_verdict,
        serial,
    })
}

fn log(msg: &str) {
    println!("[autoloop] {msg}");
}

// ══════════════ 温度 / 等冷 ══════════════

/// 最热的 SoC 结温 (只看 cpu-/cpullc/gpuss, 与 hwcond.rs::soc_max_c 同口径)。
fn read_soc_temp(serial: &str) -> Option<(String, f64)> {
    let txt = adb(
        serial,
        &[
            "shell",
            "for z in /sys/class/thermal/thermal_zone*; do echo \"$(cat $z/type) $(cat $z/temp)\"; done 2>/dev/null",
        ],
        30,
    );
    let mut best: Option<(String, i64)> = None;
    for line in txt.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let ty = parts[0];
        if !(ty.starts_with("cpu-") || ty.starts_with("cpullc") || ty.starts_with("gpuss")) {
            continue;
        }
        let Ok(t) = parts[1].parse::<i64>() else { continue };
        if !(0 < t && t < 100_000) {
            continue;
        }
        if best.as_ref().is_none_or(|(_, bt)| t > *bt) {
            best = Some((ty.to_string(), t));
        }
    }
    best.map(|(ty, t)| (ty, t as f64 / 1000.0))
}

fn wait_cool(serial: &str, cool_c: f64, timeout_s: u64) -> PyVal {
    let t0 = std::time::Instant::now();
    loop {
        let Some(cur) = read_soc_temp(serial) else {
            return pyobj! { "ok" => false, "reason" => "读不到任何 SoC 热区" };
        };
        if cur.1 < cool_c {
            return pyobj! {
                "ok" => true, "zone" => cur.0, "c" => cur.1,
                "waited_s" => py_round(t0.elapsed().as_secs_f64(), 1),
            };
        }
        if t0.elapsed().as_secs() > timeout_s {
            return pyobj! {
                "ok" => false,
                "reason" => format!("等冷超时: {}={}C 仍 >= {}C", cur.0, cur.1, cool_c),
                "zone" => cur.0, "c" => cur.1,
            };
        }
        log(&format!(
            "等冷: {}={}C >= {}C, 8s 后重测 (已等 {}s)",
            cur.0, cur.1, cool_c, t0.elapsed().as_secs()
        ));
        std::thread::sleep(std::time::Duration::from_secs(8));
    }
}

// ══════════════ 快照 diff 分类 ══════════════

/// 快照 key ←→ 设备路径 (device_snapshot.sh 的命名与 sysfs 叶子名不一样)。
pub fn snapshot_key_of(kind: &str, path: &str) -> Option<String> {
    if kind == "setting" {
        return Some(format!("settings.{}", path.split_once(':').map(|x| x.1).unwrap_or(path)));
    }
    let re = |pat: &str| Regex::new(pat).ok();
    if let Some(m) = re(r"^/sys/devices/system/cpu/cpufreq/(policy\d+)/scaling_(min|max)_freq$")
        .and_then(|r| r.captures(path))
    {
        // device_snapshot.sh 的命名是 scaling_max / scaling_min (没有 _freq 后缀)
        return Some(format!("cpu.{}.scaling_{}", &m[1], &m[2]));
    }
    if let Some(m) = re(r"^/sys/devices/system/cpu/cpufreq/(policy\d+)/scaling_governor$")
        .and_then(|r| r.captures(path))
    {
        return Some(format!("cpu.{}.governor", &m[1]));
    }
    if let Some(m) = re(r"^/sys/class/kgsl/kgsl-3d0/(min|max)_pwrlevel$")
        .and_then(|r| r.captures(path))
    {
        return Some(format!("kgsl.{}_pwrlevel", &m[1]));
    }
    if let Some(m) = re(r"^/sys/devices/system/cpu/bus_dcvs/(\w+)/boost_freq$")
        .and_then(|r| r.captures(path))
    {
        return Some(format!("bus.{}.boost_freq", &m[1]));
    }
    None
}

use regex::Regex;

/// 这一组候选真正写过的快照 key (含回滚波及到的兄弟节点)。
pub fn touched_keys(cand: &[(String, String)], wl: &sysparam::Whitelist) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (pid, _) in cand {
        let Some(spec) = sysparam::wl_get(wl, pid) else { continue };
        let mut paths = vec![spec.path.clone()];
        // 与 knob_sysparam.sh 的 siblings() 同一套波及面
        if spec.path.contains("/cpufreq/policy") {
            if let Some(d) = Path::new(&spec.path).parent() {
                let d = d.display().to_string();
                paths = vec![
                    format!("{d}/scaling_governor"),
                    format!("{d}/scaling_max_freq"),
                    format!("{d}/scaling_min_freq"),
                ];
            }
        } else if spec.path.ends_with("min_pwrlevel") || spec.path.ends_with("max_pwrlevel") {
            paths = vec![
                "/sys/class/kgsl/kgsl-3d0/min_pwrlevel".into(),
                "/sys/class/kgsl/kgsl-3d0/max_pwrlevel".into(),
            ];
        }
        for pth in paths {
            if let Some(k) = snapshot_key_of(&spec.kind, &pth) {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
    }
    out
}

/// 把快照 diff 拆成「我们留的痕」与「设备自己动的」两堆。
///
/// 判据是**这一组候选到底写过哪些项**, 不是一张写死的 key 名单: 实测红魔的厂商
/// 温控/性能管家会在游戏过程中主动改 cpu.policyN.scaling_max 与 kgsl.max_pwrlevel。
/// 拿写死名单豁免这些 key 等于给自己开后门; 按「我们写没写过」分类才站得住。
pub fn classify_diff(sd: &PyVal, ours_keys: Option<&[String]>) -> PyVal {
    let (mut ours, mut env) = (Vec::new(), Vec::new());
    if let Some(PyVal::List(diffs)) = sd.get("diffs") {
        for d in diffs {
            let key = d
                .get("before")
                .map(|v| v.py_str())
                .unwrap_or_default()
                .split_once('=')
                .map(|(k, _)| k.trim().to_string())
                .unwrap_or_default();
            let is_ours = match ours_keys {
                None => true,
                Some(keys) => keys.contains(&key),
            };
            if is_ours {
                ours.push(d.clone());
            } else {
                env.push(d.clone());
            }
        }
    }
    let strict_identical = matches!(sd.get("identical"), Some(PyVal::Bool(true)));
    pyobj! {
        "checked" => sd.get("checked").cloned().unwrap_or(PyVal::Null),
        "n_lines" => sd.get("n_lines").cloned().unwrap_or(PyVal::Null),
        "strict_identical" => strict_identical,
        "strict_n_diff" => sd.get("n_diff").cloned().unwrap_or(PyVal::Null),
        "ours_identical" => ours.is_empty(),
        "ours_keys" => match ours_keys {
            Some(k) => { let mut v = k.to_vec(); v.sort(); PyVal::List(v.into_iter().map(PyVal::Str).collect()) }
            None => PyVal::Null,
        },
        "ours_diffs" => PyVal::List(ours),
        "environment_diffs" => PyVal::List(env),
    }
}

// ══════════════ 负载自检 ══════════════

fn screencap_raw(serial: &str) -> Vec<u8> {
    let out = Command::new(crate::eval::adb_path())
        .args(["-s", serial, "exec-out", "screencap"])
        .output();
    out.map(|o| o.stdout).unwrap_or_default()
}

use std::process::Command;

/// 跑一次拖拽, 中途抓两帧, 确认画面真的在转 (判据在 src/framecheck.rs)。
fn check_spin(serial: &str, outdir: &Path) -> PyVal {
    let _ = std::fs::create_dir_all(outdir);
    let mut drag = Command::new(crate::eval::adb_path())
        .args(["-s", serial, "shell", "input swipe 900 400 2000 400 12000"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok();
    std::thread::sleep(std::time::Duration::from_secs(3));
    let a = screencap_raw(serial);
    std::thread::sleep(std::time::Duration::from_secs(5));
    let b = screencap_raw(serial);
    if let Some(mut d) = drag.take() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while matches!(d.try_wait(), Ok(None)) {
            if std::time::Instant::now() > deadline {
                let _ = d.kill();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    let mut paths = Vec::new();
    for (name, buf) in [("spin_a.raw", a), ("spin_b.raw", b)] {
        let p = outdir.join(name);
        std::fs::write(&p, &buf).ok();
        paths.push(p);
    }
    // 判据在 src/framecheck.rs; 裸帧走文件路径不走 JSON —— 一张 13MB, 过桥不划算
    let va = std::fs::read(&paths[0]).unwrap_or_default();
    let vb = std::fs::read(&paths[1]).unwrap_or_default();
    let v = framecheck::verdict(&va, &vb);
    std::fs::write(outdir.join("spin_check.json"), dumps_indent(&v, 1)).ok();
    v
}

// ══════════════ 停充 ══════════════

fn charge_suspend(serial: &str) -> String {
    let out = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh suspend"), 90);
    log(&format!(
        "停充: {}",
        out.trim().lines().rev().take(2).collect::<Vec<_>>().join(" | ")
    ));
    out
}

fn charge_restore(serial: &str) -> String {
    let out = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh restore"), 60);
    if out.contains("CHARGE_RESTORE_FAIL") {
        log(&format!("警告: 充电未能恢复, 保留 state 文件以便人工回滚:\n{out}"));
    }
    out
}

// ══════════════ 单轮指标 (与 autoloop.run_one 同形: summary 与 env 展平合并) ══════════════

const ENV_KEYS: [&str; 16] = [
    "power_w_mean",
    "power_w_median",
    "power_now_w_mean",
    "power_now_plausible",
    "power_vi_w_mean",
    "usb_input_w_mean",
    "current_now_ua_mean",
    "voltage_now_uv_mean",
    "power_rail",
    "battery_status",
    "battery_charging",
    "power_usable_reason",
    "soc_temp_max_c",
    "soc_temp_mean_c",
    "n_samples",
    "fan_state",
];

fn metrics_json(m: &Value, outdir: &Path) -> PyVal {
    let mut obj: Vec<(String, PyVal)> = Vec::new();
    obj.push(("label".into(), PyVal::Str(m["label"].as_str().unwrap_or("").into())));
    obj.push(("run_once_rc".into(), PyVal::Int(m["run_once_rc"].as_i64().unwrap_or(0))));
    if let Some(s) = m["summary"].as_object() {
        for (k, v) in s {
            obj.push((k.clone(), to_pyval(v)));
        }
    }
    if let Some(e) = m["env"].as_object() {
        for k in ENV_KEYS {
            if let Some(v) = e.get(k) {
                obj.push((k.to_string(), to_pyval(v)));
            }
        }
    }
    let v = PyVal::Obj(obj);
    // autoloop: json.dump(m, indent=1, sort_keys=True)
    std::fs::write(outdir.join("metrics.json"), dumps_indent_sorted(&v, 1)).ok();
    v
}

fn to_pyval(v: &Value) -> PyVal {
    serde_pyval::from_serde(v)
}

/// serde_json 值 → PyVal (这一侧桥接, 保留 int/float 之分)。
mod serde_pyval {
    use crate::pyjson::PyVal;
    use serde_json::Value;
    pub fn from_serde(v: &Value) -> PyVal {
        match v {
            Value::Null => PyVal::Null,
            Value::Bool(b) => PyVal::Bool(*b),
            Value::Number(n) => match n.as_i64() {
                Some(i) => PyVal::Int(i),
                None => PyVal::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            Value::String(s) => PyVal::Str(s.clone()),
            Value::Array(a) => PyVal::List(a.iter().map(from_serde).collect()),
            Value::Object(o) => PyVal::Obj(o.iter().map(|(k, x)| (k.clone(), from_serde(x))).collect()),
        }
    }
}

fn runs_as_pyval(runs: &[(String, Value)]) -> Vec<Run> {
    runs.iter()
        .map(|(l, m)| (l.clone(), metrics_json_view(m)))
        .collect()
}

/// 判定/统计用的视图: summary 字段展平 (与 Python 侧 dict 合并后同形)。
fn metrics_json_view(m: &Value) -> PyVal {
    let mut obj: Vec<(String, PyVal)> = Vec::new();
    if let Some(s) = m["summary"].as_object() {
        for (k, v) in s {
            obj.push((k.clone(), to_pyval(v)));
        }
    }
    if let Some(e) = m["env"].as_object() {
        for k in ENV_KEYS {
            if let Some(v) = e.get(k) {
                obj.push((k.to_string(), to_pyval(v)));
            }
        }
    }
    PyVal::Obj(obj)
}

// ══════════════ 候选评测 (ABBA) ══════════════

/// run_candidate 的入参束 (闭包/函数都逃不开 clippy 的复杂度阈值, 显式命名)
struct CandidateRun<'a> {
    serial: &'a str,
    cand: &'a Candidate,
    wl: &'a sysparam::Whitelist,
    outdir: &'a Path,
    pairs: u32,
    temp_cap_c: f64,
    power_available: bool,
    metrics: &'a [String],
    cool_c: f64,
    workload: &'a Path,
}

fn run_candidate(r: &CandidateRun) -> PyVal {
    let (serial, cand, wl, outdir, pairs, temp_cap_c, power_available, metrics, cool_c, workload) =
        (r.serial, r.cand, r.wl, r.outdir, r.pairs, r.temp_cap_c, r.power_available, r.metrics, r.cool_c, r.workload);
    let params = &cand.params;
    let plan = sysparam::plan_text(params, wl).unwrap_or_default();
    std::fs::write(outdir.join("plan.txt"), &plan).ok();
    // push 失败必须当场停: 设备上可能还躺着上一组的 plan, 旋钮臂施加的就是上一组
    let push_out = adb(
        serial,
        &["push", &outdir.join("plan.txt").to_string_lossy(), &format!("{DEV_TMP}/loop_v1_sysparam.plan")],
        60,
    );
    if push_out.contains("#adb_exit=") && !push_out.ends_with("#adb_exit=0") {
        return abort_result(params, cand, &format!("plan 下发失败: {push_out}"), temp_cap_c, outdir);
    }

    let snap_before = snapshot(serial);
    std::fs::write(outdir.join("snap_before.txt"), &snap_before).ok();

    // 等冷超时不作废本组: 组内 ABBA 已让两臂承受同样的残余热漂移, 只如实记录
    let cool = wait_cool(serial, cool_c, COOL_TIMEOUT_S);
    if matches!(cool.get("ok"), Some(PyVal::Bool(false))) {
        log(&format!(
            "等冷未达标 ({}), 仍按 ABBA 交替继续, 已记进证据",
            cool.get("reason").map(|v| v.py_str()).unwrap_or_default()
        ));
    }

    let charge_log = charge_suspend(serial);

    // 每轮: (标签, run_one 的合并指标)
    type ArmRuns = Vec<(String, Value)>;
    let (mut knob_runs, mut ctrl_runs): (ArmRuns, ArmRuns) = (Vec::new(), Vec::new());
    let mut apply_logs: Vec<String> = Vec::new();
    let mut apply_ok = true;
    let mut aborted: Option<String> = None;
    let mut restore_fail: Option<String> = None;

    const BAD_APPLY_TOKENS: [&str; 4] = [
        "KNOB_FAIL",
        "KNOB_REFUSE",
        "KNOB_PLAN_REJECTED",
        "KNOB_ALREADY_APPLIED",
    ];
    let restore = |s: &str| -> String { su(s, &format!("sh {DEV_TMP}/knob_sysparam.sh restore"), 120) };

    for i in 1..=pairs {
        // ABBA: 奇数对旋钮先跑, 偶数对对照先跑, 让残余漂移在两臂之间对消
        for arm in if i % 2 == 1 { ["knob", "ctrl"] } else { ["ctrl", "knob"] } {
            if arm == "knob" {
                let out = su(
                    serial,
                    &format!("sh {DEV_TMP}/knob_sysparam.sh apply {DEV_TMP}/loop_v1_sysparam.plan"),
                    120,
                );
                apply_logs.push(out.clone());
                if !adb_ok(&out) || BAD_APPLY_TOKENS.iter().any(|t| out.contains(t)) {
                    apply_ok = false;
                    log(&format!("旋钮未全部生效, 停止本组:\n{out}"));
                    let r = restore(serial);
                    if r.contains("KNOB_RESTORE_FAIL") && restore_fail.is_none() {
                        restore_fail = Some(r);
                    }
                    aborted = Some("旋钮未全部生效".into());
                    break;
                }
            }
            let label = format!("sp_{arm}{i}");
            let res = run_one(
                serial,
                &label,
                &outdir.join(format!("{arm}{i}")),
                30,
                workload,
                "com.miHoYo.Yuanshen",
            );
            if arm == "knob" {
                // 负载/采集失败也要先把旋钮摘掉, 绝不把设备留在施加态
                let r = restore(serial);
                if r.contains("KNOB_RESTORE_FAIL") && restore_fail.is_none() {
                    restore_fail = Some(r);
                }
            }
            let t = match &res {
                Ok(m) => m["summary"]["soc_temp_max_c"].as_f64(),
                Err(_) => None,
            };
            match res {
                Ok(m) => {
                    // 每轮 metrics.json (autoloop: sort_keys 落盘)
                    let _ = metrics_json(&m, &outdir.join(format!("{arm}{i}")));
                    (if arm == "knob" { &mut knob_runs } else { &mut ctrl_runs })
                        .push((label.clone(), m));
                }
                Err(e) => {
                    aborted = Some(e);
                }
            }
            if restore_fail.is_some() {
                aborted = Some("旋钮还原失败, 后续数据不可信".into());
            }
            if aborted.is_none() {
                if let Some(t) = t {
                    if t > temp_cap_c {
                        aborted = Some(format!("{arm} 臂第 {i} 轮 SoC 结温 {t}C 超过上限 {temp_cap_c}C"));
                    }
                }
            }
            if aborted.is_some() {
                break;
            }
        }
        if aborted.is_some() {
            break;
        }
    }

    // 无论如何先还原 (旋钮 + 充电), 再核快照
    let restore_log = restore(serial);
    if restore_log.contains("KNOB_RESTORE_FAIL") || restore_log.contains("#adb_exit=") && !restore_log.ends_with("#adb_exit=0") {
        if restore_fail.is_none() {
            restore_fail = Some(restore_log.clone());
        }
        log(&format!("旋钮还原失败:\n{restore_log}"));
    }
    let charge_restore_log = charge_restore(serial);
    let status_log = su(serial, &format!("sh {DEV_TMP}/knob_sysparam.sh status"), 60);
    let snap_after = snapshot(serial);
    std::fs::write(outdir.join("snap_after.txt"), &snap_after).ok();
    let sd = classify_diff(
        &snapshot_diff(
            &outdir.join("snap_before.txt").to_string_lossy(),
            &outdir.join("snap_after.txt").to_string_lossy(),
        ),
        Some(&touched_keys(params, wl)),
    );
    let ours_identical = matches!(sd.get("ours_identical"), Some(PyVal::Bool(true)));

    let all_runs: Vec<&(String, Value)> = knob_runs.iter().chain(ctrl_runs.iter()).collect();
    let temps: Vec<f64> = all_runs
        .iter()
        .filter_map(|(_, m)| m["summary"]["soc_temp_max_c"].as_f64())
        .collect();
    let temp_max = temps.iter().cloned().reduce(f64::max);

    // metrics 由调用方从 rule["metrics"] 传进来 —— 判定那一侧用的就是同一张表
    let mut comparisons: Vec<(String, PyVal)> = Vec::new();
    if knob_runs.len() >= 2 && ctrl_runs.len() >= 2 {
        let (a_runs, b_runs) = (runs_as_pyval(&ctrl_runs), runs_as_pyval(&knob_runs));
        for met in metrics {
            comparisons.push((met.clone(), loopstat::compare(&a_runs, &b_runs, met)));
        }
    }
    let comparisons_val = PyVal::Obj(comparisons.clone());

    if aborted.is_none() && restore_fail.is_some() {
        aborted = Some("收尾还原失败, 本组作废".into());
    }
    let dec = if let Some(reason) = aborted {
        pyobj! {
            "verdict" => "ABORT", "reason" => reason,
            "temp_max_c" => temp_max, "temp_cap_c" => temp_cap_c,
        }
    } else if knob_runs.len() < pairs as usize || ctrl_runs.len() < pairs as usize {
        pyobj! {
            "verdict" => "ABORT",
            "reason" => format!("轮数不足 (旋钮 {}/{}, 对照 {}/{})",
                                knob_runs.len(), pairs, ctrl_runs.len(), pairs),
            "temp_max_c" => temp_max, "temp_cap_c" => temp_cap_c,
        }
    } else {
        let cmps_val = PyVal::Obj(comparisons.clone());
        sysparam::decide(
            &cmps_val,
            &DecideCtx {
                temp_max_c: temp_max.map(PyVal::Float).unwrap_or(PyVal::Null),
                temp_cap_c: PyVal::Float(temp_cap_c),
                apply_ok,
                snapshot_identical: ours_identical,
                power_available,
            },
        )
    };

    let describe_arm = |runs: &[(String, Value)]| -> PyVal {
        let runs = runs_as_pyval(runs);
        if runs.is_empty() {
            PyVal::Null
        } else {
            loopstat::describe(&runs, "arm")
        }
    };
    let env_pick = |m: &Value| -> PyVal {
        let mut kvs: Vec<(String, PyVal)> = Vec::new();
        for k in ENV_KEYS {
            if let Some(v) = m["env"].get(k) {
                kvs.push((k.to_string(), to_pyval(v)));
            }
        }
        PyVal::Obj(kvs)
    };
    let fan_states: Vec<String> = all_runs
        .iter()
        .filter_map(|(_, m)| m["env"]["fan_state"].as_str().map(String::from))
        .collect();
    let on_battery_set: Vec<bool> = all_runs
        .iter()
        .filter_map(|(_, m)| m["env"]["on_battery"].as_bool())
        .collect();

    let mut result: Vec<(String, PyVal)> = vec![
        ("params".into(), params_pyval(params)),
        ("why".into(), PyVal::Str(cand.why.clone())),
        ("plan".into(), PyVal::Str(plan)),
        ("pairs_completed".into(), PyVal::Int(knob_runs.len().min(ctrl_runs.len()) as i64)),
        ("apply_ok".into(), PyVal::Bool(apply_ok)),
        (
            "apply_log".into(),
            PyVal::List(apply_logs.iter().take(2).map(|l| PyVal::Str(l.clone())).collect()),
        ),
        ("restore_ok".into(), PyVal::Bool(restore_fail.is_none())),
        ("restore_fail".into(), restore_fail.clone().map(PyVal::Str).unwrap_or(PyVal::Null)),
        (
            "restore_log".into(),
            PyVal::List(restore_log.trim().lines().map(|l| PyVal::Str(l.into())).collect()),
        ),
        (
            "knob_state_after".into(),
            PyVal::List(status_log.trim().lines().map(|l| PyVal::Str(l.into())).collect()),
        ),
        ("cooldown".into(), cool),
        (
            "charge_suspend_log".into(),
            PyVal::List(charge_log.trim().lines().map(|l| PyVal::Str(l.into())).collect()),
        ),
        (
            "charge_restore_log".into(),
            PyVal::List(charge_restore_log.trim().lines().map(|l| PyVal::Str(l.into())).collect()),
        ),
        (
            "on_battery_uniform".into(),
            PyVal::Bool({
                let mut v = on_battery_set.clone();
                v.sort();
                v.dedup();
                v.len() <= 1
            }),
        ),
        ("snapshot_check".into(), sd),
        ("temp_max_c".into(), temp_max.map(PyVal::Float).unwrap_or(PyVal::Null)),
        ("temp_cap_c".into(), PyVal::Float(temp_cap_c)),
        (
            "arms".into(),
            pyobj! {
                "ctrl" => describe_arm(&ctrl_runs),
                "knob" => describe_arm(&knob_runs),
            },
        ),
        (
            "env_per_run".into(),
            PyVal::Obj(
                all_runs
                    .iter()
                    .map(|(name, m)| (name.clone(), env_pick(m)))
                    .collect(),
            ),
        ),
        (
            "fan_state_uniform".into(),
            PyVal::Bool({
                let mut v = fan_states.clone();
                v.sort();
                v.dedup();
                v.len() <= 1
            }),
        ),
        ("comparisons".into(), comparisons_val),
    ];
    // **dec 的键按它自己的顺序接在后面
    if let PyVal::Obj(kvs) = dec {
        result.extend(kvs);
    }
    let result = PyVal::Obj(result);
    std::fs::write(outdir.join("result.json"), dumps_indent(&result, 1)).ok();
    result
}

fn params_pyval(params: &[(String, String)]) -> PyVal {
    PyVal::Obj(params.iter().map(|(k, v)| (k.clone(), PyVal::Str(v.clone()))).collect())
}

fn abort_result(
    params: &[(String, String)],
    cand: &Candidate,
    reason: &str,
    cap: f64,
    outdir: &Path,
) -> PyVal {
    let r = pyobj! {
        "params" => params_pyval(params),
        "why" => PyVal::Str(cand.why.clone()),
        "verdict" => "ABORT",
        "reason" => reason,
        "temp_cap_c" => cap,
    };
    std::fs::write(outdir.join("result.json"), dumps_indent(&r, 1)).ok();
    r
}

// ══════════════ 主流程 ══════════════

pub fn run_autoloop(args: &[String]) -> i32 {
    let a = match parse_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let out = if a.out.is_absolute() {
        a.out.clone()
    } else {
        std::env::current_dir().unwrap_or_default().join(&a.out)
    };
    let _ = std::fs::create_dir_all(&out);
    log(&format!("证据目录 {}", out.display()));
    let workload = match crate::eval::resolve_path("loop_v1/scripts/workload_spin_touch_v1.json") {
        Some(p) => p,
        None => {
            log("找不到负载脚本 loop_v1/scripts/workload_spin_touch_v1.json");
            return 2;
        }
    };
    let serial = a.serial.clone();

    if adb(&serial, &["get-state"], 15).trim() != "device" {
        log("设备不在线");
        return 2;
    }
    if let Err(e) = push_tools(&serial) {
        log(&e);
        return 2;
    }

    // 1) 进入前快照 —— 判据 4 的基准
    let snap_before = snapshot(&serial);
    std::fs::write(out.join("snap_before.txt"), &snap_before).ok();

    // 2) 真机探白名单
    log("探测可写系统参数 (含生效测试, 每项立刻还原) ...");
    let probe_txt = su(&serial, &format!("sh {DEV_TMP}/probe_sysparam.sh"), 600);
    std::fs::write(out.join("probe.txt"), &probe_txt).ok();
    let wl = sysparam::build_whitelist(&sysparam::parse_probe(&probe_txt));
    std::fs::write(
        out.join("whitelist.json"),
        dumps_indent_sorted(&sysparam::whitelist_to_pyval(&wl), 1),
    )
    .ok();
    let ids: Vec<&str> = wl.iter().map(|(k, _)| k.as_str()).collect();
    log(&format!("白名单 {} 项: {}", wl.len(), ids.join(", ")));
    if wl.is_empty() {
        log("白名单为空, 没有任何参数通过「存在+可写+真生效」三关");
        return 1;
    }

    // 探测本身也要不留痕
    let snap_after_probe = snapshot(&serial);
    std::fs::write(out.join("snap_after_probe.txt"), &snap_after_probe).ok();
    let probe_sd = classify_diff(
        &snapshot_diff(
            &out.join("snap_before.txt").to_string_lossy(),
            &out.join("snap_after_probe.txt").to_string_lossy(),
        ),
        None,
    );
    log(&format!(
        "探测后快照一致 (我们写过的项): {}; 严格逐行一致: {} (设备自己动的 {} 行)",
        probe_sd.get("ours_identical").map(|v| v.py_str()).unwrap_or_default(),
        probe_sd.get("strict_identical").map(|v| v.py_str()).unwrap_or_default(),
        match probe_sd.get("environment_diffs") {
            Some(PyVal::List(l)) => l.len(),
            _ => 0,
        }
    ));

    if a.probe_only {
        let report = pyobj! {
            "whitelist" => sysparam::whitelist_to_pyval(&wl),
            "probe_residue" => probe_sd,
        };
        std::fs::write(out.join("report.json"), dumps_indent(&report, 1)).ok();
        return 0;
    }

    // 3) 负载自检: 视角真的在转才继续
    let spin = check_spin(&serial, &out.join("spin_check"));
    log(&format!(
        "负载自检: 画面变化 {} (门槛 {}) → {}",
        spin.get("moved_fraction").map(|v| v.py_str()).unwrap_or_default(),
        spin.get("gate").map(|v| v.py_str()).unwrap_or_default(),
        if matches!(spin.get("spinning"), Some(PyVal::Bool(true))) { "转起来了" } else { "没在转" }
    ));
    if !matches!(spin.get("spinning"), Some(PyVal::Bool(true))) {
        log(&format!(
            "负载没生效, 不采任何数据: {}",
            spin.get("note")
                .or(spin.get("error"))
                .map(|v| v.py_str())
                .unwrap_or_default()
        ));
        let report = pyobj! {
            "whitelist" => sysparam::whitelist_to_pyval(&wl),
            "probe_residue" => probe_sd,
            "spin_check" => spin,
            "aborted" => "负载自检未通过: 视角没在转, 采到的会是静止画面",
        };
        std::fs::write(out.join("report.json"), dumps_indent(&report, 1)).ok();
        return 3;
    }

    // 4) 基线: 量基准温度与功耗可用性, 用来冻结温度上限
    log(&format!(
        "跑 {} 轮基线 (不加任何参数), 用于冻结温度上限与确认功耗可测 ...",
        a.baseline_runs
    ));
    let cool = wait_cool(&serial, COOL_C_FLOOR, COOL_TIMEOUT_S);
    log(&format!("基线前等冷: {}", dumps_indent(&cool, 0).replace('\n', " ")));
    let base_start_c = cool.get("c").and_then(|v| v.as_f64());
    let base_charge_log = charge_suspend(&serial);
    let mut base_runs: Vec<(String, Value)> = Vec::new();
    for i in 1..=a.baseline_runs {
        let label = format!("sp_base{i}");
        let dir = out.join("baseline").join(format!("base{i}"));
        match run_one(&serial, &label, &dir, 30, &workload, "com.miHoYo.Yuanshen") {
            Ok(m) => {
                log(&format!(
                    "  base{i}: p95={}ms fps={} power={}W temp={}C",
                    m["summary"]["frame_p95"],
                    m["summary"]["fps_mean"],
                    m["env"]["power_w_mean"],
                    m["summary"]["soc_temp_max_c"]
                ));
                base_runs.push((label, m));
            }
            Err(e) => log(&format!("  base{i}: {e}")),
        }
    }
    let base_charge_restore_log = charge_restore(&serial);
    let base_temps: Vec<f64> = base_runs
        .iter()
        .filter_map(|(_, m)| m["summary"]["soc_temp_max_c"].as_f64())
        .collect();
    let temp_cap = if base_temps.is_empty() {
        TEMP_CAP_FLOOR_C
    } else {
        TEMP_CAP_FLOOR_C.max(py_round(
            base_temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max) + TEMP_CAP_MARGIN_C,
            1,
        ))
    };
    let power_measurable = base_runs
        .iter()
        .any(|(_, m)| m["env"]["power_w_mean"].as_f64().is_some());
    let mut power_reasons: Vec<String> = base_runs
        .iter()
        .filter_map(|(_, m)| m["env"]["power_usable_reason"].as_str().map(String::from))
        .collect();
    power_reasons.sort();
    power_reasons.dedup();
    let on_battery_all = !base_runs.is_empty()
        && base_runs.iter().all(|(_, m)| m["env"]["on_battery"].as_bool() == Some(true));
    let power_available = match a.power_in_verdict.as_str() {
        "on" => true,
        "off" => false,
        _ => power_measurable && on_battery_all,
    };
    let first_charge_line = base_charge_log.trim().lines().next().unwrap_or("?").to_string();
    let power_note = if power_available {
        format!("功耗计入判定 (基线轮全程放电态; 停充: {first_charge_line})")
    } else {
        format!(
            "功耗**不计入判定**, 仅记录供参考。{}",
            power_reasons.iter().filter(|r| !r.is_empty()).cloned().collect::<Vec<_>>().join(" / ")
        )
    };
    // 每组候选的等冷目标 = 「回到基线是在什么热态下量的」, 也在看候选数据前冻结
    let cool_target = (COOL_C_FLOOR.max(py_round(base_start_c.unwrap_or(COOL_C_FLOOR) + 1.0, 1)))
        .min(temp_cap - 2.0);

    // 5) **在看到任何候选数据之前**冻结判定规则
    let mut rule = match sysparam::rule_doc(
        &PyVal::Float(temp_cap),
        &PyVal::Int(a.pairs as i64),
        power_available,
        &power_note,
    ) {
        PyVal::Obj(kvs) => kvs,
        _ => Vec::new(),
    };
    rule.push(("power_measurable".into(), PyVal::Bool(power_measurable)));
    rule.push(("power_in_verdict_mode".into(), PyVal::Str(a.power_in_verdict.clone())));
    rule.push(("baseline_on_battery".into(), PyVal::Bool(on_battery_all)));
    rule.push((
        "charge_suspend_log".into(),
        PyVal::List(base_charge_log.trim().lines().map(|l| PyVal::Str(l.into())).collect()),
    ));
    rule.push((
        "charge_restore_log".into(),
        PyVal::List(base_charge_restore_log.trim().lines().map(|l| PyVal::Str(l.into())).collect()),
    ));
    rule.push((
        "power_reasons".into(),
        PyVal::List(power_reasons.iter().filter(|r| !r.is_empty()).map(|r| PyVal::Str(r.clone())).collect()),
    ));
    rule.push((
        "temp_cap_derivation".into(),
        pyobj! {
            "floor_c" => TEMP_CAP_FLOOR_C, "margin_c" => TEMP_CAP_MARGIN_C,
            "baseline_max_c" => base_temps.iter().cloned().reduce(f64::max),
            "formula" => "max(floor, baseline_max + margin)",
        },
    ));
    rule.push(("cooldown_target_c".into(), PyVal::Float(cool_target)));
    rule.push((
        "cooldown_derivation".into(),
        pyobj! {
            "floor_c" => COOL_C_FLOOR, "baseline_start_c" => base_start_c,
            "formula" => "min(max(floor, baseline_start + 1), temp_cap - 2)",
            "on_timeout" => "如实记录后继续 (组内 ABBA 交替已让两臂承受同样的残余热漂移), 不作废本组",
        },
    ));
    let base_py = runs_as_pyval(&base_runs);
    rule.push(("baseline".into(), loopstat::describe(&base_py, "baseline")));
    let rule_val = PyVal::Obj(rule.clone());
    std::fs::write(out.join("rule.json"), dumps_indent(&rule_val, 1)).ok();
    let rget = |k: &str| rule.iter().find(|(k2, _)| k2 == k).map(|(_, v)| v.clone());
    log(&format!(
        "判定规则已冻结: 温度上限 {temp_cap}C, 等冷目标 {cool_target}C, 指标 {} 个, alpha_win={}, {}v{} 最小可达 p={}, 可达={}; {power_note}",
        rget("n_metrics").map(|v| v.py_str()).unwrap_or_default(),
        rget("alpha_win_bonferroni").map(|v| v.py_str()).unwrap_or_default(),
        a.pairs, a.pairs,
        rget("min_reachable_p").map(|v| v.py_str()).unwrap_or_default(),
        rget("reachable").map(|v| v.py_str()).unwrap_or_default(),
    ));
    if !matches!(rget("reachable"), Some(PyVal::Bool(true))) {
        log("警告: 轮数不足以达到 Bonferroni 收紧后的显著性门槛, 本次任何候选都不可能判保留");
    }
    let rule_metrics: Vec<String> = rget("metrics")
        .and_then(|v| match v {
            PyVal::List(ms) => Some(
                ms.iter()
                    .filter_map(|m| m.get("id").map(|x| x.py_str()))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();

    // 大模型密钥: 不给或读不到就全程走本地变异器
    let root = crate::eval::repo_root();
    let keys: Vec<(String, String)> = if a.no_llm {
        Vec::new()
    } else {
        llm::load_keys(&[
            root.join("secrets.env").to_string_lossy().into_owned(),
            "/Users/mac/projects/phonefarm/secrets.env".into(),
        ])
        .unwrap_or_else(|e| {
            log(&format!("secrets 读不到, 全程走本地变异器: {e}"));
            Vec::new()
        })
    };
    let wl_desc = sysparam::describe_whitelist(&wl);
    let mut history: Vec<PyVal> = Vec::new();
    let mut all_results: Vec<PyVal> = Vec::new();
    let mut session_keys: Vec<String> = Vec::new();

    for gen in 1..=a.generations {
        log(&format!("=== 第 {gen} 代 ==="));
        let gdir = out.join(format!("gen{gen}"));
        let _ = std::fs::create_dir_all(&gdir);

        // 6) 大模型挑参数 (失败降级到本地变异器)
        let mut cands: Vec<Candidate> = Vec::new();
        let mut source = "local_mutate".to_string();
        if !keys.is_empty() {
            let prompt = llm::build_prompt(&wl_desc, &history, a.children as usize, "");
            std::fs::write(gdir.join("prompt.txt"), &prompt).ok();
            if let Some((prov, raw)) = llm::chat(&prompt, &keys, &gdir.display().to_string(), &log) {
                source = prov;
                std::fs::write(gdir.join("llm_raw.txt"), &raw).ok();
                cands = llm::parse_candidates(&raw, a.children as usize);
                log(&format!("{source} 给出 {} 组", cands.len()));
            }
        }
        let mut valid: Vec<Candidate> = Vec::new();
        for c in cands {
            let (ok, why) = sysparam::validate_candidate(&c.params, &wl);
            if ok {
                valid.push(c);
            } else {
                log(&format!(
                    "作废一组 (模型给的参数不合法): {why} — {}",
                    crate::pyjson::dumps_compact(&c.params_pyval())
                ));
            }
        }
        if valid.len() < a.children as usize {
            let need = a.children as usize - valid.len();
            log(&format!("用本地变异器补 {need} 组"));
            let mut hist = history.clone();
            for v in &valid {
                hist.push(pyobj! { "params" => v.params_pyval() });
            }
            for c in llm::local_mutate(&wl, &hist, need, (gen as i64) * 1000 + valid.len() as i64) {
                let (ok, _why) = sysparam::validate_candidate(&c.params, &wl);
                if ok {
                    valid.push(c);
                }
            }
        }
        let cands_json = pyobj! {
            "source" => source.clone(),
            "candidates" => PyVal::List(valid.iter().map(|c| c.to_pyval()).collect()),
        };
        std::fs::write(gdir.join("candidates.json"), dumps_indent(&cands_json, 1)).ok();

        // 7) 逐组上真机
        for (ci, cand) in valid.iter().enumerate() {
            let cdir = gdir.join(format!("cand{}", ci + 1));
            let _ = std::fs::create_dir_all(&cdir);
            log(&format!(
                "[gen{gen}/cand{}] {}",
                ci + 1,
                crate::pyjson::dumps_compact(&cand.params_pyval())
            ));
            let res = run_candidate(&CandidateRun {
                serial: &serial,
                cand,
                wl: &wl,
                outdir: &cdir,
                pairs: a.pairs,
                temp_cap_c: temp_cap,
                power_available,
                metrics: &rule_metrics,
                cool_c: cool_target,
                workload: &workload,
            });
            for k in touched_keys(&cand.params, &wl) {
                if !session_keys.contains(&k) {
                    session_keys.push(k);
                }
            }
            let verdict = res.get("verdict").map(|v| v.py_str()).unwrap_or_default();
            // 把 KNOB_FAIL 原文喂回模型: 实测 gpu.min_pwrlevel 写 0 会被内核夹到 2
            let clamped: Vec<String> = res
                .get("apply_log")
                .and_then(|v| match v {
                    PyVal::List(ls) => Some(ls.clone()),
                    _ => None,
                })
                .unwrap_or_default()
                .iter()
                .flat_map(|lg| lg.py_str().lines().map(String::from).collect::<Vec<_>>())
                .filter(|l| l.contains("KNOB_FAIL") || l.contains("KNOB_REFUSE"))
                .collect();
            history.push(pyobj! {
                "params" => cand.params_pyval(),
                "verdict" => verdict.clone(),
                "reason" => res.get("reason").cloned().unwrap_or(PyVal::Null),
                "clamped" => PyVal::List(clamped.into_iter().map(PyVal::Str).collect()),
                "per_metric" => res.get("per_metric").cloned().unwrap_or(PyVal::Null),
            });
            log(&format!("[gen{gen}/cand{}] 判定 {verdict}: {}", ci + 1,
                res.get("reason").map(|v| v.py_str()).unwrap_or_default()));
            let mut entry = match res {
                PyVal::Obj(kvs) => kvs,
                _ => Vec::new(),
            };
            entry.push(("gen".into(), PyVal::Int(gen as i64)));
            entry.push(("cand".into(), PyVal::Int(ci as i64 + 1)));
            all_results.push(PyVal::Obj(entry));
        }
    }

    // 8) 收尾: 强制还原 (旋钮 + 充电) + 全局快照比对
    let _ = charge_restore(&serial);
    let fin_restore = su(&serial, &format!("sh {DEV_TMP}/knob_sysparam.sh restore"), 120);
    if fin_restore.contains("KNOB_RESTORE_FAIL") || fin_restore.contains("#adb_exit=") && !fin_restore.ends_with("#adb_exit=0") {
        // state 文件是「还原不成功时唯一的回滚依据」, 这时候删它等于把现场毁了
        log(&format!("警告: 收尾还原失败, 保留 state 文件以便人工回滚:\n{fin_restore}"));
    } else {
        let _ = su(
            &serial,
            &format!("rm -f {DEV_TMP}/loop_v1_sysparam.state {DEV_TMP}/loop_v1_knob_ddr.state"),
            30,
        );
    }
    let fin_cool = wait_cool(&serial, cool_target, COOL_TIMEOUT_S);
    log(&format!("收尾等冷: {}", dumps_indent(&fin_cool, 0).replace('\n', " ")));
    let snap_final = snapshot(&serial);
    std::fs::write(out.join("snap_final.txt"), &snap_final).ok();
    // 收尾也按「我们写过什么」判, 与逐候选同一套口径
    let final_sd = classify_diff(
        &snapshot_diff(
            &out.join("snap_before.txt").to_string_lossy(),
            &out.join("snap_final.txt").to_string_lossy(),
        ),
        Some(&session_keys),
    );
    let ours_identical = matches!(final_sd.get("ours_identical"), Some(PyVal::Bool(true)));
    let strict_identical = matches!(final_sd.get("strict_identical"), Some(PyVal::Bool(true)));

    let kept: Vec<&PyVal> = all_results
        .iter()
        .filter(|r| r.get("verdict").map(|v| v.py_str()) == Some("KEEP".into()))
        .collect();
    let fan_states: Vec<String> = base_runs
        .iter()
        .filter_map(|(_, m)| m["env"]["fan_state"].as_str().filter(|s| !s.is_empty()).map(String::from))
        .collect();
    let batt: Vec<String> = base_runs
        .iter()
        .filter_map(|(_, m)| m["env"]["battery_status"].as_str().map(String::from))
        .collect();
    let psrc: Vec<String> = base_runs
        .iter()
        .filter_map(|(_, m)| m["env"]["power_source"].as_str().map(String::from))
        .collect();
    let sort_uniq = |mut v: Vec<String>| {
        v.sort();
        v.dedup();
        PyVal::List(v.into_iter().map(PyVal::Str).collect())
    };
    let report = pyobj! {
        "goal" => "系统参数全自动闭环 (原神实测)",
        "workload" => workload.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        "spin_check" => spin,
        "test_conditions" => pyobj! {
            "device" => serial,
            "fan_state_at_baseline" => sort_uniq(fan_states),
            "battery_status_at_baseline" => sort_uniq(batt),
            "power_in_verdict" => power_available,
            "power_caveat" => power_note,
            "power_source_at_baseline" => sort_uniq(psrc),
            "charge_suspended_during_measurement" => base_charge_log.contains("CHARGE_SUSPENDED"),
            "fan_is_a_knob" => false,
            "fan_note" => "风扇转速是系统参数的一种, 但不进自动调参白名单 (DENY_KEYWORDS 含 fan)",
            "power_rail" => "battery",
        },
        "rule" => rule_val,
        "whitelist" => PyVal::List(ids.iter().map(|s| PyVal::Str(s.to_string())).collect()),
        "whitelist_detail" => sysparam::whitelist_to_pyval(&wl),
        "probe_residue" => probe_sd,
        "generations" => a.generations as i64,
        "children_per_generation" => a.children as i64,
        "pairs_per_candidate" => a.pairs as i64,
        "results" => PyVal::List(all_results.clone()),
        "kept" => PyVal::List(kept.iter().map(|r| pyobj! {
            "params" => r.get("params").cloned().unwrap_or(PyVal::Null),
            "reason" => r.get("reason").cloned().unwrap_or(PyVal::Null),
            "per_metric" => r.get("per_metric").cloned().unwrap_or(PyVal::Null),
        }).collect()),
        "rejected" => PyVal::List(all_results.iter()
            .filter(|r| r.get("verdict").map(|v| v.py_str()) != Some("KEEP".into()))
            .map(|r| pyobj! {
                "params" => r.get("params").cloned().unwrap_or(PyVal::Null),
                "verdict" => r.get("verdict").cloned().unwrap_or(PyVal::Null),
                "reason" => r.get("reason").cloned().unwrap_or(PyVal::Null),
            }).collect()),
        "final_restore_log" => PyVal::List(fin_restore.trim().lines().map(|l| PyVal::Str(l.into())).collect()),
        "final_cooldown" => fin_cool,
        "final_snapshot_identical" => ours_identical,
        "final_snapshot_strict_identical" => strict_identical,
        "final_snapshot_diff" => final_sd,
    };
    std::fs::write(out.join("report.json"), dumps_indent(&report, 1)).ok();
    log(&format!(
        "完成。保留 {} 组 / 共 {} 组; 收尾快照一致 (我们写过的项): {}, 严格逐行一致: {}",
        kept.len(),
        all_results.len(),
        ours_identical,
        strict_identical
    ));
    if ours_identical {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysparam::{build_whitelist, parse_probe};

    // 与 loop_v1/auto/test_auto.py 同一份 PROBE (白名单/判定口径那部分已搬去
    // sysparam.rs, 这里只需要它造一份测试用的表)
    const PROBE: &str = "# probe_sysparam v1
cpu.policies=0,3
cpu.policy0.avail_freqs=300000 1000000 2000000
cpu.policy0.avail_governors=schedutil performance
cpu.policy0.scaling_min_freq.cur=300000
cpu.policy0.scaling_max_freq.cur=2000000
cpu.policy0.scaling_governor.cur=schedutil
cpu.policy0.scaling_min_freq.writable=yes
cpu.policy0.scaling_min_freq.effect=live
cpu.policy0.scaling_max_freq.writable=yes
cpu.policy0.scaling_governor.writable=yes
cpu.policy3.avail_freqs=500000 3000000
cpu.policy3.scaling_min_freq.cur=500000
cpu.policy3.scaling_min_freq.writable=yes
cpu.policy3.scaling_min_freq.effect=rejected(wrote=3000000 readback=500000)
gpu.num_pwrlevels=4
gpu.min_pwrlevel.cur=3
gpu.max_pwrlevel.cur=0
gpu.min_pwrlevel.writable=yes
gpu.min_pwrlevel.effect=live
gpu.max_pwrlevel.writable=yes
bus.DDR.boost_freq.cur=0
bus.DDR.hw_min_freq=200000
bus.DDR.hw_max_freq=5333000
bus.DDR.avail_freqs=200000 3200000 5333000
bus.DDR.boost_freq.writable=yes
bus.DDR.boost_freq.effect=live
setting.system.peak_refresh_rate.cur=120
setting.system.min_refresh_rate.cur=60
display.modes=fps=60,fps=120
thermal.cpu-1-0=42000
";

    fn wl() -> sysparam::Whitelist {
        build_whitelist(&parse_probe(PROBE))
    }
    fn cand(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn key_mapping_matches_device_snapshot_naming() {
        // device_snapshot.sh 用 scaling_max / governor, 不是 sysfs 的叶子名
        assert_eq!(
            snapshot_key_of("sysfs", "/sys/devices/system/cpu/cpufreq/policy6/scaling_max_freq")
                .as_deref(),
            Some("cpu.policy6.scaling_max")
        );
        assert_eq!(
            snapshot_key_of("sysfs", "/sys/devices/system/cpu/cpufreq/policy0/scaling_governor")
                .as_deref(),
            Some("cpu.policy0.governor")
        );
        assert_eq!(
            snapshot_key_of("sysfs", "/sys/class/kgsl/kgsl-3d0/max_pwrlevel").as_deref(),
            Some("kgsl.max_pwrlevel")
        );
        assert_eq!(
            snapshot_key_of("sysfs", "/sys/devices/system/cpu/bus_dcvs/DDR/boost_freq").as_deref(),
            Some("bus.DDR.boost_freq")
        );
        assert_eq!(
            snapshot_key_of("setting", "system:refresh_rate_mode").as_deref(),
            Some("settings.refresh_rate_mode")
        );
    }

    /// 碰 governor 会波及同 policy 的 min/max, 三项都算我们的账。
    #[test]
    fn touched_keys_include_the_rollback_siblings() {
        let w = wl();
        let mut keys = touched_keys(
            &cand(&[("cpu.policy0.scaling_governor", "performance")]),
            &w,
        );
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "cpu.policy0.governor".to_string(),
                "cpu.policy0.scaling_max".to_string(),
                "cpu.policy0.scaling_min".to_string(),
            ]
        );
    }

    fn sd(pairs: &[(&str, &str, &str)]) -> PyVal {
        pyobj! {
            "checked" => true,
            "n_lines" => 38i64,
            "identical" => pairs.is_empty(),
            "n_diff" => pairs.len() as i64,
            "diffs" => PyVal::List(pairs.iter().enumerate().map(|(i, (k, a, b))| pyobj! {
                "line" => (i as i64) + 1,
                "before" => format!("{k}={a}"),
                "after" => format!("{k}={b}"),
            }).collect()),
        }
    }

    /// 只写了 DDR/LLCC 的那一组, 厂商管家改的三项不该算我们留的痕;
    /// 严格 diff 原样保留, 不藏。
    #[test]
    fn vendor_drift_on_untouched_keys_is_not_our_residue() {
        let w = wl();
        let keys = touched_keys(&cand(&[("bus.DDR.boost_freq", "5333000")]), &w);
        let c = classify_diff(
            &sd(&[
                ("cpu.policy0.scaling_max", "1785600", "1228800"),
                ("kgsl.max_pwrlevel", "2", "0"),
            ]),
            Some(&keys),
        );
        assert_eq!(c.get("ours_identical"), Some(&PyVal::Bool(true)));
        assert_eq!(c.get("strict_identical"), Some(&PyVal::Bool(false)));
        assert_eq!(
            c.get("environment_diffs").and_then(|v| match v {
                PyVal::List(l) => Some(l.len()),
                _ => None,
            }),
            Some(2)
        );
    }

    /// 我们写过的节点没回原值 = 留痕, 判 ABORT 的依据。
    #[test]
    fn a_node_we_wrote_still_counts_as_residue() {
        let w = wl();
        let keys = touched_keys(&cand(&[("bus.DDR.boost_freq", "5333000")]), &w);
        let c = classify_diff(&sd(&[("bus.DDR.boost_freq", "0", "5333000")]), Some(&keys));
        assert_eq!(c.get("ours_identical"), Some(&PyVal::Bool(false)));
        assert_eq!(c.get("environment_diffs"), Some(&PyVal::List(vec![])));
    }

    /// 切 governor 导致 scaling_max 没回来 —— 这正是必须被抓住的那一类。
    #[test]
    fn sibling_left_behind_is_caught() {
        let w = wl();
        let keys = touched_keys(
            &cand(&[("cpu.policy0.scaling_governor", "performance")]),
            &w,
        );
        let c = classify_diff(&sd(&[("cpu.policy0.scaling_max", "1785600", "1228800")]), Some(&keys));
        assert_eq!(c.get("ours_identical"), Some(&PyVal::Bool(false)));
    }

    /// 干净快照两层都过。
    #[test]
    fn clean_snapshot_passes_both_layers() {
        let c = classify_diff(&sd(&[]), Some(&[]));
        assert_eq!(c.get("ours_identical"), Some(&PyVal::Bool(true)));
        assert_eq!(c.get("strict_identical"), Some(&PyVal::Bool(true)));
    }

    /// 温度上限与等冷目标的推导公式 (与 rule.json 里的 derivation 文本一致)。
    #[test]
    fn temp_cap_and_cooldown_derivations() {
        // max(floor, baseline_max + margin): 基线 55.2C 时上限 70C (floor 顶住)
        let cap = TEMP_CAP_FLOOR_C.max(py_round(55.2 + TEMP_CAP_MARGIN_C, 1));
        assert_eq!(cap, TEMP_CAP_FLOOR_C);
        // 基线 65C 时上限 75C (margin 生效)
        assert_eq!(TEMP_CAP_FLOOR_C.max(py_round(65.0 + 10.0, 1)), 75.0);
        // 等冷目标: min(max(floor, base+1), cap-2)
        let cool = COOL_C_FLOOR.max(py_round(55.0 + 1.0, 1)).min(75.0 - 2.0);
        assert_eq!(cool, 56.0);
    }
}
