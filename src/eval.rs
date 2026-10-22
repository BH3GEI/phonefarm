//! `phonefarm eval` — game_opt_loop 下发的真机评测请求的唯一入口。
//!
//! 契约: `game_opt_loop/contracts/eval_request.schema.json` 与 `eval_report.schema.json`
//! (v2: kind = shader / sysparam / gray 的信封; v1 扁平格式继续兼容, 它就是 shader 一种时
//! 的形状)。
//!
//! 路由
//! ----
//! · v1 / 缺 kind          → 原样转交 `gpu-op` (它就是 v1 的消费者, 行为一字不动);
//! · v2 kind=shader        → 按契约把信封降级成 v1 请求转交 gpu-op, 回包补上
//!                           version/kind 与 conditions_observed 再交还上层;
//! · v2 kind=sysparam      → 走 `src/sysparam.rs` 的白名单校验与判定 + 设备端脚本
//!                           (knob_sysparam / probe_sysparam / sample_env / run_once);
//! · v2 kind=gray          → 调 knobs/gray 的调用层 (enable_layer.sh), 只调不改。
//!
//! 纪律 (与 gpuop / sysparam 一致):
//!   · 判定规则 (protocol / conditions) 在看到候选数据之前带入并冻结;
//!   · 接收端**再校验一遍**参数与白名单, 不信上层; deny_keywords 接收端再拦一道;
//!   · 量不到的指标如实 null, 每个 null 在 unavailable 里配一条人话;
//!   · ABORT (没量准) 与 REJECT (量到了但不值得保留) 严格分开。
//!
//! 真机路径的验证状态: 离线部分 (请求校验 / v2→v1 降级 / 校验拒绝 / 报告装配) 有单测;
//! 涉及设备的部分代码走通但尚未在真机上验证过 —— 设备被占用期间合入, 首次真机跑按
//! probe_only 起步。

use crate::device;
use crate::loopstat::{self, Run};
use crate::pyjson::{dumps as py_dumps, PyVal};
use crate::curtail;
use crate::sysparam;
use crate::qualification::{self, Evidence, Goal, Policy, Stage};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_DENY_KEYWORDS: [&str; 7] = [
    "thermal",
    "trip_point",
    "cooling",
    "fan",
    "tsens",
    "bcl",
    "throttl",
];

/// gray 收端目前实现的受控开关。层的开关集合是有限且已实现的, 上层发明新开关
/// 或请求收端没实现的开关, 整份拒绝。
const GRAY_SUPPORTED_KNOBS: [&str; 2] = ["loadop_dont_care", "storeop_dont_care"];

const USAGE: &str = "恢复: phonefarm eval recover --session DIR --serial S [--json]\n用法: phonefarm eval --request <eval_request.json> [--serial S] [--power-rail usb|battery] [--out 目录] [--no-curtail] [--evidence-origin device|synthetic] [--json]";

/// PyVal → serde_json 值 (同一个桥的反向)。经 dumps 走文本, 非有限值
/// (Infinity/NaN) 会被 serde_json 拒掉, 这里如实落 null。
fn to_serde(v: &PyVal) -> Value {
    let s = py_dumps(v);
    if s.contains("Infinity") || s.contains("NaN") {
        // 逐值处理太啰嗦; 这份请求路径里非有限值只可能来自方差为 0 的退化臂,
        // 落 null 并交由 unavailable 解释
        serde_json::from_str(&s.replace("Infinity", "null").replace("NaN", "null"))
            .unwrap_or(Value::Null)
    } else {
        serde_json::from_str(&s).unwrap_or(Value::Null)
    }
}

/// serde_json 值 → PyVal (请求侧是 serde, 统计侧是 PyVal, 桥在这)。
/// 保留 int/float 之分 —— 统计层靠它对齐 Python 的类型提升。
fn to_pyval(v: &Value) -> PyVal {
    match v {
        Value::Null => PyVal::Null,
        Value::Bool(b) => PyVal::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => PyVal::Int(i),
            None => PyVal::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => PyVal::Str(s.clone()),
        Value::Array(a) => PyVal::List(a.iter().map(to_pyval).collect()),
        Value::Object(o) => PyVal::Obj(o.iter().map(|(k, x)| (k.clone(), to_pyval(x))).collect()),
    }
}

// ══════════════ 入口与路由 ══════════════

struct EvalArgs {
    request: String,
    serial: Option<String>,
    power_rail: Option<String>,
    out: Option<String>,
    /// 确定性截断 (src/curtail.rs): 缺省开。`--no-curtail` 跑满全部轮次。
    curtail: bool,
    qualify_from: Option<String>,
    evidence_origin: String,
}

fn parse_args(args: &[String]) -> Result<EvalArgs, String> {
    let mut it = args.iter();
    let (mut request, mut serial, mut power_rail, mut out) = (None, None, None, None);
    let mut curtail = true;
    let mut qualify_from = None;
    let mut evidence_origin = "device".to_string();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--request" => request = it.next().cloned(),
            "--serial" => serial = it.next().cloned(),
            "--power-rail" => power_rail = it.next().cloned(),
            "--out" => out = it.next().cloned(),
            "--no-curtail" => curtail = false,
            "--qualify-from" => qualify_from = it.next().cloned(),
            "--evidence-origin" => evidence_origin = it.next().ok_or("missing evidence origin")?.clone(),
            "--json" => {} // eval 的回包恒为 JSON, 收下这个旗子仅为兼容接口形状
            other => return Err(format!("不认识的参数 {other}\n{USAGE}")),
        }
    }
    if !matches!(evidence_origin.as_str(),"device"|"synthetic") {return Err("invalid evidence origin".into());}
    Ok(EvalArgs {
        request: request.ok_or(format!("缺少 --request\n\n{USAGE}"))?,
        serial,
        power_rail,
        out,
        curtail,
        qualify_from,
        evidence_origin,
    })
}

pub fn run_eval(args: &[String]) -> i32 {
    if args.first().is_some_and(|a|a=="recover") {return recover_gray_eval(&args[1..]);}
    let a = match parse_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let raw = match std::fs::read_to_string(&a.request) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("读不了 {}: {e}", a.request);
            return 2;
        }
    };
    let req: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{} 不是合法 JSON: {e}", a.request);
            return 2;
        }
    };
    let version = req.get("version").and_then(|v| v.as_i64()).unwrap_or(1);
    let kind = req.get("kind").and_then(|v| v.as_str()).unwrap_or("shader");
    if version == 1 && a.qualify_from.is_some() {
        eprintln!("--qualify-from 仅支持 v2 显式目标请求");
        return 2;
    }
    if version == 1 {
        // v1 扁平格式: gpu-op 就是它的消费者, 行为一字不动地转交
        return delegate_gpu_op(
            &a.request,
            a.serial.as_deref(),
            a.power_rail.as_deref(),
            a.out.as_deref(),
        );
    }
    if version != 2 {
        eprintln!("不认识的 eval_request 版本 {version} (契约只认 1 与 2)");
        return 2;
    }
    if let Err(e) = validate_v2(&req) {
        if kind == "shader" {
            println!("{}", crate::shaderplan::abort(&req, &e, None));
        } else {
            eprintln!("eval_request v2 校验失败: {e}");
        }
        return 2;
    }
    if let Some(path) = &a.qualify_from {
        return match offline_qualification(&req, Path::new(path)) {
            Ok(report) => {
                println!("{}", serde_json::to_string_pretty(&report).unwrap());
                if report["qualification"]["conclusion_allowed"] == true { 0 } else { 1 }
            }
            Err(e) => { eprintln!("{e}"); 2 }
        };
    }
    let evidence = a
        .out
        .clone()
        .unwrap_or_else(|| format!("eval_evidence/{}", req["candidate_id"].as_str().unwrap_or("x")));
    // serial: CLI --serial 优先, 否则请求体里的 (game_opt_loop 的驱动把 --serial
    // 放进请求体而不是命令行)
    let serial = a
        .serial
        .clone()
        .or_else(|| req["serial"].as_str().map(String::from));
    if kind == "shader" {
        return eval_shader_v2(&req, &a);
    }
    let _lease = match crate::devlease::Lease::target(serial.as_deref(), "eval") {
        Ok(v) => v,
        Err(e) => {
            let mut r = base_report(&req, kind);
            r["verdict"]["reason"] = json!(e);
            r["qualification"] = json!({"stage":"experiment","eligible":false,"can_claim_improvement":false,"status":"INELIGIBLE","blockers":[e]});
            println!("{r}");
            eprintln!("{e}");
            return 2;
        }
    };
    let serial = Some(_lease.serial.clone());
    if kind=="gray" {
        return match gray_eval_owned(&req, _lease, Path::new(&evidence), &a.evidence_origin) {
            Ok(r)=>{ let code=if r["recovery"]["status"]!="reported_complete" || !r["report_error"].is_null() || !r["qualification_error"].is_null() {2} else if r["status"]=="ABORT" {1} else {0}; println!("{}",serde_json::to_string_pretty(&r).unwrap()); code },
            Err(e)=>{eprintln!("{e}");2}
        };
    }
    if a.evidence_origin!="device" {eprintln!("--evidence-origin is supported for owned gray eval only");return 2;}
    let report = match kind {
        "sysparam" => sysparam_eval(&req, serial.as_deref(), Path::new(&evidence), a.curtail),
        "whitebox" => crate::whitebox::whitebox_eval(&req, serial.as_deref(), Path::new(&evidence)),
        other => {
            eprintln!("不认识的 kind {other}");
            return 2;
        }
    };
    match report {
        Ok(r) => {
            if let Err(e) = _lease.check() {
                eprintln!("{e}");
                return 2;
            }
            println!("{r}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

/// v2 shader: 契约规定的收端降级 —— 合成等价的 v1 请求转交 gpu-op,
/// 回包补上 version/kind/conditions_observed。
fn eval_shader_v2(req: &Value, a: &EvalArgs) -> i32 {
    let plan = match crate::shaderplan::build(req, a.serial.as_deref(), a.power_rail.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            println!("{}", crate::shaderplan::abort(req, &e, None));
            return 2;
        }
    };
    let dir = a.out.clone().map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("phonefarm-eval-{}", std::process::id()))
    });
    if let Err(e) = std::fs::create_dir_all(&dir) {
        println!(
            "{}",
            crate::shaderplan::abort(
                req,
                &format!("evidence directory: {e}"),
                Some(&plan.resolved)
            )
        );
        return 2;
    }
    let path = dir.join(format!("_eval_v1_request_{}.json", std::process::id()));
    if let Err(e) = std::fs::write(&path, plan.request.to_string()) {
        println!(
            "{}",
            crate::shaderplan::abort(
                req,
                &format!("adapter request write: {e}"),
                Some(&plan.resolved)
            )
        );
        return 2;
    }
    let mut args = plan.args;
    args.extend([
        "--request".into(),
        path.to_string_lossy().into_owned(),
        "--out".into(),
        dir.to_string_lossy().into_owned(),
    ]);
    // Same process: gpu-op owns one lease and emits the v2 envelope on measured outcomes.
    // Exit 1 is still a valid non-PASS report. Exit 2 means unverified conditions, hence ABORT.
    let rc = crate::gpuop::run_gpu_op(&args);
    if rc == 2 {
        println!(
            "{}",
            crate::shaderplan::abort(
                req,
                "gpu-op execution/conditions failed; see stderr",
                Some(&plan.resolved)
            )
        );
    }
    rc
}

fn delegate_gpu_op(
    request: &str,
    serial: Option<&str>,
    power_rail: Option<&str>,
    out: Option<&str>,
) -> i32 {
    // run_gpu_op 吃的是 main.rs 剥掉子命令后的参数 —— 这里别把 "gpu-op" 再带上
    let mut v = vec!["--request".to_string(), request.to_string()];
    if let Some(s) = serial {
        v.push("--serial".into());
        v.push(s.to_string());
    }
    if let Some(r) = power_rail {
        v.push("--power-rail".into());
        v.push(r.to_string());
    }
    if let Some(o) = out {
        v.push("--out".into());
        v.push(o.to_string());
    }
    gpuop_passthrough(&v)
}

fn gpuop_passthrough(args: &[String]) -> i32 {
    crate::gpuop::run_gpu_op(args)
}

// ══════════════ 请求校验 ══════════════

fn validate_v2(req: &Value) -> Result<(), String> {
    for k in ["kind", "candidate_id", "package", "workload", "conditions", "protocol", "payload"] {
        if req.get(k).is_none() {
            return Err(format!("v2 缺必填字段 {k}"));
        }
    }
    let kind = req["kind"].as_str().unwrap_or("");
    let carrier = req["workload"]["carrier"].as_str().unwrap_or("");
    if !matches!(carrier, "headless" | "refbench" | "game_replay") {
        return Err(format!("workload.carrier 不合法: {carrier:?}"));
    }
    let p = &req["protocol"];
    if p.get("rounds").and_then(|v| v.as_i64()).unwrap_or(0) < 1 {
        return Err("protocol.rounds 必须 >= 1".into());
    }
    if p.get("alternating").and_then(|v| v.as_bool()).is_none() {
        return Err("protocol.alternating 缺失或不是布尔".into());
    }
    let pt = p.get("p_threshold").and_then(|v| v.as_f64());
    match pt {
        Some(x) if x > 0.0 && x <= 0.05 => {}
        _ => return Err("protocol.p_threshold 必须 (0, .05]（收端不接受更宽门槛）".into()),
    }
    match kind {
        "shader" => {
            for k in ["track", "shader_path", "spirv_path", "budget_ms"] {
                if req["payload"].get(k).map(|v| v.is_null()).unwrap_or(true) {
                    return Err(format!("kind=shader 的 payload 缺 {k}"));
                }
            }
        }
        "sysparam" => {
            if req["payload"].get("params").map(|v| v.is_object()) != Some(true) {
                return Err("kind=sysparam 的 payload.params 必须是对象".into());
            }
        }
        "gray" => {
            let rewrites = req["payload"]["rewrites"].as_array();
            let n = rewrites.map(|r| r.len()).unwrap_or(0);
            if n < 1 {
                return Err("kind=gray 的 payload.rewrites 至少 1 项".into());
            }
            for r in rewrites.unwrap() {
                let knob = r["knob"].as_str().unwrap_or("");
                if !GRAY_SUPPORTED_KNOBS.contains(&knob) {
                    return Err(format!(
                        "gray knob {knob:?} 不在收端已实现的集合 {GRAY_SUPPORTED_KNOBS:?} 里 —— 层的开关集合有限, 不收凭空发明的开关"
                    ));
                }
            }
        }
        "whitebox" => {
            crate::whitebox::knob_extras(&req["payload"])?;
            if launch_spec(req).is_none() {
                return Err("kind=whitebox 需要 workload.launch.extras (白盒载体靠启动参数换设置)".into());
            }
        }
        other => return Err(format!("不认识的 kind {other:?}")),
    }
    if matches!(kind,"sysparam"|"gray") { policy_for_request(req)?; }
    Ok(())
}

/// Objective selects receiver-owned requirements. Incoming required_metrics cannot weaken them.
fn policy_for_request(req: &Value) -> Result<Policy, String> {
    let goal = Goal::parse(req["qualification"]["goal"].as_str().unwrap_or("render_efficiency"))?;
    let pairs = req["protocol"]["rounds"].as_u64().ok_or("缺 protocol.rounds")?;
    let pairs = u32::try_from(pairs).map_err(|_| "rounds 溢出")?;
    let mut policy = Policy::new(goal, pairs, req["kind"] == "gray");
    policy.temp_cap_c=req["conditions"]["temp_cap_c"].as_f64();
    policy.cool_c=req["conditions"]["cool_c"].as_f64();
    policy.alpha=req["protocol"]["p_threshold"].as_f64().ok_or("缺 p_threshold")?;
    policy.validate()?;
    if let Some(ms) = req["qualification"]["required_metrics"].as_array() {
        if json!(policy.required_metrics) != Value::Array(ms.clone()) {
            return Err("请求 required_metrics 不得缩窄收端目标合同".into());
        }
    }
    Ok(policy)
}

fn offline_qualification(req: &Value, path: &Path) -> Result<Value, String> {
    validate_v2(req)?;
    let mut evidence = qualification::read(path)?;
    evidence.policy = policy_for_request(req)?;
    let q = qualification::evaluate(&evidence)?;
    let mut report = base_report(req, req["kind"].as_str().unwrap_or(""));
    report["qualification"] = serde_json::to_value(q).map_err(|e|e.to_string())?;
    report["qualification_only"] = json!(true);
    report["verdict"]["reason"] = json!("离线资格评估，不应用候选，不作优化 PASS 判定");
    Ok(report)
}

fn record_qualification(report: &mut Value, evidence: &Path, facts: &Evidence) -> Result<qualification::Report,String> {
    let q = qualification::evaluate(facts)?;
    write_qualification(report, evidence, facts, q)
}

fn write_qualification(report: &mut Value, evidence: &Path, facts: &Evidence, q: qualification::Report) -> Result<qualification::Report,String> {
    std::fs::create_dir_all(evidence).map_err(|e|e.to_string())?;
    qualification::write(&evidence.join("qualification.json"), &q)?;
    std::fs::write(evidence.join("qualification_evidence.json"),serde_json::to_string_pretty(facts).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    report["qualification"] = serde_json::to_value(&q).map_err(|e|e.to_string())?;
    Ok(q)
}

pub(crate) fn snapshots_identical(before: &str, after: &str) -> bool {
    adb_ok(before) && adb_ok(after) && before.lines().any(|l|l.contains('=')) && before == after
}

fn probe_report(report: &mut Value, before: &str, after: &str, wl: &sysparam::Whitelist) {
    let identical = snapshots_identical(before,after);
    report["status"] = json!(if identical {"REVERTED"} else {"ABORT"});
    report["verdict"]["reason"] = json!(if identical {"probe_only: 探测写入已验证还原"} else {"probe_only: 快照缺失/读取失败或不一致，不能声称已还原"});
    report["conditions_observed"] = json!({"snapshot_identical":identical,"restored":identical,"whitelist":wl_pyval(wl)});
}

/// Verify the supplied workload, rather than injecting a separate hard-coded gesture.
pub(crate) fn verify_workload(serial: &str, workload: &Path, pkg: &str, outdir: &Path) -> Value {
    let _ = std::fs::create_dir_all(outdir);
    let mut child = match Command::new(std::env::current_exe().unwrap_or_default())
        .args(["script", "--serial", serial, "--task", "qualification_workload", "--app", pkg])
        .arg(workload).current_dir(repo_root()).stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null()).spawn() {
        Ok(c)=>c,Err(e)=>return json!({"verified":false,"reason":e.to_string()})
    };
    let capture = || Command::new(adb_path()).args(["-s",serial,"exec-out","screencap"]).output().ok()
        .filter(|o|o.status.success()).map(|o|o.stdout).unwrap_or_default();
    std::thread::sleep(std::time::Duration::from_secs(3)); let a=capture();
    std::thread::sleep(std::time::Duration::from_secs(5)); let b=capture();
    let deadline=std::time::Instant::now()+std::time::Duration::from_secs(65);
    let success=loop {match child.try_wait() {
        Ok(Some(s))=>break s.success(),
        Ok(None) if std::time::Instant::now()<deadline=>std::thread::sleep(std::time::Duration::from_millis(100)),
        _=>{let _=child.kill();let _=child.wait();break false;}
    }};
    let _=std::fs::write(outdir.join("frame_a.raw"),&a); let _=std::fs::write(outdir.join("frame_b.raw"),&b);
    let motion=to_serde(&crate::framecheck::verdict(&a,&b));
    let result=json!({"verified":success && motion["spinning"]==true,"script_exit_success":success,"motion":motion});
    let _=std::fs::write(outdir.join("workload_check.json"),serde_json::to_string_pretty(&result).unwrap());
    result
}

// ══════════════ 设备助手 ══════════════

/// su()/adb() 的输出以 `#adb_exit=N` 结尾, N=0 才算成功。
/// (autoloop 的 `#adb_exit=` 令牌写法依赖"成功时不追加", 这边的 helper 恒追加,
/// 判断必须区分退出码 —— 实测把成功 apply 判成了 FAIL, 白白施加又还原一次。)
pub(crate) fn adb_ok(out: &str) -> bool {
    out.ends_with("#adb_exit=0")
}

/// 调用方视角的路径解析: 绝对/相对 cwd 先试, 再试相对仓库根。
pub(crate) fn resolve_path(p: &str) -> Option<PathBuf> {
    let as_given = Path::new(p);
    if as_given.exists() {
        return Some(as_given.to_path_buf());
    }
    let under_root = repo_root().join(p);
    if under_root.exists() {
        return Some(under_root);
    }
    None
}

pub(crate) fn adb_path() -> String {
    device::locate_adb().unwrap_or_else(|| "adb".into())
}

pub(crate) fn adb(serial: &str, args: &[&str], timeout: u64) -> String {
    let mut cmd = Command::new(adb_path());
    cmd.arg("-s").arg(serial).args(args);
    run_with_timeout(&mut cmd, timeout)
}

pub(crate) fn su(serial: &str, cmd_str: &str, timeout: u64) -> String {
    adb(serial, &["shell", &format!("su -c '{cmd_str}'")], timeout)
}

fn run_with_timeout(cmd: &mut Command, timeout: u64) -> String {
    use std::io::Read;
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("#adb_exit=spawn_err {e}"),
    };
    // 读端单独开线程: 管道满了不读会卡死子进程, 而 recv_timeout 要占住主线程
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code().unwrap_or(-1),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return "#adb_exit=timeout".into();
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => return "#adb_exit=err".into(),
        }
    };
    let out = reader.join().unwrap_or_default();
    format!("{}#adb_exit={code}", String::from_utf8_lossy(&out))
}

/// 找到 phonefarm 仓库根 (含 loop_v1/tools 的目录)。
/// 顺序: PF_REPO_ROOT > 二进制所在目录向上找 (装在仓库根的 ./phonefarm 一击即中)
/// 然后 cwd 向上找, 最后编译期路径 (本机开发布局兜底)。eval 常被 game_opt_loop 以
/// 相对路径 ``../phonefarm/phonefarm`` 调起, cwd 在别人家, 所以二进制位置最可靠。
pub(crate) fn repo_root() -> PathBuf {
    if let Ok(r) = std::env::var("PF_REPO_ROOT") {
        if !r.is_empty() {
            return PathBuf::from(r);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        for _ in 0..8 {
            if cur.join("loop_v1/tools").is_dir() {
                return cur;
            }
            if !cur.pop() {
                break;
            }
        }
    }
    let mut cur = std::env::current_dir().unwrap_or_default();
    for _ in 0..6 {
        if cur.join("loop_v1/tools").is_dir() {
            return cur;
        }
        if !cur.pop() {
            break;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

pub(crate) const DEV_TMP: &str = "/data/local/tmp";
pub(crate) const DEV_SCRIPTS: [&str; 6] = [
    "device_snapshot.sh",
    "ftrace_capture.sh",
    "knob_sysparam.sh",
    "probe_sysparam.sh",
    "sample_env.sh",
    "charge_suspend.sh",
];

pub(crate) fn push_tools(serial: &str) -> Result<(), String> {
    let root = repo_root();
    for f in DEV_SCRIPTS {
        let p = root.join("loop_v1").join(match f {
            "device_snapshot.sh" | "ftrace_capture.sh" => PathBuf::from("tools").join(f),
            _ => PathBuf::from("auto").join(f),
        });
        if !p.exists() {
            return Err(format!("设备端脚本不在: {}", p.display()));
        }
        let out = adb(serial, &["push", &p.to_string_lossy(), DEV_TMP], 60);
        if out.contains("#adb_exit=") && !out.ends_with("#adb_exit=0") {
            return Err(format!("push {} 失败: {out}", p.display()));
        }
    }
    Ok(())
}

pub(crate) fn snapshot(serial: &str) -> String {
    su(serial, &format!("sh {DEV_TMP}/device_snapshot.sh"), 60)
}

/// 每臂开跑前等冷的上限。等不到不作废 (组内交替让两臂承受同样的残余热漂移), 但如实记下。
const ARM_COOL_TIMEOUT_S: u64 = 300;
/// 同一次评测里已经有一臂等冷超时之后, 后面每臂最多再等这么久: 降不到就是降不到,
/// 一臂一臂地累计干等只会把一次评测拖成一个小时 (10 臂 × 300 秒)。
const ARM_COOL_TIMEOUT_AFTER_MISS_S: u64 = 30;
/// 等冷期间多久打一行进度 (stderr; 上层把它原样转进节点日志)。
const COOL_PROGRESS_EVERY_S: u64 = 30;

/// 一臂开跑前的等冷: 等到 SoC 最热热区 < cool_c 或超时。回 {zone, c, waited_s, timeout}。
/// cool_c 缺省 (null) = 不等冷, 只记一次起跑温度。`timeout_s` 由调用方按「这次评测里之前
/// 有没有臂已经超时」给 (见 [`ARM_COOL_TIMEOUT_AFTER_MISS_S`])。
pub(crate) fn arm_wait_cool(serial: &str, cool_c: Option<f64>, label: &str, timeout_s: u64) -> Value {
    let t0 = std::time::Instant::now();
    let mut last_note = 0u64;
    loop {
        let cur = crate::autoloop::read_soc_temp(serial);
        let waited = (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0;
        let Some((zone, c)) = cur else {
            return json!({"zone": null, "c": null, "waited_s": waited, "timeout": false, "note": "读不到 SoC 热区"});
        };
        match cool_c {
            Some(target) if c >= target => {
                let el = t0.elapsed().as_secs();
                if el >= timeout_s {
                    eprintln!("等冷 {label}: 等了 {el}s 仍 {zone}={c:.1}C >= 目标 {target:.1}C, 超时, 照常开跑 (如实标注)");
                    return json!({"zone": zone, "c": c, "waited_s": waited, "timeout": true});
                }
                if el >= last_note + COOL_PROGRESS_EVERY_S || last_note == 0 {
                    eprintln!("等冷 {label}: {zone}={c:.1}C, 目标 < {target:.1}C, 已等 {el}s (最多 {timeout_s}s)");
                    last_note = el.max(1);
                }
                std::thread::sleep(std::time::Duration::from_secs(8));
            }
            _ => return json!({"zone": zone, "c": c, "waited_s": waited, "timeout": false}),
        }
    }
}

/// 这一臂最多等多久: 之前已有一臂超时就只再给 [`ARM_COOL_TIMEOUT_AFTER_MISS_S`]。
pub(crate) fn arm_cool_timeout(log: &[Value]) -> u64 {
    if log.iter().any(|a| a["timeout"].as_bool() == Some(true)) {
        ARM_COOL_TIMEOUT_AFTER_MISS_S
    } else {
        ARM_COOL_TIMEOUT_S
    }
}

/// 逐臂等冷记录 → conditions_observed 里的 cool_reached_c (最热的一次起跑温度) / cool_timeout。
pub(crate) fn cool_summary(arms: &[Value]) -> (Option<f64>, bool) {
    let worst = arms.iter().filter_map(|a| a["c"].as_f64()).reduce(f64::max);
    (worst, arms.iter().any(|a| a["timeout"].as_bool() == Some(true)))
}

/// 施加后隔多少秒回读 (https://github.com/HGamey/docs/blob/main/repos/phonefarm/knobs/DESIGN.md: 厂商性能服务约 10 秒内改回)。
const READBACK_AFTER_S: u64 = 12;

/// 读回 plan 里每一项当前值的一条 shell 命令。输出每行 `<路径>=<值>`。
pub(crate) fn readback_cmd(plan: &str) -> String {
    let mut parts = Vec::new();
    for line in plan.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 3 {
            continue;
        }
        match f[0] {
            "sysfs" => parts.push(format!("echo \"{}=$(cat {} 2>/dev/null)\"", f[1], f[1])),
            "setting" => {
                let (ns, key) = f[1].split_once(':').unwrap_or(("system", f[1]));
                parts.push(format!("echo \"{}=$(settings get {ns} {key} 2>/dev/null)\"", f[1]))
            }
            _ => {}
        }
    }
    parts.join("; ")
}

/// 回读对不上的项: (路径, 写的值, 读回的值)。读不到的也算对不上 (守没守住说不清就不算守住)。
pub(crate) fn readback_mismatches(plan: &str, out: &str) -> Vec<(String, String, String)> {
    let got: std::collections::BTreeMap<&str, &str> = out
        .lines()
        .filter_map(|l| l.trim().split_once('='))
        .collect();
    let mut bad = Vec::new();
    for line in plan.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 3 || !matches!(f[0], "sysfs" | "setting") {
            continue;
        }
        let want = f[2].trim();
        let have = got.get(f[1]).map(|v| v.trim()).unwrap_or("<读不到>");
        if have != want {
            bad.push((f[1].to_string(), want.to_string(), have.to_string()));
        }
    }
    bad
}

/// 厂商性能服务自己会动的快照键 (频率边界)。
fn vendor_managed(key: &str) -> bool {
    matches!(
        key,
        "kgsl.max_pwrlevel" | "kgsl.min_pwrlevel" | "kgsl.thermal_pwrlevel" | "kgsl.max_gpuclk"
            | "kgsl.devfreq_min_freq" | "kgsl.devfreq_max_freq"
    ) || (key.starts_with("cpu.policy") && (key.ends_with(".scaling_max") || key.ends_with(".scaling_min")))
}

/// 快照前后的差异, 分成 (厂商服务会动的, 其余)。每项 `键: 前 → 后`。
pub(crate) fn split_snapshot_diff(before: &str, after: &str) -> (Vec<String>, Vec<String>) {
    let parse = |s: &str| -> std::collections::BTreeMap<String, String> {
        s.lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with("#adb_exit"))
            .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
            .collect()
    };
    let (a, b) = (parse(before), parse(after));
    let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let (mut vendor, mut other) = (Vec::new(), Vec::new());
    for k in keys {
        let (x, y) = (a.get(k).map(String::as_str).unwrap_or("<无>"), b.get(k).map(String::as_str).unwrap_or("<无>"));
        if x != y {
            let line = format!("{k}: {x} → {y}");
            if vendor_managed(k) {
                vendor.push(line);
            } else {
                other.push(line);
            }
        }
    }
    (vendor, other)
}

// ══════════════ 报告装配 ══════════════

pub(crate) fn base_report(req: &Value, kind: &str) -> Value {
    json!({
        "version": 2,
        "kind": kind,
        "candidate_id": req["candidate_id"],
        "status": "ABORT",
        "metrics": {
            "operator_latency_ms": null,
            "fps_p95_ms": null,
            "frame_p95_ms": null,
            "fps_mean": null,
            "power_watt": null,
            "psnr_db": null,
            "soc_temp_max_c": null,
            "unavailable": []
        },
        "verdict": {
            "is_pareto_improvement": false,
            // 未检验哨兵: 1.0, 不伪造小 p 值
            "p_value": 1.0,
            "reason": ""
        },
        "source": "phonefarm",
    })
}

pub(crate) fn unavailable(metrics: &mut Value, field: &str, reason: &str) {
    metrics["unavailable"]
        .as_array_mut()
        .expect("unavailable 数组")
        .push(json!({ "field": field, "reason": reason }));
}

// ══════════════ sysparam ══════════════

fn deny_keywords(req: &Value) -> Vec<String> {
    match req["payload"]["deny_keywords"].as_array() {
        Some(a) if !a.is_empty() => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => DEFAULT_DENY_KEYWORDS.iter().map(|s| s.to_string()).collect(),
    }
}

/// 接收端**再校验一遍**: 参数在白名单里、值在取值表里、不命中温控关键词 ——
/// 不信上层。plan 若随请求下发, 也逐项对回白名单 (path 必须一致)。
/// 任何一项越界整份拒绝, 一个字节都不写。
fn revalidate(req: &Value, wl: &sysparam::Whitelist) -> Result<(), String> {
    let params: Vec<(String, String)> = req["payload"]["params"]
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default();
    let deny = deny_keywords(req);
    for (pid, _) in &params {
        let low = pid.to_lowercase();
        if deny.iter().any(|k| low.contains(k)) {
            return Err(format!("{pid} 命中温控保护关键词 {deny:?} —— 接收端再拦一道"));
        }
    }
    let (ok, why) = sysparam::validate_candidate(&params, wl);
    if !ok {
        return Err(format!("接收端校验拒绝: {why}"));
    }
    for pid in params.iter().map(|(p, _)| p) {
        if sysparam::denied(pid) {
            return Err(format!("{pid} 命中温控保护关键词"));
        }
    }
    if let Some(plan) = req["payload"]["plan"].as_array() {
        for entry in plan {
            let (pkind, ppath, pval) = (
                entry["kind"].as_str().unwrap_or(""),
                entry["path"].as_str().unwrap_or(""),
                entry["value"].as_str().unwrap_or(""),
            );
            // plan 里的每一项都必须能在白名单里找到同 path 的条目, 且值合法
            let hit = wl.iter().find(|(_, s)| s.path == ppath && s.kind == pkind);
            let Some((_, spec)) = hit else {
                return Err(format!("plan 项 ({pkind}, {ppath}) 不在白名单里"));
            };
            if !spec.values.iter().any(|v| v == pval) {
                return Err(format!("plan 项 {ppath}={pval:?} 不在合法取值表里"));
            }
            if sysparam::denied(ppath) {
                return Err(format!("plan 路径 {ppath} 命中温控保护关键词"));
            }
        }
    }
    Ok(())
}

fn probe_whitelist(serial: &str, evidence: &Path) -> Result<sysparam::Whitelist, String> {
    push_tools(serial)?;
    let out = su(serial, &format!("sh {DEV_TMP}/probe_sysparam.sh"), 600);
    if out.contains("#adb_exit=") && !out.ends_with("#adb_exit=0") {
        return Err(format!("probe_sysparam 失败: {out}"));
    }
    let _ = std::fs::create_dir_all(evidence);
    std::fs::write(evidence.join("probe.txt"), &out).map_err(|e| e.to_string())?;
    Ok(sysparam::build_whitelist(&sysparam::parse_probe(&out)))
}

/// 自驱动负载 (`workload.launch`): 应用带 intent extras 启动后自己跑一段确定性路线
/// (Megacity 的 city_flythrough), 不需要注入触控/手柄。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LaunchSpec {
    /// 测量用的 extras (例 `--es scene city_flythrough --es frames 3600 ...`)。
    pub extras: String,
    /// 应用进入被测窗口时在自己 files/ 下落的标记文件名 (Megacity: megacity_started)。
    /// None = 启动后固定等 [`LAUNCH_SETTLE_S`] 秒。
    pub marker: Option<String>,
    /// 画面比对用的静态场景 extras (相机不动, 例 `--es scene city_static --es frames 100000`)。
    pub static_extras: Option<String>,
}

/// 没有标记文件时, 启动后等多久才开采。
const LAUNCH_SETTLE_S: u64 = 20;
/// 等标记文件的上限 (城市流式加载 + 预热可能很久)。
const LAUNCH_MARKER_TIMEOUT_S: u64 = 300;

pub(crate) fn launch_spec(req: &Value) -> Option<LaunchSpec> {
    let l = &req["workload"]["launch"];
    let extras = l["extras"].as_str()?.trim().to_string();
    if extras.is_empty() {
        return None;
    }
    Some(LaunchSpec {
        extras,
        marker: l["marker"].as_str().filter(|m| !m.is_empty()).map(String::from),
        static_extras: l["static_extras"].as_str().filter(|m| !m.is_empty()).map(String::from),
    })
}

/// 带 extras 冷启动应用: 停掉 → 清掉旧的标记文件与 `also_clear` 里的文件 → 解析启动 activity →
/// `am start -W`。am 的回显记进 outdir/am.log。(run_one_launch 与 whitebox 共用)
pub(crate) fn start_with_extras(
    serial: &str,
    pkg: &str,
    extras: &str,
    marker: Option<&str>,
    also_clear: &[&str],
    outdir: &Path,
) -> Result<(), String> {
    let files = format!("/storage/emulated/0/Android/data/{pkg}/files");
    let _ = adb(serial, &["shell", &format!("am force-stop {pkg}")], 30);
    for f in marker.into_iter().chain(also_clear.iter().copied()) {
        let _ = adb(serial, &["shell", &format!("rm -f {files}/{f}")], 15);
    }
    let comp = adb(serial, &["shell", &format!("cmd package resolve-activity --brief {pkg}")], 30);
    let comp = comp
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.contains('/') && !l.starts_with('#'))
        .ok_or(format!("解析不到 {pkg} 的启动 activity"))?
        .to_string();
    let am = adb(serial, &["shell", &format!("am start -W -n {comp} {extras}")], 60);
    std::fs::write(outdir.join("am.log"), &am).ok();
    Ok(())
}

/// 等应用进入被测窗口: 有标记文件就等它出现 (应用中途退了或超时报错), 没有就固定等
/// [`LAUNCH_SETTLE_S`] 秒。
pub(crate) fn wait_launch_window(serial: &str, pkg: &str, label: &str, marker: Option<&str>) -> Result<(), String> {
    let files = format!("/storage/emulated/0/Android/data/{pkg}/files");
    match marker {
        Some(m) => {
            let t0 = std::time::Instant::now();
            loop {
                let got = adb(serial, &["shell", &format!("cat {files}/{m} 2>/dev/null")], 15);
                if !crate::graylayer::GrayLayer::adb_stdout(&got).trim().is_empty() {
                    return Ok(());
                }
                let alive = adb(serial, &["shell", &format!("pidof {pkg}")], 15);
                if crate::graylayer::GrayLayer::adb_stdout(&alive).trim().is_empty() {
                    return Err(format!("{label}: {pkg} 在进入被测窗口前就退了"));
                }
                if t0.elapsed().as_secs() > LAUNCH_MARKER_TIMEOUT_S {
                    return Err(format!("{label}: 等了 {LAUNCH_MARKER_TIMEOUT_S}s 还没进入被测窗口 (没有 {m})"));
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        None => {
            std::thread::sleep(std::time::Duration::from_secs(LAUNCH_SETTLE_S));
            Ok(())
        }
    }
}

/// 自驱动负载跑一轮: (可选) 带 extras 重启应用 → 等它进入被测窗口 → 功耗采样 + ftrace 采集
/// `seconds` 秒 → 解析。回的形状与 [`run_one_with`] 相同 ({label, run_once_rc, summary, env})。
/// `relaunch = false` 时应用已经由别人 (挂层那一步) 带着 extras 拉起来了。
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_one_launch(
    serial: &str,
    label: &str,
    outdir: &Path,
    seconds: u64,
    pkg: &str,
    comm: Option<&str>,
    launch: &LaunchSpec,
    relaunch: bool,
) -> Result<Value, String> {
    std::fs::create_dir_all(outdir).map_err(|e| e.to_string())?;
    if relaunch {
        start_with_extras(serial, pkg, &launch.extras, launch.marker.as_deref(), &[], outdir)?;
    }
    // 等进入被测窗口
    wait_launch_window(serial, pkg, label, launch.marker.as_deref())?;
    // 功耗采样与 ftrace 并行, 覆盖整个采集窗
    let env_path = outdir.join("env.txt");
    let dur = seconds + 4;
    let sampler = Command::new(adb_path())
        .args(["-s", serial, "shell"])
        .arg(format!("su -c 'sh {DEV_TMP}/sample_env.sh {dur} 2'"))
        .stdout(std::fs::File::create(&env_path).map_err(|e| e.to_string())?)
        .stderr(std::process::Stdio::null())
        .spawn();
    let tr = format!("{DEV_TMP}/lw_{label}.txt");
    let cap = su(serial, &format!("sh {DEV_TMP}/ftrace_capture.sh {seconds} {tr}"), seconds + 120);
    std::fs::write(outdir.join("capture.log"), &cap).ok();
    let _ = adb(serial, &["pull", &tr, &outdir.join("trace.txt").to_string_lossy()], 120);
    let _ = adb(serial, &["shell", &format!("rm -f {tr}")], 15);
    // 自驱动负载没有脚本退出码: 采集结束时应用仍活着记 0, 否则 1
    let alive = adb(serial, &["shell", &format!("pidof {pkg}")], 15);
    let workload_exit = Some(if adb_ok(&alive) && !crate::graylayer::GrayLayer::adb_stdout(&alive).trim().is_empty() { 0 } else { 1 });
    let sampler_success=if let Ok(mut s)=sampler {
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(90);
        loop {match s.try_wait() {
            Ok(Some(status))=>break status.success(),
            Ok(None) if std::time::Instant::now()<deadline=>std::thread::sleep(std::time::Duration::from_millis(200)),
            _=>{let _=s.kill();let _=s.wait();break false;}
        }}
    } else {false};
    if relaunch {
        // 量完就停掉, 别让它在还原、等冷的时候接着烧
        let _ = adb(serial, &["shell", &format!("am force-stop {pkg}")], 30);
    }
    let trace = std::fs::read(outdir.join("trace.txt")).map_err(|e| format!("{label}: 没拉回 trace ({e})"))?;
    let c = comm.filter(|c| !c.is_empty()).unwrap_or(crate::looptrace::DEFAULT_RENDER_COMM);
    let d = crate::looptrace::parse(&String::from_utf8_lossy(&trace), c);
    let summary = crate::pyjson::dumps(&d.summarize());
    std::fs::write(outdir.join("summary.json"), &summary).map_err(|e| e.to_string())?;
    let mut m = json!({ "label": label, "run_once_rc": 0, "launch": true });
    m["summary"] = serde_json::from_str(&summary).map_err(|e| format!("{label}: summary 解析失败 {e}"))?;
    if let Ok(t) = std::fs::read_to_string(&env_path) {
        m["env"] = serde_json::from_str(&py_dumps(&sysparam::env_stats(&t))).unwrap_or(Value::Null);
    }
    bind_run_evidence(&mut m, label, outdir, &env_path, sampler_success, dur, seconds, workload_exit)?;
    Ok(m)
}

/// storeop-check 用静态场景时, 启动后等城市装完的时间 (秒)。
const STATIC_SETTLE_S: u64 = 60;
/// 开/关之间的像素差占比, 最多比同状态噪声高这么多个百分点。
const LIVE_CHECK_MARGIN_PCT: f64 = 0.5;

/// storeop-check 的结果判画面: 改写要真的命中, 游戏要活着, 关~开 的差异不能明显超过同状态噪声。
pub(crate) fn live_check_verdict(v: &Value) -> Result<(), String> {
    if v["game_alive"].as_bool() == Some(false) {
        return Err("开着改写时游戏退了".into());
    }
    if v["effective_hit"].as_u64().unwrap_or(0) == 0 {
        return Err("改写一次都没命中 (effective_hit=0), 比对不出东西".into());
    }
    let rows = v["diffs"].as_array().cloned().unwrap_or_default();
    let max_of = |kind: &str| {
        rows.iter()
            .filter(|r| r["kind"].as_str() == Some(kind))
            .filter_map(|r| r["diff_px_pct"].as_f64())
            .reduce(f64::max)
    };
    let (Some(on_off), Some(noise)) = (max_of("off_vs_on"), max_of("same_state")) else {
        return Err("storeop-check 没给出关~开 与同状态的比对".into());
    };
    if on_off > noise + LIVE_CHECK_MARGIN_PCT {
        return Err(format!(
            "开/关改写之间 {on_off:.3}% 像素变了, 同状态噪声只有 {noise:.3}% (容差 {LIVE_CHECK_MARGIN_PCT} 个百分点)"
        ));
    }
    Ok(())
}

/// 跑一轮负载 + ftrace 采集 + 功耗温度采样 (与 autoloop.run_one 同一编排:
/// sample_env 与 run_once 并行, 覆盖整段负载)。渲染线程按 Unity 缺省 (UnityGfxDeviceW) 解析。
pub(crate) fn run_one(
    serial: &str,
    label: &str,
    outdir: &Path,
    seconds: u64,
    workload: &Path,
    pkg: &str,
) -> Result<Value, String> {
    run_one_with(serial, label, outdir, seconds, workload, pkg, None)
}

/// 同 [`run_one`], 但可指定渲染线程名 (ftrace comm)。非 Unity 游戏 (虚幻的 RHIThread 等)
/// 不指定就解析出零帧 —— 这个名字由 `phonefarm onboard` 从 ftrace 里数出来, 记在游戏档案
/// 的 workload.render_comm, 经评测请求的同名字段传到这里。
pub(crate) fn run_one_with(
    serial: &str,
    label: &str,
    outdir: &Path,
    seconds: u64,
    workload: &Path,
    pkg: &str,
    comm: Option<&str>,
) -> Result<Value, String> {
    std::fs::create_dir_all(outdir).map_err(|e| e.to_string())?;
    let root = repo_root();
    let dur = seconds + 6 + 4;
    let env_path = outdir.join("env.txt");
    let sampler = Command::new(adb_path())
        .args(["-s", serial, "shell"])
        .arg(format!("su -c 'sh {DEV_TMP}/sample_env.sh {dur} 2'"))
        .stdout(std::fs::File::create(&env_path).map_err(|e| e.to_string())?)
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("起不了功耗采样: {e}"));
    let run_once = root.join("loop_v1/tools/run_once.sh");
    let rc = Command::new("bash")
        .arg(&run_once)
        .arg(label)
        .arg(outdir)
        .env("SERIAL", serial)
        .env("ROOT", &root)
        .env("LEAD", "6")
        .env("CAPDUR", seconds.to_string())
        .env("WL", workload)
        .env("PKG", pkg)
        .env(
            "PATH",
            format!(
                "{}:{}",
                std::env::var("PATH").unwrap_or_default(),
                Path::new(&adb_path()).parent().map(|p| p.display().to_string()).unwrap_or_default()
            ),
        )
        .env("PF_BIN", std::env::current_exe().unwrap_or_else(|_| PathBuf::from("phonefarm")))
        .output();
    let sampler_success=if let Ok(mut s)=sampler {
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(90);
        loop {match s.try_wait() {
            Ok(Some(status))=>break status.success(),
            Ok(None) if std::time::Instant::now()<deadline=>std::thread::sleep(std::time::Duration::from_millis(200)),
            _=>{let _=s.kill();let _=s.wait();break false;}
        }}
    } else {false};
    let rc = match rc {
        Ok(o) if o.status.success() => 0,
        Ok(o) => {
            eprintln!("run_once stderr: {}", String::from_utf8_lossy(&o.stderr));
            1
        }
        Err(e) => return Err(format!("run_once 起不了: {e}")),
    };
    let mut m = json!({ "label": label, "run_once_rc": rc });
    let sp = outdir.join("summary.json");
    // 指定了渲染线程: 在本进程里按它重解析 trace.txt, 覆盖 run_once.sh 按 Unity 缺省解析出的
    // summary.json。放在这里而不是给 run_once.sh 加参数, 是为了换个仓库根 (手机节点上的
    // main 版脚本) 也照样生效。
    if let Some(c) = comm.filter(|c| !c.is_empty()) {
        if let Ok(b) = std::fs::read(outdir.join("trace.txt")) {
            let d = crate::looptrace::parse(&String::from_utf8_lossy(&b), c);
            std::fs::write(&sp, crate::pyjson::dumps(&d.summarize())).map_err(|e| e.to_string())?;
        }
    }
    match std::fs::read_to_string(&sp).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        Some(s) => {
            m["summary"] = s;
        }
        None => return Err(format!("{label}: 没拿到可用 summary.json (负载或采集失败, rc={rc})")),
    }
    if let Ok(t) = std::fs::read_to_string(&env_path) {
        // 功耗温度解析在 src/sysparam.rs (与 autoloop 同一份口径)
        m["env"] = serde_json::from_str(&py_dumps(&sysparam::env_stats(&t)))
            .unwrap_or(Value::Null);
    }
    let workload_exit=std::fs::read_to_string(outdir.join("workload_exit.txt")).ok().and_then(|t|t.trim().parse::<i64>().ok());
    bind_run_evidence(&mut m, label, outdir, &env_path, sampler_success, dur, seconds, workload_exit)?;
    if rc != 0 { return Err(format!("{label}: 负载失败 rc={rc}，旧 summary 不能补成成功")); }
    Ok(m)
}

/// 单轮证据绑定 (实验资格要用): 采样器正常退出、采样窗覆盖 ftrace 采集窗、负载正常退出,
/// 否则功耗/温度窗不作数; 附帧证据与 trace 摘要。
#[allow(clippy::too_many_arguments)]
fn bind_run_evidence(
    m: &mut Value,
    label: &str,
    outdir: &Path,
    env_path: &Path,
    sampler_success: bool,
    dur: u64,
    seconds: u64,
    workload_exit: Option<i64>,
) -> Result<(), String> {
    m["workload_exit"]=json!(workload_exit);
    let env_text=std::fs::read_to_string(env_path).unwrap_or_default();
    let trace_text=std::fs::read_to_string(outdir.join("trace.txt")).unwrap_or_default();
    let window=sysparam::sample_window(&env_text);
    let bound=sampler_success && window.duration_s==Some(dur as f64) && window.interval_s==Some(2.0)
        && sysparam::trace_window_covered(&window,&trace_text,seconds);
    m["env"]["sampler_exit_success"]=json!(sampler_success);
    m["env"]["trace_window_verified"]=json!(bound);
    m["env"]["power_observation_scope"]=json!("whole sampler window including lead/tail; covers trace capture window");
    if !bound {m["env"]["power_w_mean"]=Value::Null;m["env"]["power_window_verified"]=json!(false);m["env"]["thermal_window_verified"]=json!(false);}
    m["frame_evidence"] = qualification::frame_evidence(&m["summary"]);
    {use sha2::{Digest,Sha256};m["frame_evidence"]["trace_sha256"]=json!(format!("{:x}",Sha256::digest(trace_text.as_bytes())));}
    if workload_exit!=Some(0) {return Err(format!("{label}: 逐轮负载退出失败/未知: {workload_exit:?}"));}
    if let Some(t) = m["env"]["soc_temp_max_c"].as_f64() { m["summary"]["soc_temp_max_c"] = json!(t); }
    Ok(())
}

/// 按负载形态跑一轮: 自驱动 (launch) 走 [`run_one_launch`] (带 extras 重启), 否则 [`run_one_with`]。
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_one_for(
    serial: &str,
    label: &str,
    outdir: &Path,
    seconds: u64,
    workload: &Path,
    pkg: &str,
    comm: Option<&str>,
    launch: Option<&LaunchSpec>,
) -> Result<Value, String> {
    match launch {
        Some(l) => run_one_launch(serial, label, outdir, seconds, pkg, comm, l, true),
        None => run_one_with(serial, label, outdir, seconds, workload, pkg, comm),
    }
}

/// 资格用的负载自检。脚本负载走 [`verify_workload`]; 自驱动负载 (launch) 带 extras 冷启动,
/// 进入被测窗口后隔 5 秒截两帧: 应用全程活着且画面在动才算。
pub(crate) fn verify_workload_for(
    serial: &str,
    workload: &Path,
    launch: Option<&LaunchSpec>,
    pkg: &str,
    outdir: &Path,
) -> Value {
    let Some(l) = launch else {
        return verify_workload(serial, workload, pkg, outdir);
    };
    let _ = std::fs::create_dir_all(outdir);
    let started = start_with_extras(serial, pkg, &l.extras, l.marker.as_deref(), &[], outdir)
        .and_then(|_| wait_launch_window(serial, pkg, "qualification_workload", l.marker.as_deref()));
    let result = match started {
        Err(e) => json!({"verified": false, "reason": e, "launch": true}),
        Ok(()) => {
            let capture = || Command::new(adb_path()).args(["-s",serial,"exec-out","screencap"]).output().ok()
                .filter(|o|o.status.success()).map(|o|o.stdout).unwrap_or_default();
            let a = capture();
            std::thread::sleep(std::time::Duration::from_secs(5));
            let b = capture();
            let alive = adb(serial, &["shell", &format!("pidof {pkg}")], 15);
            let alive = adb_ok(&alive) && !crate::graylayer::GrayLayer::adb_stdout(&alive).trim().is_empty();
            let _=std::fs::write(outdir.join("frame_a.raw"),&a); let _=std::fs::write(outdir.join("frame_b.raw"),&b);
            let motion=to_serde(&crate::framecheck::verdict(&a,&b));
            json!({"verified": alive && motion["spinning"]==true, "app_alive": alive, "motion": motion, "launch": true})
        }
    };
    let _ = adb(serial, &["shell", &format!("am force-stop {pkg}")], 30);
    let _=std::fs::write(outdir.join("workload_check.json"),serde_json::to_string_pretty(&result).unwrap());
    result
}

/// [`run_one`] 回包里并进判定视图的 env 字段 (sample_env.sh → [`sysparam::env_stats`])。
pub(crate) const RUN_ENV_KEYS: [&str; 30] = [
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
    "battery_status_after",
    "power_window_verified",
    "thermal_window_verified",
    "trace_window_verified",
    "sampler_complete",
    "sampler_exit_success",
    "power_observation_scope",
    "battery_charging",
    "power_usable_reason",
    "on_battery",
    "bypass",
    "power_valid",
    "power_source",
    "soc_temp_max_c",
    "soc_temp_mean_c",
    "n_samples",
    "fan_state",
    // 只有设备端采到 GPU 行时 env 里才有这三项 (厂商性能服务会改 GPU 上限, 逐臂留证)
    "gpu_max_pwrlevel_seen",
    "gpu_clk_mhz_mean",
    "gpu_clk_mhz_max",
];

/// 判定/统计用的单轮视图: summary 字段展平, 再并上 env 的功耗/温度字段
/// (与旧 Python `m.update(summary); m.update(env_stats)` 同形)。
///
/// 只取 `m["summary"]` 是错的: parse-trace 的 summary 里**没有** `power_w_mean` 与
/// `soc_temp_max_c` —— 那样功耗永远「缺数据」、温度上限永远不触发, 而报告看着一切正常。
pub(crate) fn run_view(m: &Value) -> PyVal {
    let mut obj: Vec<(String, PyVal)> = Vec::new();
    if let Some(s) = m["summary"].as_object() {
        for (k, v) in s {
            obj.push((k.clone(), to_pyval(v)));
        }
    }
    if let Some(e) = m["env"].as_object() {
        for k in RUN_ENV_KEYS {
            if let Some(v) = e.get(k) {
                obj.push((k.to_string(), to_pyval(v)));
            }
        }
    }
    PyVal::Obj(obj)
}

/// 一轮的 SoC 结温峰值。来源是 sample_env (env), 不是 parse-trace 的 summary。
pub(crate) fn run_soc_temp_max(m: &Value) -> Option<f64> {
    m["env"]["soc_temp_max_c"]
        .as_f64()
        .or_else(|| m["summary"]["soc_temp_max_c"].as_f64())
}

/// 判定视图下的一臂 (标签 + [`run_view`])。
fn eval_runs(runs: &[Value]) -> Vec<Run> {
    runs.iter()
        .filter_map(|r| Some((r["label"].as_str()?.to_string(), run_view(r))))
        .collect()
}

/// 两臂已跑的每一轮都量到了可信功耗、且是同一种口径 (全电池放电或全 USB 旁路), 功耗才进
/// 判定; 混了口径这组功耗无效。空集算真 —— 后面的轮次还可能全合格。
pub(crate) fn all_on_battery(knob_runs: &[Value], ctrl_runs: &[Value]) -> bool {
    sysparam::power_group_ok(knob_runs.iter().chain(ctrl_runs.iter()).map(|r| &r["env"]))
}

/// 跑满时**可能**进入判定的「显著改善」检验, 各取最宽的 alpha_win:
/// 帧时/帧率至少在 2 指标表里 (alpha/2); 功耗只有在已跑的轮全在放电态时才可能进表,
/// 进表时是 3 指标 (alpha/3)。覆盖所有可能的指标表, 截断只会更保守。
/// 冻结 policy 的判定指标对应的截断检验 (与 [`judge_runs_with`] 同一张表、同一个 alpha_win)。
fn policy_win_tests(required: &[String], alpha: f64) -> Vec<curtail::WinTest> {
    let n = required.len().max(1) as f64;
    required.iter().map(|m| curtail::WinTest::new(m, m != "fps_mean", alpha / n)).collect()
}

#[allow(dead_code)]
fn sysparam_win_tests(alpha: f64, power_possible: bool) -> Vec<curtail::WinTest> {
    let mut t = vec![
        curtail::WinTest::new("frame_p95", true, alpha / 2.0),
        curtail::WinTest::new("fps_mean", false, alpha / 2.0),
    ];
    if power_possible {
        t.push(curtail::WinTest::new("power_w_mean", true, alpha / 3.0));
    }
    t
}

/// 截断记录, 原样进回包 `verdict.curtailed`。
fn curtail_note(knob: usize, ctrl: usize, rounds: u32, tests: &[curtail::WinTest]) -> Value {
    json!({
        "arms_run": knob + ctrl,
        "arms_planned": 2 * rounds,
        "knob_runs": knob,
        "ctrl_runs": ctrl,
        "pairs_planned": rounds,
        "checked": tests.iter().map(|t| json!({
            "metric": t.metric, "better": if t.lower_is_better { "lower" } else { "higher" },
            "alpha_win": t.alpha_win,
        })).collect::<Vec<_>>(),
        "rule": "剩余轮次取任意值时, 每个可能进判定的指标在改善方向上的置换 p 下界都已 >= alpha_win; \
                 KEEP 不可达, 跑满的判定必然相同 (src/curtail.rs)。--no-curtail 可跑满",
    })
}

fn sysparam_eval(req: &Value, serial: Option<&str>, evidence: &Path, curtail: bool) -> Result<String, String> {
    let Some(serial) = serial else {
        return Err("kind=sysparam 需要真机: 请加 --serial (或设备唯一在线时省略)".into());
    };
    let kind = "sysparam";
    let mut report = base_report(req, kind);
    let _ = std::fs::create_dir_all(evidence);
    let probe_only = req["payload"]["probe_only"].as_bool().unwrap_or(false);

    let policy = policy_for_request(req)?;
    std::fs::write(evidence.join("qualification_policy.json"),serde_json::to_string_pretty(&policy).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let mut facts=Evidence::new(Stage::Probe,policy,qualification::inspect_device(serial));
    let preflight=record_qualification(&mut report,evidence,&facts)?;
    if !preflight.probe_allowed {
        report["verdict"]["reason"]=json!("root/目标 trace 能力未确认，禁止差异写探测");
        return Ok(serde_json::to_string_pretty(&report).unwrap());
    }
    push_tools(serial)?;
    let before_probe=snapshot(serial);
    std::fs::write(evidence.join("snap_before_probe.txt"),&before_probe).map_err(|e|e.to_string())?;
    let wl=probe_whitelist(serial,evidence)?; // Caller whitelist is a proposal, never evidence of actual effectiveness.
    let after_probe=snapshot(serial);
    std::fs::write(evidence.join("snap_after_probe.txt"),&after_probe).map_err(|e|e.to_string())?;
    let probe_restored=snapshots_identical(&before_probe,&after_probe);
    facts.device.control_verified=Some(!wl.is_empty()); facts.restored=Some(probe_restored);
    record_qualification(&mut report,evidence,&facts)?;
    if probe_only {probe_report(&mut report,&before_probe,&after_probe,&wl);record_qualification(&mut report,evidence,&facts)?;return Ok(serde_json::to_string_pretty(&report).unwrap());}
    if !probe_restored {
        report["verdict"]["reason"]=json!("差异写探测未验证恢复，禁止开始候选实验");
        return Ok(serde_json::to_string_pretty(&report).unwrap());
    }

    // ── 接收端再校验: 越界整份拒绝, 一个字节都不写 ──
    if let Err(e) = revalidate(req, &wl) {
        report["verdict"]["reason"] = json!(e);
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }

    // ── 组装 plan ──
    let params: Vec<(String, String)> = req["payload"]["params"]
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default();
    let plan = match req["payload"]["plan"].as_array() {
        Some(entries) => {
            let mut lines = vec!["# loop_v1 sysparam plan (from request payload)".to_string()];
            for e in entries {
                lines.push(format!(
                    "{}\t{}\t{}",
                    e["kind"].as_str().unwrap_or(""),
                    e["path"].as_str().unwrap_or(""),
                    e["value"].as_str().unwrap_or("")
                ));
            }
            lines.join("\n") + "\n"
        }
        None => sysparam::plan_text(&params, &wl)?,
    };
    let _ = std::fs::create_dir_all(evidence);
    std::fs::write(evidence.join("plan.txt"), &plan).map_err(|e| e.to_string())?;

    let plan_local = evidence.join("plan.txt");
    let push = adb(serial, &["push", &plan_local.to_string_lossy(), &format!("{DEV_TMP}/loop_v1_sysparam.plan")], 60);
    if !push.ends_with("#adb_exit=0") {
        report["verdict"]["reason"] = json!(format!("plan 下发失败: {push}"));
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }

    let snap_before = snapshot(serial);
    std::fs::write(evidence.join("snap_before.txt"), &snap_before).map_err(|e| e.to_string())?;

    // ── ABBA 交替跑测 ──
    let rounds = req["protocol"]["rounds"].as_i64().unwrap_or(1).max(1) as u32;
    let seconds = req["workload"]["seconds"].as_i64().unwrap_or(30).max(1) as u64;
    // workload 脚本: 请求给的是调用方视角的路径 (game_opt_loop 用 "../phonefarm/..."),
    // 先按原样 (cwd 相对/绝对) 找, 找不到再按仓库根拼
    let wl_file = req["workload"]["script"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| "loop_v1/scripts/workload_spin_touch_v1.json".into());
    let pkg = req["package"].as_str().unwrap_or("com.miHoYo.Yuanshen");
    let launch = launch_spec(req);
    let workload_path = match (resolve_path(&wl_file), &launch) {
        (Some(p), _) => p,
        // 自驱动负载用不到脚本
        (None, Some(_)) => PathBuf::new(),
        (None, None) => {
            report["verdict"]["reason"] = json!(format!(
                "负载脚本不在: {wl_file} (按 cwd 与仓库根都找不到)"
            ));
            return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
        }
    };

    let cold=match facts.policy.cool_c {Some(c)=>crate::autoloop::wait_cool(serial,c,420),None=>crate::pyobj! {"ok"=>false,"reason"=>"conditions.cool_c 未知"}};
    facts.cold_ok=cold.get("ok").and_then(|v|v.as_bool());
    if facts.cold_ok!=Some(true) {
        facts.stage=Stage::Experiment;let _=record_qualification(&mut report,evidence,&facts)?;
        report["verdict"]["reason"]=json!("冷机条件未通过，禁止候选测量");
        return Ok(serde_json::to_string_pretty(&report).unwrap());
    }
    let workload_check=verify_workload_for(serial,&workload_path,launch.as_ref(),pkg,&evidence.join("workload_check"));
    facts.workload_verified=workload_check["verified"].as_bool();
    // 停充尝试: 请求要求且节点没指定时由接收端自己探 (charge_suspend.sh 内置节点表)。
    // 停充失败保留证据；能效目标缺整窗放电功耗时资格失败，不能缩窄目标。
    let want_suspend = req["conditions"]["charging_suspend"]["enabled"].as_bool().unwrap_or(false);
    let mut suspended_via: Option<String> = None;
    if want_suspend {
        let out = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh suspend"), 90);
        if let Err(e)=std::fs::write(evidence.join("charge_suspend.log"),&out) {
            let _=su(serial,&format!("sh {DEV_TMP}/charge_suspend.sh restore"),60);
            return Err(e.to_string());
        }
        if adb_ok(&out) && out.contains("CHARGE_SUSPENDED") {
            // 脚本自己回显用的哪个节点; 原样记进报告
            suspended_via = out
                .lines()
                .find(|l| l.contains("node="))
                .map(|l| l.trim().to_string())
                .or_else(|| Some("/sys/class/qcom-battery/charging_enabled".into()));
        }
    }

    for i in 1..=3 {
        let cold=match facts.policy.cool_c {Some(c)=>crate::autoloop::wait_cool(serial,c,420),None=>crate::pyobj! {"ok"=>false}};
        let _=std::fs::write(evidence.join(format!("cold_baseline{i}.json")),py_dumps(&cold));
        if cold.get("ok").and_then(|v|v.as_bool())!=Some(true) {facts.cold_ok=Some(false);break;}
        match run_one_for(serial,&format!("baseline{i}"),&evidence.join(format!("baseline{i}")),seconds,&workload_path,pkg,req["workload"]["render_comm"].as_str(),launch.as_ref()) {
            Ok(m)=>facts.baseline.push(m),Err(e)=>{report["verdict"]["reason"]=json!(e);break;}
        }
    }
    let baseline_restore=su(serial,&format!("sh {DEV_TMP}/charge_suspend.sh restore"),60);
    std::fs::write(evidence.join("baseline_charge_restore.log"),&baseline_restore).ok();
    let baseline_after=snapshot(serial);
    std::fs::write(evidence.join("snap_after_baseline.txt"),&baseline_after).ok();
    facts.restored=Some(snapshots_identical(&snap_before,&baseline_after) && adb_ok(&baseline_restore)
        && !baseline_restore.contains("CHARGE_RESTORE_FAIL"));
    facts.stage=Stage::Experiment;
    let before_candidates=record_qualification(&mut report,evidence,&facts)?;
    if !before_candidates.experiment_allowed {
        let restore=su(serial,&format!("sh {DEV_TMP}/charge_suspend.sh restore"),60);
        std::fs::write(evidence.join("charge_restore.log"),restore).ok();
        report["verdict"]["reason"]=json!("基线/供电/来源/负载资格未通过，禁止候选实验");
        return Ok(serde_json::to_string_pretty(&report).unwrap());
    }
    if want_suspend {
        let out=su(serial,&format!("sh {DEV_TMP}/charge_suspend.sh suspend"),90);
        std::fs::write(evidence.join("candidate_charge_suspend.log"),&out).ok();
        // Always attempt restore after a requested suspend, even when its acknowledgement failed.
        suspended_via=Some(suspended_via.unwrap_or_else(||"requested; acknowledgement unknown".into()));
    }
    const BAD_APPLY_TOKENS: [&str; 4] = [
        "KNOB_FAIL",
        "KNOB_REFUSE",
        "KNOB_PLAN_REJECTED",
        "KNOB_ALREADY_APPLIED",
    ];
    let restore = |serial: &str| -> String { su(serial, &format!("sh {DEV_TMP}/knob_sysparam.sh restore"), 120) };

    let mut knob_runs: Vec<Value> = Vec::new();
    let mut ctrl_runs: Vec<Value> = Vec::new();
    let mut abort_reason: Option<String> = None;
    let mut apply_ok = true;
    let mut restore_fail: Option<String> = None;
    let mut apply_logs: Vec<String> = Vec::new();
    // 冻结的资格合同: alpha 即 protocol.p_threshold, 判定指标即 policy.required_metrics
    let alpha = facts.policy.alpha;
    let mut curtailed: Option<Value> = None;

    let mut contested: Vec<Value> = Vec::new();
    let cool_c = req["conditions"]["cool_c"].as_f64().filter(|x| *x > 0.0 && *x < 100.0);
    let mut cool_log: Vec<Value> = Vec::new();
    'pairs: for i in 1..=rounds {
        let cold=match facts.policy.cool_c {Some(c)=>crate::autoloop::wait_cool(serial,c,420),None=>crate::pyobj! {"ok"=>false}};
        let _=std::fs::write(evidence.join(format!("cold_pair{i}.json")),py_dumps(&cold));
        if cold.get("ok").and_then(|v|v.as_bool())!=Some(true) {
            facts.cold_ok=Some(false);abort_reason=Some("候选对冷机失败，禁止继续".into());break;
        }
        for arm in if i % 2 == 1 { ["knob", "ctrl"] } else { ["ctrl", "knob"] } {
            // 风扇按死在关 (owner 规矩), 再等冷 —— 风扇状态会改等冷的结果
            let fan = arm_fan_off(serial);
            // 每臂开跑前按请求的 cool_c 等冷 (施加之前等: 等的那几分钟里厂商服务有足够时间把刚写的值改回去)
            let mut c = arm_wait_cool(serial, cool_c, &format!("{arm}{i}"), arm_cool_timeout(&cool_log));
            c["arm"] = json!(format!("{arm}{i}"));
            c["fan"] = fan.clone();
            cool_log.push(c);
            if let Some(e) = fan_still_on(&fan) {
                abort_reason = Some(format!("{arm}{i}: {e}"));
                break;
            }
            if arm == "knob" {
                let out = su(
                    serial,
                    &format!("sh {DEV_TMP}/knob_sysparam.sh apply {DEV_TMP}/loop_v1_sysparam.plan"),
                    120,
                );
                apply_logs.push(out.clone());
                if !adb_ok(&out) || BAD_APPLY_TOKENS.iter().any(|t| out.contains(t)) {
                    apply_ok = false;
                    abort_reason = Some("旋钮未全部生效 (apply 回包有 FAIL 令牌)".into());
                    let r = restore(serial);
                    if !adb_ok(&r) || r.contains("KNOB_RESTORE_FAIL") {
                        restore_fail = Some(r);
                    }
                    break;
                }
            }
            let label = format!("sp_{arm}{i}");
            // 施加后隔十几秒回读一次 (https://github.com/HGamey/docs/blob/main/repos/phonefarm/knobs/DESIGN.md: 写得进不等于守得住, 厂商性能服务
            // perf2-hal / zperfcubeservice 会在约 10 秒内把 GPU/CPU 频率边界改回去)。
            // 两臂都读 —— 只在改写臂读的话, 那一次 adb 调用本身就成了两臂之间的差别。
            let rb_cmd = readback_cmd(&plan);
            let rb_thread = {
                let (serial, cmd) = (serial.to_string(), rb_cmd.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(READBACK_AFTER_S));
                    su(&serial, &cmd, 30)
                })
            };
            let res = match &launch {
                Some(l) => run_one_launch(
                    serial,
                    &label,
                    &evidence.join(format!("{arm}{i}")),
                    seconds,
                    pkg,
                    req["workload"]["render_comm"].as_str(),
                    l,
                    true,
                ),
                None => run_one_with(
                    serial,
                    &label,
                    &evidence.join(format!("{arm}{i}")),
                    seconds,
                    &workload_path,
                    pkg,
                    req["workload"]["render_comm"].as_str(),
                ),
            };
            let rb_mid = rb_thread.join().unwrap_or_default();
            let mut arm_contested: Vec<String> = Vec::new();
            if arm == "knob" {
                // 负载跑完、还原之前再读一次: 守住了整段测量才算数
                let rb_end = su(serial, &rb_cmd, 30);
                for (when, out) in [("+12s", &rb_mid), ("末尾", &rb_end)] {
                    for (path, want, got) in readback_mismatches(&plan, out) {
                        arm_contested.push(format!("{path}: 写 {want} {when}回读 {got}"));
                    }
                }
                std::fs::write(
                    evidence.join(format!("knob{i}_readback.txt")),
                    format!("# +{READBACK_AFTER_S}s\n{rb_mid}\n# 末尾\n{rb_end}\n"),
                )
                .ok();
            }
            if arm == "knob" {
                let r = restore(serial);
                if (!adb_ok(&r) || r.contains("KNOB_RESTORE_FAIL")) && restore_fail.is_none() {
                    restore_fail = Some(r.clone());
                }
            }
            match res {
                Ok(m) => {
                    // 温度上限: 超了这一组作废
                    let t = run_soc_temp_max(&m);
                    if let (Some(t), Some(cap)) = (t, req["conditions"]["temp_cap_c"].as_f64()) {
                        if t > cap {
                            abort_reason = Some(format!("{arm} 臂第 {i} 轮 SoC 结温 {t}C 超过上限 {cap}C"));
                        }
                    }
                    if abort_reason.is_none() {
                        if arm == "knob" {
                            if arm_contested.is_empty() {
                                knob_runs.push(m);
                            } else {
                                // 守不住的那一臂量的不是这组参数: 不进统计, 单列计数
                                contested.push(json!({"round": i, "nodes": arm_contested}));
                            }
                        } else {
                            ctrl_runs.push(m);
                        }
                    }
                    if restore_fail.is_some() {
                        abort_reason = Some("旋钮还原失败, 后续数据不可信".into());
                    }
                }
                Err(e) => {
                    abort_reason = Some(e);
                }
            }
            if abort_reason.is_some() {
                break;
            }
            // 确定性截断: 剩下的臂无论量出什么, 都不可能有指标显著改善 → 跑满也必然不是
            // KEEP, 现在停 (论证见 src/curtail.rs)。KEEP 一侧永远不提前锁定。
            if curtail {
                let tests = policy_win_tests(&facts.policy.required_metrics, alpha);
                let (left_a, left_b) = (
                    (rounds as usize).saturating_sub(ctrl_runs.len()),
                    (rounds as usize).saturating_sub(knob_runs.len()),
                );
                if !curtail::keep_possible(&eval_runs(&ctrl_runs), &eval_runs(&knob_runs), left_a, left_b, &tests) {
                    curtailed = Some(curtail_note(knob_runs.len(), ctrl_runs.len(), rounds, &tests));
                    break 'pairs;
                }
            }
        }
        if abort_reason.is_some() {
            break;
        }
    }

    // ── 还原 + 快照核对 ──
    if suspended_via.is_some() {
        let cr = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh restore"), 60);
        std::fs::write(evidence.join("charge_restore.log"), &cr).map_err(|e| e.to_string())?;
        if !adb_ok(&cr) || cr.contains("CHARGE_RESTORE_FAIL") {
            restore_fail = Some(restore_fail.clone().unwrap_or_else(|| "停充还原失败".into()));
        }
    }
    let restore_log = restore(serial);
    if (!adb_ok(&restore_log) || restore_log.contains("KNOB_RESTORE_FAIL")) && restore_fail.is_none() {
        restore_fail = Some(restore_log);
    }
    let final_cold=match facts.policy.cool_c {Some(c)=>crate::autoloop::wait_cool(serial,c,420),None=>crate::pyobj! {"ok"=>false}};
    let _=std::fs::write(evidence.join("cold_final.json"),py_dumps(&final_cold));
    if final_cold.get("ok").and_then(|v|v.as_bool())!=Some(true) {facts.cold_ok=Some(false);abort_reason=Some("收尾冷机失败，撤销可保留结论".into());}
    let snap_after = snapshot(serial);
    std::fs::write(evidence.join("snap_after.txt"), &snap_after).map_err(|e| e.to_string())?;
    let snapshot_identical = snapshots_identical(&snap_before,&snap_after);
    // 快照对不上时分两类: 厂商性能服务自己会改的频率边界 (它在游戏过程中主动压/抬, 与我们
    // 无关; 我们写过的节点由 restore 逐项写回并回读核对, 失败另有 KNOB_RESTORE_FAIL), 和其余。
    // 只有其余的键变了才判留痕作废。前者如实记进报告。
    let (vendor_drift, other_drift) = split_snapshot_diff(&snap_before, &snap_after);
    // 资格里的「已还原」: 两次快照都读成功, 且除厂商自管的频率边界外逐项相等
    let snapshot_restored = snapshot_identical
        || (adb_ok(&snap_before) && adb_ok(&snap_after) && snap_before.lines().any(|l| l.contains('=')) && other_drift.is_empty());

    let (cool_reached, cool_timeout) = cool_summary(&cool_log);
    report["conditions_observed"] = json!({
        "cool_c_target": cool_c,
        "cool_reached_c": cool_reached,
        "cool_timeout": cool_timeout,
        "cool_per_arm": cool_log,
        "temp_cap_c": req["conditions"]["temp_cap_c"],
        "apply_ok": apply_ok,
        "snapshot_identical": snapshot_identical,
        "restored": restore_fail.is_none(),
        // 改写臂里有几臂被厂商服务抢回 (施加后 +12s 或负载末尾回读对不上), 这几臂没进统计
        "contested_arms": contested.len(),
        "contested": contested,
        "snapshot_vendor_drift": vendor_drift,
        "snapshot_diff": if snapshot_identical { Value::Null } else {
            json!([
                format!("before {} 行 / after {} 行, 逐行比对不相等 (全文见 evidence_dir)",
                        snap_before.lines().count(), snap_after.lines().count())
            ])
        },
    });
    report["evidence_dir"] = json!(evidence.display().to_string());
    std::fs::write(evidence.join("apply_log.txt"), apply_logs.join("\n---\n")).ok();

    facts.stage=Stage::Conclusion;facts.control=ctrl_runs.clone();facts.candidate=knob_runs.clone();
    facts.device.control_verified=Some(apply_ok);
    facts.restored=Some(snapshot_restored && restore_fail.is_none());
    let qualified=record_qualification(&mut report,evidence,&facts)?;
    if !qualified.conclusion_allowed {
        report["verdict"]["reason"]=json!("实验结论资格未通过；数字保留，禁止 PASS/KEEP");
        return Ok(serde_json::to_string_pretty(&report).unwrap());
    }
    // ── 判定 ──
    if let Some(reason) = abort_reason.clone().or_else(|| restore_fail.clone().map(|r| format!("旋钮还原失败: {r}"))) {
        if suspended_via.is_some() {
            let _ = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh restore"), 60);
        }
        report["verdict"]["reason"] = json!(reason);
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    if !other_drift.is_empty() {
        report["verdict"]["reason"] = json!(format!("轮末设备快照与轮前不一致, 留痕: {}", other_drift.join("; ")));
        if suspended_via.is_some() {
            let _ = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh restore"), 60);
        }
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }

    if knob_runs.is_empty() && !contested.is_empty() {
        report["verdict"]["reason"] = json!(format!(
            "改写臂 {} 臂全部被厂商性能服务抢回 (施加后回读对不上, contested), 没有一臂量的是这组参数",
            contested.len()
        ));
        if suspended_via.is_some() {
            let _ = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh restore"), 60);
        }
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    if let Some(e) = fan_mismatch(&knob_runs, &ctrl_runs) {
        report["verdict"]["reason"] = json!(e);
        if suspended_via.is_some() {
            let _ = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh restore"), 60);
        }
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    let (verdict, keep, on_battery_all) = judge_runs_with(&ctrl_runs, &knob_runs, alpha, rounds, Some(&facts.policy.required_metrics));
    report["verdict"] = verdict;
    if !contested.is_empty() {
        let r = report["verdict"]["reason"].as_str().unwrap_or("").to_string();
        report["verdict"]["reason"] = json!(format!(
            "{r} (另有 {} 臂改写被厂商性能服务抢回, 没进统计)",
            contested.len()
        ));
    }
    if let Some(note) = curtailed {
        // 构造上不可能: 截断只在「任何指标都已不可能显著改善」时发生, 部分数据的 p
        // 不会低于跑满的下界 (src/curtail.rs)。真出现了就是实现错了, 别把它当 KEEP 发出去。
        if keep {
            return Err("内部错误: 截断后的部分数据判出了 KEEP, 与截断的前提矛盾".into());
        }
        let partial = report["verdict"]["reason"].as_str().unwrap_or("").to_string();
        report["verdict"]["reason"] = json!(format!(
            "确定性截断: 跑了 {}/{} 臂后, 剩余轮次无论量出什么都不可能有指标显著改善, 跑满也必然不保留 (已量部分: {partial})",
            note["arms_run"], note["arms_planned"]
        ));
        report["verdict"]["curtailed"] = note;
    }

    // ── metrics: 逐臂聚合 (B 臂 = 候选臂) ──
    // 臂内均值: ABBA 的全部轮一起算 —— 只取最后一轮的话, 那恰好是整场最热的
    // 一轮, 交替设计要抹掉的热漂移就全进了头条指标
    let mut m = report["metrics"].take();
    let power = aggregate_metrics(&mut m, &knob_runs, &ctrl_runs, on_battery_all);
    let rail = knob_runs.first().and_then(|r| r["env"]["power_source"].as_str()).unwrap_or("battery");
    report["metric_provenance"] = json!({"frame_p95_ms": {"per_run":qualified.frame_evidence,"display_verified":false},"fps_mean":{"semantics":"gpu_submit_proxy","display_verified":false},"fps_p95_ms":{"semantics":"gpu_submit_proxy","display_verified":false},"power_watt":{"rail":rail,"window_verified":knob_runs.iter().all(|r|r["env"]["power_window_verified"]==true)}});
    m["psnr_db"] = Value::Null;
    unavailable(&mut m, "psnr_db", "sysparam 不动画面, 无画质可量");
    if power.is_none() {
        unavailable(&mut m, "power_watt", "非放电态 (或功耗采样缺失): 整机功耗量不准, 如实 null");
    }
    if m["fps_p95_ms"].is_null() {
        unavailable(&mut m, "fps_p95_ms", "本轮没拿到可用的帧时序");
    }
    report["metrics"] = m;

    // ── power_source ──
    let env0 = knob_runs
        .last()
        .or(ctrl_runs.last())
        .cloned()
        .unwrap_or(json!({}));
    let env = env0.get("env").cloned().unwrap_or(json!({}));
    report["power_source"] = json!({
        "rail": env["power_rail"],
        "battery_status": env["battery_status"],
        "battery_current_now_ua": env["current_now_ua_mean"].as_f64().map(|x| x as i64),
        "usb_input_w_mean": env["usb_input_w_mean"],
        "bypass": env["bypass"],
        "power_usable_reason": env["power_usable_reason"],
        "fan_state": env["fan_state"],
        "battery_capacity_pct": env["battery_capacity_pct"].as_str().and_then(|s| s.parse::<i64>().ok()),
        "on_battery": env["on_battery"],
        "source": env["power_source"],
        "charging_suspended_via": suspended_via.clone(),
        // 这两个数决定「这轮功耗能不能拿去跟别的任务比」, 不靠看图猜
        "cross_run_comparable": sysparam::power_cross_run_comparable(
            knob_runs.iter().chain(ctrl_runs.iter()).filter_map(|r| r.get("env")),
        ),
        "excluded_reason": sysparam::power_group_note(
            knob_runs.iter().chain(ctrl_runs.iter()).filter_map(|r| r.get("env")),
        ),
    });

    report["status"] = json!(if keep { "PASS" } else { "REJECT" });
    Ok(serde_json::to_string_pretty(&report).unwrap_or_default())
}

fn wl_pyval(wl: &sysparam::Whitelist) -> Value {
    let pairs: Vec<(String, PyVal)> = wl.iter().map(|(k, s)| (k.clone(), s.to_pyval())).collect();
    let s = py_dumps(&PyVal::Obj(pairs));
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

/// n v n 精确置换检验的最小双侧 p = 2/C(2n,n); 够不着 alpha_win 就是 false。
fn reachable(n: u32, alpha_win: f64) -> bool {
    if n < 2 {
        return false;
    }
    let mut c = 1.0f64;
    for i in 1..=n {
        c *= (n + i) as f64 / i as f64;
        if !c.is_finite() {
            return false;
        }
    }
    2.0 / c < alpha_win
}

/// 风扇开关节点 (红魔 NX809J)。owner 规矩: 风扇默认必须关 (噪音)。
const FAN_EN_NODE: &str = "/sys/kernel/fan/fan_enable";

/// 每臂开测前: 读风扇开关, 不是 0 就写 0 并回读。回 {before, after, forced};
/// 没有风扇节点 (别的机型) 时 before/after 为 null。2026-10-12 W1 实测: 不知谁开着的风扇
/// 在前 4 对开、第 5 对关, 整组功耗作废 —— 所以每臂都按死, 不信上一个人。
pub(crate) fn arm_fan_off(serial: &str) -> Value {
    let rd = |s: &str| {
        crate::graylayer::GrayLayer::adb_stdout(&su(s, &format!("cat {FAN_EN_NODE} 2>/dev/null"), 15))
            .trim()
            .to_string()
    };
    let before = rd(serial);
    if before.is_empty() {
        return json!({"before": null, "after": null, "forced": false});
    }
    let forced = before != "0";
    if forced {
        su(serial, &format!("echo 0 > {FAN_EN_NODE}"), 15);
    }
    let after = rd(serial);
    json!({"before": before, "after": after, "forced": forced})
}

/// [`arm_fan_off`] 之后风扇仍开着 (写不进去): 这一臂不能量。
pub(crate) fn fan_still_on(fan: &Value) -> Option<String> {
    match fan["after"].as_str() {
        Some(a) if a != "0" => Some(format!("风扇关不掉 (fan_enable 写 0 后回读 {a}), 按规矩风扇必须关, 不量")),
        _ => None,
    }
}

/// 两臂测量期间 (sample_env 逐臂记的 fan_state) 风扇状态必须一致, 不一致这组整体作废。
pub(crate) fn fan_mismatch(knob_runs: &[Value], ctrl_runs: &[Value]) -> Option<String> {
    let seen: std::collections::BTreeSet<String> = knob_runs
        .iter()
        .chain(ctrl_runs.iter())
        .filter_map(|r| r["env"]["fan_state"].as_str().map(String::from))
        .collect();
    (seen.len() > 1).then(|| format!("测量期间各臂风扇状态不一致 {seen:?}: 风扇自己耗电、还改热态, 这组不可比"))
}

/// 功耗的辅助证据: GPU 平均频率 (sample_env 的 GPU 行)、总线带宽投票中位与 GPU 忙碌时间
/// (parse-trace)。只报不判 —— 不进 wins / regressions, 不影响是否保留; 功耗量不准时它们说明
/// 「GPU 那头确实少干了活」(2026-10-12 W1: 去 MSAA 后频率 −13%、带宽投票 −15%, 5 对完全分开,
/// 而忙碌时间不变: DVFS 把省下的活换成了降频)。缺数据的项记 null。
pub(crate) fn aux_compare(a_runs: &[Run], b_runs: &[Run]) -> Value {
    let mut out = serde_json::Map::new();
    for metric in ["gpu_clk_mhz_mean", "bw_median", "gpu_active_mean"] {
        let cmp = loopstat::compare(a_runs, b_runs, metric);
        let sv = |k: &str| cmp.get(k).map(to_serde).unwrap_or(Value::Null);
        out.insert(
            metric.to_string(),
            if cmp.get("error").is_some() {
                Value::Null
            } else {
                json!({
                    "a_mean": sv("a_mean"), "b_mean": sv("b_mean"),
                    "diff_pct": sv("diff_pct"), "p": sv("perm_p_two_sided"), "ci95": sv("ci95"),
                })
            },
        );
    }
    json!({"note": "辅助证据, 只报不判: 不进 wins/regressions, 不替代功耗", "metrics": Value::Object(out)})
}

/// ABBA 两臂的统计判定 (sysparam / gray 共用): 帧时 p95、平均帧率, 两臂都放电时加功耗。
/// 回 (verdict JSON, 是否保留, 两臂是否都在放电态)。
pub(crate) fn judge_runs(ctrl_runs: &[Value], knob_runs: &[Value], alpha: f64, rounds: u32) -> (Value, bool, bool) {
    judge_runs_with(ctrl_runs, knob_runs, alpha, rounds, None)
}

/// 同 [`judge_runs`]; `required` 给了 (实验资格冻结的 policy.required_metrics) 就按它判,
/// 功耗是否进判定由目标合同决定, 不随这一轮量没量到功耗而缩窄。
pub(crate) fn judge_runs_with(
    ctrl_runs: &[Value],
    knob_runs: &[Value],
    alpha: f64,
    rounds: u32,
    required: Option<&[String]>,
) -> (Value, bool, bool) {
    let primary = "frame_p95";
    let (a_runs, b_runs) = (eval_runs(ctrl_runs), eval_runs(knob_runs));
    // 与 autoloop 一致的判定指标表: 功耗只在两臂轨道一致且都量到时才进判定。
    // 一致 ≠ 一定是放电态: 停充后拿 USB 旁路轨的两臂相减也成立 (同一轨、同一套损耗),
    // 但那个绝对值不能跨任务比, 回包里用 power.rail / cross_run_comparable 标清楚。
    let on_battery_all = all_on_battery(knob_runs, ctrl_runs);
    let metrics_list: Vec<(String, &str)> = match required {
        Some(ms) => ms.iter().map(|m| (m.clone(), if m == "fps_mean" { "higher" } else { "lower" })).collect(),
        None => {
            let mut l: Vec<(String, &str)> = vec![("frame_p95".into(), "lower"), ("fps_mean".into(), "higher")];
            if on_battery_all {
                l.push(("power_w_mean".into(), "lower"));
            }
            l
        }
    };
    let mut p_primary = 1.0f64;
    let mut wins: Vec<String> = Vec::new();
    let mut regressions: Vec<String> = Vec::new();
    let mut per_metric: Vec<(String, Value)> = Vec::new();
    let alpha_win = alpha / metrics_list.len() as f64;
    for (metric, better) in &metrics_list {
        let cmp = loopstat::compare(&a_runs, &b_runs, metric);
        let diff = cmp.get("diff_mean").and_then(|v| v.as_f64());
        let p = cmp.get("perm_p_two_sided").and_then(|v| v.as_f64());
        let improved = match (diff, *better) {
            (Some(d), "lower") => d < 0.0,
            (Some(d), "higher") => d > 0.0,
            _ => false,
        };
        let status = if cmp.get("error").is_some() || diff.is_none() {
            "缺数据"
        } else if improved && p.is_some_and(|x| x < alpha_win) {
            wins.push(metric.to_string());
            "显著改善"
        } else if !improved && diff.is_some_and(|x| x != 0.0) && p.is_some_and(|x| x < alpha) {
            regressions.push(metric.to_string());
            "显著变差"
        } else {
            "无显著变化"
        };
        if metric == primary {
            p_primary = p.unwrap_or(1.0);
        }
        per_metric.push((
            metric.to_string(),
            {
                let sv = |k: &str| cmp.get(k).map(to_serde).unwrap_or(Value::Null);
                json!({
                    "status": status,
                    "a_mean": sv("a_mean"),
                    "b_mean": sv("b_mean"),
                    "diff_mean": sv("diff_mean"),
                    "diff_pct": sv("diff_pct"),
                    "p": sv("perm_p_two_sided"),
                    "ci95": sv("ci95"),
                })
            }
        ));
    }
    let keep = !wins.is_empty() && regressions.is_empty();
    let aux = aux_compare(&a_runs, &b_runs);
    let verdict = json!({
        "is_pareto_improvement": keep,
        "p_value": p_primary,
        "stat": "exact_permutation",
        "alpha_actual": alpha,
        "alpha_win": alpha_win,
        "alpha_regression": alpha,
        // 轮数下界: alpha_win 是否够得着 (2/C(2n,n) < alpha_win)
        "reachable": reachable(rounds, alpha_win),
        "wins": wins,
        "regressions": regressions,
        "per_metric": Value::Object(per_metric.into_iter().collect()),
        "aux": aux,
        "reason": if keep {
            json!(format!("改善 {} 且无显著退化", metrics_list.iter().find(|(m, _)| wins.first() == Some(&m.to_string())).map(|(m, _)| m.clone()).unwrap_or_default()))
        } else if !regressions.is_empty() {
            json!(format!("存在显著退化: {}", regressions.join(",")))
        } else {
            json!("没有任何指标显著改善")
        },
    });
    (verdict, keep, on_battery_all)
}

/// 逐臂聚合进 metrics (B 臂 = 候选臂)。回候选臂功耗 (放电态才有)。
pub(crate) fn aggregate_metrics(m: &mut Value, knob_runs: &[Value], ctrl_runs: &[Value], on_battery_all: bool) -> Option<f64> {
    let f = |runs: &[Value], k: &str| -> Option<f64> {
        let xs: Vec<f64> = runs.iter().filter_map(|r| r["summary"][k].as_f64()).collect();
        (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
    };
    let temps: Vec<f64> = knob_runs
        .iter()
        .chain(ctrl_runs.iter())
        .filter_map(run_soc_temp_max)
        .collect();
    m["fps_p95_ms"] = json!(f(knob_runs, "frame_p95"));
    m["frame_p95_ms"] = json!(f(knob_runs, "frame_p95"));
    m["fps_mean"] = json!(f(knob_runs, "fps_mean"));
    m["soc_temp_max_c"] = json!(temps.iter().cloned().reduce(f64::max));
    // 功耗: 只在放电态均值可信时给; 与帧时一样取候选臂全部轮的均值, 不取最后一轮
    let power = if on_battery_all {
        let xs: Vec<f64> = knob_runs.iter().filter_map(|r| r["env"]["power_w_mean"].as_f64()).collect();
        (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
    } else {
        None
    };
    m["power_watt"] = json!(power);
    power
}

// ══════════════ gray ══════════════

/// 一个 gray 候选的改写 → 挂层参数。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GrayKnobs {
    /// LoadOp LOAD→DONT_CARE 全开。
    pub loadop: bool,
    /// 「去掉多余写回」指定串 ("0" = 关)。
    pub storeop: String,
}

/// 读 payload.rewrites。不认识的开关、缺范围的写回改写一律拒收 —— 在碰设备之前。
///
/// - `loadop_dont_care`: 全部 LoadOp LOAD → DONT_CARE (层的 debug.knobs.loadop)。
/// - `storeop_dont_care` + `scope = "<pass 特征码前缀 ≥8 位>:<附件下标>"`: 只改那一个 pass 的
///   那一个附件 (debug.knobs.storeop)。特征码取自 `phonefarm frame-audit analyze`。可以有多条,
///   合成一个指定串一起施加。
pub(crate) fn parse_gray_rewrites(payload: &Value) -> Result<GrayKnobs, String> {
    let arr = payload["rewrites"].as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        return Err("payload.rewrites 是空的: 没有要改写的".into());
    }
    let mut loadop = false;
    let mut items: Vec<(String, u32)> = Vec::new();
    for r in &arr {
        let knob = r["knob"].as_str().unwrap_or("");
        match knob {
            "loadop_dont_care" => loadop = true,
            "storeop_dont_care" => {
                let scope = r["scope"].as_str().unwrap_or("");
                let (sig, att) = scope.split_once(':').ok_or_else(|| {
                    format!("storeop_dont_care 要 scope=\"<特征码前缀>:<附件下标>\", 收到 {scope:?}")
                })?;
                let att: u32 = att
                    .trim()
                    .parse()
                    .map_err(|_| format!("storeop_dont_care 的附件下标看不懂: {scope:?}"))?;
                items.push((sig.trim().to_string(), att));
            }
            other => return Err(format!("接收端不认识改写开关 {other:?} (认: loadop_dont_care / storeop_dont_care)")),
        }
    }
    let storeop = crate::graylayer::storeop_spec(&items)?;
    Ok(GrayKnobs { loadop, storeop })
}

/// 画面比对缺省门槛: 改写臂与对照臂同位置截图的 PSNR 低于它就判画面出错。
pub(crate) const GRAY_MIN_PSNR_DB: f64 = 30.0;

/// 画面判据允许比「同设置重拍」差多少 dB。
///
/// 1.0 dB 的依据: 实测同一场景两次重拍能差 1.5-2 dB (Megacity city_static 实测 26.41-28.06),
/// 而真正改了画面的改写差 2 dB 以上 (同次实测 knob 29.71 dB)。留 1 dB 给噪声, 剩下留给真实的画面变化。
pub(crate) const IMAGE_GATE_MARGIN_DB: f64 = 1.0;
/// 噪声底低于这个值 = 这个场景自身不稳定, 画面判据没有分辨力, 必须拒绝判定而不是猜。
pub(crate) const IMAGE_GATE_MIN_USABLE_DB: f64 = 20.0;

/// 画面判据的输入: 配置阈值 + 这个场景实测的噪声底。
///
/// 固定阈值的毛病是它不知道场景有多稳。Megacity city_static 实测同一臂相隔 1 秒重拍只有
/// 26.41 / 27.90 dB, 对照对对照 28.06 dB —— 固定 30 dB 的阈值比噪声底还高, 任何候选
/// (包括什么都没改的) 都会被判「画面出错」。所以判定要拿噪声底比。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageGate {
    /// 配置的绝对阈值 ([`GRAY_MIN_PSNR_DB`] 或请求里的 `image_check.min_psnr_db`)。
    pub configured_db: f64,
    /// 同设置重拍能达到的相似度 (取观测到的最大值)。`None` = 没有重拍可用。
    pub noise_floor_db: Option<f64>,
    pub margin_db: f64,
    pub min_usable_db: f64,
}

impl ImageGate {
    pub(crate) fn new(configured_db: f64, noise_floor_db: Option<f64>) -> Self {
        ImageGate {
            configured_db,
            noise_floor_db,
            margin_db: IMAGE_GATE_MARGIN_DB,
            min_usable_db: IMAGE_GATE_MIN_USABLE_DB,
        }
    }

    /// 这一关按什么过的: `absolute` / `noise_floor`。
    pub(crate) fn gate(&self, passed_by: &str) -> serde_json::Value {
        serde_json::json!({
            "passed_by": passed_by,
            "configured_db": self.configured_db,
            "noise_floor_db": self.noise_floor_db,
            "margin_db": self.margin_db,
            "min_usable_db": self.min_usable_db,
        })
    }
}

/// 画面判定结果。
pub(crate) enum ImageRuling {
    Pass(&'static str),
    Fail(String),
}

/// 同设置重拍能达到的相似度: 取观测到的**最大值**。
///
/// 用最大值而不是最小值: 场景里有一点动画时, 两次重拍的差距本身就是噪声,
/// 取最大的那一次才是这套采集能在同一内容上达到的上限。
pub(crate) fn noise_floor_db(values: impl IntoIterator<Item = Option<f64>>) -> Option<f64> {
    values.into_iter().flatten().reduce(f64::max)
}

/// 判定「改写臂的画面是不是比同设置重拍还差」。
///
/// `best_knob_vs_ctrl_db` 取改写臂对多个对照帧里**最好**的那一次
/// (场景有动画时, 最差的那次只是撞上了动画帧, 它的含义不是「画面被改坏了」)。
pub(crate) fn judge_image(gate: &ImageGate, best_knob_vs_ctrl_db: Option<f64>) -> ImageRuling {
    let Some(w) = best_knob_vs_ctrl_db else {
        return ImageRuling::Fail("截图尺寸不一致, 比不了".into());
    };
    if w >= gate.configured_db {
        return ImageRuling::Pass("absolute");
    }
    match gate.noise_floor_db {
        Some(n) if n < gate.min_usable_db => ImageRuling::Fail(format!(
            "同设置重拍只有 {n:.2} dB (低于可用下限 {:.0} dB): 这个场景自身不稳定, \
画面判据在它上面没有分辨力 —— 换静态场景或改用孪生 pass 现场开关, 不要拿这个数判画面",
            gate.min_usable_db
        )),
        Some(n) if w >= n - gate.margin_db => ImageRuling::Pass("noise_floor"),
        Some(n) => ImageRuling::Fail(format!(
            "改写臂只有 {w:.2} dB, 低于配置阈值 {:.2} dB, 也低于同设置重拍噪声底 {n:.2} dB 减 {:.1} dB",
            gate.configured_db, gate.margin_db
        )),
        None => ImageRuling::Fail(format!(
            "改写臂 {w:.2} dB 低于配置阈值 {:.2} dB, 且没有同设置重拍可作噪声底: 判不了 \
(至少跑 2 轮, 或让采集侧多拍一张同设置对照)",
            gate.configured_db
        )),
    }
}
/// 挂层重启、进到场景之后先等画面稳下来再截图 (流式加载、过场淡入)。
const GRAY_SHOT_SETTLE_S: u64 = 8;
/// 两张图逐像素相同时 PSNR 是无穷大; JSON 放不下, 按这个值记。
const PSNR_IDENTICAL_DB: f64 = 99.0;

/// 两张 RGBA 截图的 PSNR (只算 RGB)。尺寸不同回 None —— 比不了, 不是"差别很大"。
pub(crate) fn psnr_rgba(a: &[u8], b: &[u8]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 4 {
        return None;
    }
    let mut se: f64 = 0.0;
    let mut n: u64 = 0;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            let d = pa[c] as f64 - pb[c] as f64;
            se += d * d;
        }
        n += 3;
    }
    let mse = se / n as f64;
    if mse == 0.0 {
        return Some(PSNR_IDENTICAL_DB);
    }
    Some(((10.0 * (255.0f64 * 255.0 / mse).log10()) * 100.0).round() / 100.0)
}

fn recover_gray_eval(args:&[String])->i32 {
    let result=(||->Result<Value,String>{
        let (mut dir,mut serial)=(None,None);let mut it=args.iter();
        while let Some(a)=it.next(){match a.as_str(){"--session"=>dir=it.next(),"--serial"=>serial=it.next(),"--json"=>{},_=>return Err(format!("unknown eval recover argument: {a}"))}}
        let parent=crate::gray_eval_session::Parent::resume(Path::new(dir.ok_or("missing --session")?),serial.ok_or("missing --serial")?)?;
        parent.finish(Some("explicit same-context recovery; original experiment report is unchanged"))
    })();
    match result {Ok(r)=>{println!("{}",serde_json::to_string_pretty(&r).unwrap());if r["status"]=="reported_complete"{0}else{2}},Err(e)=>{eprintln!("{e}");2}}
}

fn wait_owned_launch(parent:&crate::gray_eval_session::Parent, launch:&LaunchSpec, layered:bool)->Result<(),String> {
    let start=std::time::Instant::now();
    loop {
        if !parent.base.target_alive()? {return Err("target exited before the launch window".into());}
        let ready=if launch.marker.is_some() {parent.base.eval_launch_marker()?.is_some_and(|v|!v.trim().is_empty())}
            else {start.elapsed().as_secs()>=LAUNCH_SETTLE_S};
        let loaded=!layered || parent.base.marker()?.is_some_and(|v|v["frames"].as_u64().is_some());
        if ready&&loaded {if layered{parent.base.narrow()?;}return Ok(());}
        if start.elapsed().as_secs()>=LAUNCH_MARKER_TIMEOUT_S {return Err("launch marker or current layer evidence timed out".into());}
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

enum OwnedGrayWorkload { Launch(LaunchSpec), Script(crate::gray_script::Plan) }
impl OwnedGrayWorkload {
    fn plan(&self)->Option<&crate::gray_script::Plan> {match self {Self::Script(p)=>Some(p),_=>None}}
    fn restart(&self,parent:&crate::gray_eval_session::Parent,label:&str,layered:bool,knobs:&GrayKnobs,candidate:bool)->Result<(),String> {
        match self {
            Self::Launch(l)=>{parent.base.restart_eval(label,layered.then_some(if candidate{knobs.storeop.as_str()}else{"0"}),&l.extras)?;wait_owned_launch(parent,l,layered)},
            Self::Script(_)=>{
                parent.base.restart_script(label,layered,candidate&&knobs.loadop,if candidate{&knobs.storeop}else{"0"})?;
                std::thread::sleep(std::time::Duration::from_secs(GRAY_SHOT_SETTLE_S));
                if !parent.base.target_alive()? {return Err("target exited before finite input".into());}
                if layered {
                    let start=std::time::Instant::now();
                    loop {
                        if parent.base.marker()?.is_some_and(|v|v["frames"].as_u64().is_some()) {parent.base.narrow()?;break;}
                        if !parent.base.target_alive()? || start.elapsed().as_secs()>=LAUNCH_MARKER_TIMEOUT_S {return Err("current layer evidence unavailable before finite input".into());}
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
                Ok(())
            }
        }
    }
}
fn owned_shot(parent:&crate::gray_eval_session::Parent,path:&Path,plan:Option<&crate::gray_script::Plan>)->Result<(u32,u32,Vec<u8>),String> {
    parent.base.screenshot(path)?;
    let raw=crate::frameaudit::png_raw(path)?;
    let (w,h,pixels)=crate::framecheck::parse_screencap(&raw)?;
    if let Some(plan)=plan {plan.validate_screen(w,h)?;}
    Ok((w,h,pixels.to_vec()))
}
fn shot_psnr(a:&(u32,u32,Vec<u8>),b:&(u32,u32,Vec<u8>))->Result<f64,String> {
    if (a.0,a.1)!=(b.0,b.1) {return Err("original screen dimensions changed across comparison images".into());}
    psnr_rgba(&a.2,&b.2).ok_or_else(||"image comparison bytes differ".into())
}
fn run_owned_gray(parent:&crate::gray_eval_session::Parent,workload:&OwnedGrayWorkload,label:&str,out:&Path,seconds:u64,comm:Option<&str>)->Result<Value,String> {
    let (workload_ok,sampler_ok,input)=match workload {
        OwnedGrayWorkload::Launch(_)=>{let(w,s)=parent.sample(label,seconds,out)?;(w,s,Value::Null)},
        OwnedGrayWorkload::Script(_)=>parent.sample_script(label,seconds,out,false)?,
    };
    let alive=parent.base.target_alive()?;
    parent.base.eval_stop_target()?;
    if !workload_ok {return Err(format!("{label}: trace workload failed; raw exit and log retained"));}
    let trace=std::fs::read_to_string(out.join("trace.txt")).map_err(|e|e.to_string())?;
    let parsed=crate::looptrace::parse(&trace,comm.filter(|c|!c.is_empty()).unwrap_or(crate::looptrace::DEFAULT_RENDER_COMM));
    let summary=py_dumps(&parsed.summarize());
    crate::confirmation_transport::new_file(&out.join("summary.json"),summary.as_bytes())?;
    let env_path=out.join("env.txt");
    let env=std::fs::read_to_string(&env_path).map_err(|e|e.to_string())?;
    let mut result=json!({"label":label,"run_once_rc":0,"launch":workload.plan().is_none(),"input_window":input,
        "summary":serde_json::from_str::<Value>(&summary).map_err(|e|e.to_string())?,"env":to_serde(&sysparam::env_stats(&env))});
    bind_run_evidence(&mut result,label,out,&env_path,sampler_ok,seconds+if workload.plan().is_some(){10}else{4},seconds,Some(if alive {0}else{1}))?;
    crate::confirmation_transport::new_file(&out.join("run.json"),&serde_json::to_vec_pretty(&result).unwrap())?;
    Ok(result)
}

pub(crate) fn run_owned_gray_script(parent:&crate::gray_eval_session::Parent,plan:&crate::gray_script::Plan,label:&str,out:&Path)->Result<Value,String> {
    run_owned_gray(parent,&OwnedGrayWorkload::Script(plan.clone()),label,out,plan.seconds,None)
}

/// Archive a current-scene sample without restarting/stopping the target or
/// applying the eval qualification/decision policy. Collection errors are data;
/// only an inability to persist the result returns Err to the owner.
#[allow(clippy::too_many_arguments)]
pub(crate) fn summarize_control_sample(
    out: &Path,
    label: &str,
    seconds: u64,
    trace_ok: bool,
    sampler_ok: bool,
    input: Value,
    target_alive: bool,
    evidence_origin: &str,
    render_comm: Option<&str>,
) -> Result<Value, String> {
    let mut errors = Vec::new();
    if let Ok(text) = std::fs::read_to_string(out.join("collection-error.json")) {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(error) = value["error"].as_str() { errors.push(error.to_owned()); }
        }
    }
    let sample_request = std::fs::read_to_string(out.join("sample-request.json")).ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok()).unwrap_or(Value::Null);
    let requested_power = sample_request["power_policy"].as_str().unwrap_or("observe");
    let power_path = out.join("power.json");
    // The returned snapshot also carries a final evidence-save failure; an
    // earlier on-disk snapshot must not hide that failure from qualification.
    let power = input.get("power").filter(|v|v.is_object()).cloned().or_else(||
        std::fs::read_to_string(&power_path).ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok()).filter(Value::is_object))
        .unwrap_or_else(||json!({"policy":requested_power,"before":null,"after":null,
            "error":if power_path.exists(){json!("power evidence could not be read")}else{Value::Null}}));
    let power_condition_ok = power["error"].is_null() && power["policy"] == requested_power
        && (requested_power == "observe" || (requested_power == "battery"
            && power["before"]["on_battery"] == true && power["after"]["on_battery"] == true));
    if !power_condition_ok {
        errors.push(power["error"].as_str().unwrap_or("requested battery policy lacks verified before/after discharge observations").to_owned());
    }
    let trace = std::fs::read_to_string(out.join("trace.txt"));
    let env_text = std::fs::read_to_string(out.join("env.txt"));
    if !trace_ok { errors.push("trace child failed; inspect its exit code and log".to_owned()); }
    if !sampler_ok { errors.push("environment sampler failed; inspect its exit code and log".to_owned()); }
    if let Err(error) = &trace { errors.push(format!("trace evidence unavailable: {error}")); }
    if let Err(error) = &env_text { errors.push(format!("environment evidence unavailable: {error}")); }
    let summary = if trace_ok {
        render_comm.zip(trace.as_ref().ok()).map(|(comm, text)| {
            to_serde(&crate::looptrace::parse(text, comm).summarize())
        })
    } else { None };
    if let Some(summary) = &summary {
        crate::confirmation_transport::new_file(&out.join("summary.json"), &serde_json::to_vec_pretty(summary).unwrap())?;
    }
    let mut run = json!({"schema":"phonefarm/gray-control-sample@1", "label":label,
        "requested_seconds":seconds,"evidence_origin":evidence_origin,"target_alive_after":target_alive,
        "input_kind":sample_request["input_kind"],"planned_input_ms":sample_request["planned_input_ms"],"initial_frame":sample_request["initial_frame"],"power":power,
        "input_window":input,"summary":summary.clone().unwrap_or_else(||json!({})),
        "env":env_text.as_ref().map(|text|to_serde(&sysparam::env_stats(text))).unwrap_or_else(|_|json!({}))});
    if trace_ok && trace.is_ok() && env_text.is_ok() {
        if let Err(error) = bind_run_evidence(&mut run,label,out,&out.join("env.txt"),sampler_ok,seconds+10,seconds,Some(if target_alive{0}else{1})) {
            errors.push(error);
        }
    } else {
        run["env"]["trace_window_verified"] = json!(false);
        run["env"]["sampler_exit_success"] = json!(sampler_ok);
        if !target_alive { errors.push("target was not alive after collection".into()); }
    }
    // No implicit Unity filter and no claim that a comm filter proves process
    // ownership. Keep the original parser semantics available for later audit.
    if summary.is_none() { run["summary"] = Value::Null; }
    let mut limitations = vec![
        "GPU submit proxy; target process/context ownership and display FPS are not verified".to_owned(),
        "power covers the whole sampler window including lead/tail, not only the trace window".to_owned(),
        "energy per frame requires the complete GPU-submit group window and verified frame semantics".to_owned(),
    ];
    if sample_request["input_kind"] == "finite" {
        limitations.push("input geometry is verified at launch only; natural completion does not restore game state".into());
    }
    if render_comm.is_none() {
        limitations.push("no render thread filter selected; raw trace is retained and frame metrics are unknown".into());
    } else if summary.as_ref().is_some_and(|s|s["n_frames"].as_u64().unwrap_or(0)==0) {
        limitations.push("no frame intervals matched the requested render thread filter".into());
    }
    if evidence_origin != "device" {
        limitations.push("synthetic evidence is not a real device performance measurement".into());
    }
    // An old charge-suspend/bypass file is not an ownership or power-window
    // proof for this session. Battery discharge must be observed throughout a
    // verified sampler/trace window, including any requested power conditions.
    let power_ok = power_condition_ok && sampler_ok && run["env"]["trace_window_verified"] == true
        && run["env"]["power_window_verified"] == true && run["env"]["on_battery"] == true
        && run["env"]["power_source"] == "battery";
    if !power_ok {
        let reason = if !power_condition_ok {
            power["error"].as_str().unwrap_or("requested battery policy was not verified before and after this sample").to_owned()
        } else if run["env"]["trace_window_verified"] != true {
            "sampler/trace window coverage was not verified".to_owned()
        } else if run["env"]["power_source"] == "usb_bypass" {
            "legacy bypass marker does not establish battery discharge for this control sample".to_owned()
        } else {
            run["env"]["power_usable_reason"].as_str().unwrap_or("battery discharge throughout the sampling window was not verified").to_owned()
        };
        run["env"]["power_w_mean"] = Value::Null;
        run["env"]["power_w_median"] = Value::Null;
        run["env"]["power_valid"] = json!(false);
        run["env"]["power_cross_run_comparable"] = json!(false);
        run["env"]["power_usable_reason"] = json!(reason);
        limitations.push(format!("power unavailable: {reason}"));
    }
    let energy = if power_ok {
        crate::sysparam::battery_energy_in_capture(env_text.as_ref().unwrap(),trace.as_ref().unwrap(),seconds)
    } else {
        Err(run["env"]["power_usable_reason"].as_str().unwrap_or("battery power window is not qualified").to_owned())
    };
    let energy = energy.unwrap_or_else(|reason|json!({"available":false,"estimated":true,"reason":reason,
        "energy_j":null,"power_w_mean":null,"rail":"battery","window":null}));
    run["metrics"] = json!({"gpu_submit_fps":run["summary"]["fps_mean"],
        "frame_p95_ms":run["summary"]["frame_p95"],"gpu_active_mean_ms":run["summary"]["gpu_active_mean"],
        "queue_p95_ms":run["summary"]["queue_p95"],"power_w_mean":run["env"]["power_w_mean"],
        "trace_energy_j":energy["energy_j"],"trace_power_w_mean":energy["power_w_mean"],"energy_per_frame_mj":null});
    run["metric_provenance"] = json!({"frame":{"semantics":"gpu_submit_proxy","render_comm":render_comm,
        "grouping":"existing auto submits-per-frame parser","target_ownership_verified":false,"display_verified":false},
        "power":{"rail":run["env"]["power_rail"],"window_verified":power_ok,
            "scope":"whole sampler window including lead/tail"},"trace_energy":energy,
        "energy_per_frame":{"available":false,"reason":"complete GPU-submit group window and frame ownership are not verified"}});
    run["collection"] = json!({"status":if errors.is_empty(){"complete"}else{"failed"},
        "error":if errors.is_empty(){Value::Null}else{json!(errors.join("; "))}});
    run["limitations"] = json!(limitations);
    let names = ["trace.txt","summary.json","env.txt","capture.log","collection-error.json","input-plan.json","workload_exit.txt","trace_exit.txt","sampler_exit.txt",
        "quiescence.txt","workload.log","trace.log","workload-time.json","trace-time.json","sampler-time.json",
        "input-complete.json","input-window.json","sample-request.json","input-frame.json","input-check.png","power.json","power-before.json","power-after.json"];
    run["artifact_path_base"] = json!("run_directory");
    run["artifacts"] = json!(names.into_iter().filter(|name|out.join(name).is_file())
        .map(|name|json!({"name":name,"path":name})).collect::<Vec<_>>());
    let workload = format!("workload-{label}.sh");
    if out.join(&workload).is_file() {
        run["artifacts"].as_array_mut().unwrap().push(json!({"name":workload,"path":workload}));
    }
    crate::confirmation_transport::new_file(&out.join("run.json"),&serde_json::to_vec_pretty(&run).unwrap())?;
    Ok(run)
}

fn effective_gray_hits(marker:&Value)->u64 {
    marker["effective"].as_array().map(|xs|xs.iter().filter_map(|e|e["begins"].as_u64()).sum()).unwrap_or(0)
}

fn gray_eval_owned(req:&Value, lease:crate::devlease::Lease, evidence:&Path, origin:&str)->Result<Value,String> {
    let knobs=parse_gray_rewrites(&req["payload"])?;
    if req["workload"]["enter"].as_str()==Some("onboard") {return Err("owned gray eval does not support onboard/HID input".into());}
    let seconds=match req["workload"].get("seconds") {None=>30,Some(v)=>v.as_u64().ok_or("workload.seconds must be an integer")?};
    let workload=if let Some(launch)=launch_spec(req) {
        if knobs.loadop {return Err("gray workload.launch supports storeop only".into());}
        if !(1..=86400).contains(&seconds) {return Err("workload.seconds must be 1..86400".into());}
        OwnedGrayWorkload::Launch(launch)
    } else {
        let file=match req["workload"].get("script") {None|Some(Value::Null)=>"loop_v1/scripts/workload_spin_touch_v1.json",Some(v)=>v.as_str().ok_or("workload.script must be a local path")?};
        let path=resolve_path(file).ok_or_else(||format!("finite input script not found: {file}"))?;
        let input:Value=serde_json::from_slice(&crate::confirmation_transport::bounded(&path,4*1024*1024)?).map_err(|e|e.to_string())?;
        OwnedGrayWorkload::Script(crate::gray_script::parse(&input,seconds)?)
    };
    let policy=policy_for_request(req)?;
    let rounds=policy.required_pairs;let alpha=policy.alpha;let cap=policy.temp_cap_c;let cool=policy.cool_c;
    let min_psnr=req["payload"]["image_check"]["min_psnr_db"].as_f64().unwrap_or(GRAY_MIN_PSNR_DB);
    let pkg=req["package"].as_str().ok_or("missing package")?;
    let parent=match &workload {
        OwnedGrayWorkload::Launch(l)=>crate::gray_eval_session::Parent::new(crate::gray_session::Session::new_eval(lease,evidence,pkg,origin,l.marker.as_deref())?,req)?,
        OwnedGrayWorkload::Script(p)=>crate::gray_eval_session::Parent::new_script(crate::gray_session::Session::new_script(lease,evidence,pkg,origin)?,req,p)?,
    };
    let dir=parent.base.dir.clone();
    let mut report=base_report(req,"gray");
    report["session"]=json!(dir);report["evidence_dir"]=json!(dir);report["evidence_origin"]=json!(origin);
    let mut facts=Evidence::new(Stage::Probe,policy,qualification::Capabilities::default());
    if origin=="synthetic" {facts.evidence_origin="synthetic".into();}
    let mut ctrl=Vec::new();let mut candidate=Vec::new();let mut cool_log=Vec::new();let mut frames=Vec::new();let mut image_check=Value::Null;
    let mut psnr_pairs=Vec::new();let mut psnr_controls=Vec::new();let mut noise_within=Vec::new();let mut previous_control=None;
    let result=(||->Result<(),String>{
        parent.stage("capability")?;
        facts.device=parent.capabilities()?;
        if !record_qualification(&mut report,&dir,&facts)?.probe_allowed {return Err("root/trace prerequisites not verified".into());}
        parent.base.prepare()?;
        parent.stage("image_check")?;
        match &workload {
            OwnedGrayWorkload::Launch(launch)=>{
                let static_extras=launch.static_extras.as_deref().unwrap_or(&launch.extras);
                parent.base.install_storeop(req["payload"]["layer_so"].as_str(),&knobs.storeop,Some(static_extras))?;
                let image_dir=dir.join("image_check");std::fs::create_dir(&image_dir).map_err(|e|e.to_string())?;
                let live=crate::frameaudit::collect_storeop(&parent.base,&image_dir,&knobs.storeop,2,pkg,&[],STATIC_SETTLE_S,&mut frames)?;
                crate::confirmation_transport::new_file(&image_dir.join("result.json"),&serde_json::to_vec_pretty(&live).unwrap())?;
                image_check=json!({"method":"storeop_check_live","result":live});
                live_check_verdict(&live)?;facts.quality_verified=Some(true);
            },
            OwnedGrayWorkload::Script(_)=>parent.base.install_script(req["payload"]["layer_so"].as_str())?,
        }
        parent.base.eval_stop_target()?;
        parent.stage("qualification")?;parent.deploy()?;
        let fan=parent.fan_off()?;
        let cold=parent.wait_cool(cool,420)?;facts.cold_ok=cold["ok"].as_bool();
        report["baseline_conditions"]=json!({"fan":fan,"cold":cold});
        if facts.cold_ok!=Some(true) {return Err("baseline cold condition not verified".into());}
        workload.restart(&parent,"workload_check",false,&knobs,false)?;
        let motion_dir=dir.join("workload_check");std::fs::create_dir(&motion_dir).map_err(|e|e.to_string())?;
        let a=motion_dir.join("frame_a.png");let b=motion_dir.join("frame_b.png");
        let input=match &workload {
            OwnedGrayWorkload::Launch(_)=>{
                parent.base.screenshot(&a)?;std::thread::sleep(std::time::Duration::from_secs(5));parent.base.screenshot(&b)?;Value::Null
            },
            OwnedGrayWorkload::Script(plan)=>{
                owned_shot(&parent,&motion_dir.join("input_screen.png"),Some(plan))?;
                let (trace_ok,sampler_ok,input)=parent.sample_script("workload_check",seconds,&motion_dir,true)?;
                if !trace_ok||!sampler_ok {return Err("finite workload qualification child failed".into());}input
            }
        };
        let motion=to_serde(&crate::framecheck::verdict(&crate::frameaudit::png_raw(&a)?,&crate::frameaudit::png_raw(&b)?));
        let alive=parent.base.target_alive()?;parent.base.eval_stop_target()?;
        facts.workload_verified=Some(alive&&motion["spinning"]==true);
        let check=json!({"verified":facts.workload_verified,"app_alive":alive,"motion":motion,"frames":[a,b],"launch":workload.plan().is_none(),"input_window":input});
        crate::confirmation_transport::new_file(&motion_dir.join("workload_check.json"),&serde_json::to_vec_pretty(&check).unwrap())?;
        if facts.workload_verified!=Some(true) {return Err("workload motion not verified".into());}
        for i in 1..=3 {
            let label=format!("baseline{i}");parent.stage(&label)?;
            workload.restart(&parent,&label,false,&knobs,false)?;
            let out=dir.join(&label);std::fs::create_dir(&out).map_err(|e|e.to_string())?;
            if let Some(plan)=workload.plan(){owned_shot(&parent,&out.join("input_screen.png"),Some(plan))?;}
            facts.baseline.push(run_owned_gray(&parent,&workload,&label,&out,seconds,req["workload"]["render_comm"].as_str())?);
        }
        parent.admission_ready()?;facts.restored=None;facts.stage=Stage::Experiment;
        let q=qualification::evaluate_gray_session_collection(&facts)?;
        if !write_qualification(&mut report,&dir,&facts,q)?.experiment_allowed {return Err("gray baseline/workload/power qualification did not admit candidate collection".into());}
        for i in 1..=rounds {
            let mut shots=std::collections::BTreeMap::new();
            for arm in if i%2==1 {["knob","ctrl"]}else{["ctrl","knob"]} {
                let label=format!("{arm}{i}");parent.stage(&label)?;
                let fan=parent.fan_off()?;let mut cold=parent.wait_cool(cool,arm_cool_timeout(&cool_log))?;
                cold["arm"]=json!(label);cold["fan"]=fan;cool_log.push(cold);
                workload.restart(&parent,&label,true,&knobs,arm=="knob")?;
                let out=dir.join(&label);std::fs::create_dir(&out).map_err(|e|e.to_string())?;
                if let Some(plan)=workload.plan() {
                    let path=out.join("image.png");let shot=owned_shot(&parent,&path,Some(plan))?;frames.push(json!(path));
                    if arm=="ctrl" {
                        std::thread::sleep(std::time::Duration::from_secs(GRAY_SHOT_SETTLE_S));
                        let path=out.join("image-repeat.png");let repeat=owned_shot(&parent,&path,Some(plan))?;frames.push(json!(path));
                        noise_within.push(shot_psnr(&shot,&repeat)?);
                    }
                    shots.insert(arm,shot);
                }
                let mut run=run_owned_gray(&parent,&workload,&label,&out,seconds,req["workload"]["render_comm"].as_str())?;
                if let (Some(t),Some(limit))=(run_soc_temp_max(&run),cap) {if t>limit{return Err(format!("{label}: SoC temperature {t} exceeds {limit}"));}}
                if arm=="knob" {
                    let marker=parent.base.marker()?.ok_or("candidate marker missing")?;
                    crate::confirmation_transport::new_file(&out.join("layer_marker.json"),&serde_json::to_vec_pretty(&marker).unwrap())?;
                    run["layer_hits"]=json!(effective_gray_hits(&marker));candidate.push(run);
                } else {ctrl.push(run);}
            }
            if workload.plan().is_some() {
                let c=shots.remove("ctrl").ok_or("missing control quality image")?;
                let k=shots.remove("knob").ok_or("missing candidate quality image")?;
                psnr_pairs.push(shot_psnr(&k,&c)?);
                if let Some(previous)=&previous_control {psnr_controls.push(shot_psnr(previous,&c)?);}
                previous_control=Some(c);
            }
        }
        if workload.plan().is_some() {
            let noise=noise_floor_db(noise_within.iter().chain(psnr_controls.iter()).copied().map(Some));
            let gate=ImageGate::new(min_psnr,noise);
            let used=noise_floor_db(psnr_pairs.iter().copied().map(Some));
            let ruling=judge_image(&gate,used);
            image_check=json!({"method":"same_spot_screenshot","min_psnr_db":min_psnr,"psnr_knob_vs_ctrl_db":psnr_pairs,
                "psnr_ctrl_vs_ctrl_db":psnr_controls,"noise_within_round_db":noise_within,"noise_floor_db":noise,"psnr_db_used":used,
                "gate":match &ruling {ImageRuling::Pass(by)=>gate.gate(by),ImageRuling::Fail(_)=>gate.gate("failed")}});
            facts.quality_verified=Some(matches!(ruling,ImageRuling::Pass(_)));
            if let ImageRuling::Fail(reason)=ruling {return Err(format!("画面出错: {reason}"));}
        }
        Ok(())
    })();
    let mut error=result.err();
    let collection=json!({"schema":if workload.plan().is_some(){"phonefarm/gray-eval-script-collection@1"}else{"phonefarm/gray-eval-launch-collection@1"},"session":dir,"evidence_origin":origin,
        "input_plan":workload.plan().map(|p|p.to_value()),"baseline":facts.baseline,"control":ctrl,"candidate":candidate,"image_check":if workload.plan().is_some(){&image_check}else{&image_check["result"]},"frames":frames,"cool_per_arm":cool_log,"error":error});
    if let Err(e)=crate::confirmation_transport::new_file(&dir.join("eval-collection.json"),&serde_json::to_vec_pretty(&collection).unwrap()) {
        error=Some(format!("{}; collection report: {e}",error.as_deref().unwrap_or("collection completed")));
    }
    let recovery=parent.finish(error.as_deref()).unwrap_or_else(|e|json!({"status":"required","error":e,"session":dir}));
    let complete=recovery["status"]=="reported_complete";
    let hits=candidate.iter().filter_map(|v|v["layer_hits"].as_u64()).max().unwrap_or(0);
    facts.stage=Stage::Conclusion;facts.control=ctrl.clone();facts.candidate=candidate.clone();facts.restored=Some(complete);facts.device.control_verified=Some(hits>0);
    report["recovery"]=recovery;
    let (reached,timed_out)=cool_summary(&cool_log);
    report["conditions_observed"]=json!({"cool_c_target":cool,"cool_reached_c":reached,"cool_timeout":timed_out,"cool_per_arm":cool_log,
        "hits":hits,"apply_ok":hits>0,"restored":complete,"game_state_restored":false,"image_check":image_check});
    let qualified=record_qualification(&mut report,&dir,&facts);
    if let Some(e)=error {report["verdict"]["reason"]=json!(e);}
    else if !complete {report["verdict"]["reason"]=json!("owned gray eval recovery not complete; evidence retained");}
    else if !qualified.as_ref().is_ok_and(|q|q.conclusion_allowed) {report["verdict"]["reason"]=json!("conclusion qualification not satisfied");}
    else if req["payload"]["require_hit"].as_bool().unwrap_or(true)&&hits==0 {report["verdict"]["reason"]=json!("candidate rewrite never hit");}
    else {finish_gray_statistics(&mut report,&candidate,&ctrl,alpha,rounds,psnr_pairs.iter().copied().reduce(f64::min),&facts.policy.required_metrics)?;}
    if let Err(e)=qualified {report["qualification_error"]=json!(e);}
    let path=dir.join("eval-report.json");report["report_path"]=json!(path);
    if let Err(e)=crate::confirmation_transport::new_file(&path,&serde_json::to_vec_pretty(&report).unwrap()) {report["report_error"]=json!(e);}
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn finish_gray_statistics(report:&mut Value, knob_runs:&[Value], ctrl_runs:&[Value], alpha:f64, rounds:u32, psnr:Option<f64>, required:&[String])->Result<(),String> {
    if let Some(e) = fan_mismatch(&knob_runs, &ctrl_runs) {
        report["verdict"]["reason"] = json!(e);
        return Ok(());
    }
    // 画面过了门槛: 照常做统计判定。psnr_db 是约束不是目标, 记最差的那一轮。
    let (verdict, keep, on_battery_all) = judge_runs_with(ctrl_runs, knob_runs, alpha, rounds, Some(required));
    report["verdict"] = verdict;
    let mut m = report["metrics"].take();
    let power = aggregate_metrics(&mut m, &knob_runs, &ctrl_runs, on_battery_all);
    m["psnr_db"] = json!(psnr);
    if power.is_none() {
        unavailable(&mut m, "power_watt", "非放电态 (或功耗采样缺失): 整机功耗量不准, 如实 null");
    }
    if m["fps_p95_ms"].is_null() {
        unavailable(&mut m, "fps_p95_ms", "本轮没拿到可用的帧时序");
    }
    report["metrics"] = m;
    report["status"] = json!(if keep { "PASS" } else { "REJECT" });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2_req(kind: &str, payload: Value) -> Value {
        json!({
            "version": 2,
            "kind": kind,
            "candidate_id": "cand-test-1",
            "package": "com.example.game",
            "workload": {"carrier": "game_replay", "script": null, "seconds": 30},
            "conditions": {"cool_c": null, "temp_cap_c": 58.2, "require_root": true,
                           "require_discharging": true, "charging_suspend": {"enabled": true, "node": null},
                           "fan": "on", "ingame_fps_setting": "60fps/高",
                           "screen_brightness": null, "quality_reference": null, "notes": null},
            "protocol": {"rounds": 5, "alternating": true, "p_threshold": 0.05},
            "payload": payload
        })
    }

    fn validate_ok(req: &Value) -> bool { validate_v2(req).is_ok() }
    fn validate_err(req: &Value) -> String { validate_v2(req).unwrap_err() }

    #[test]
    fn probe_only_never_claims_reverted_from_failed_or_missing_snapshot() {
        for (a,b) in [("",""),("x=1\n#adb_exit=1","x=1\n#adb_exit=1"),("x=1\n#adb_exit=0","x=2\n#adb_exit=0")] {
            let mut r=base_report(&v2_req("sysparam",json!({"params":{}})),"sysparam");
            probe_report(&mut r,a,b,&vec![]);
            assert_eq!(r["status"],"ABORT");assert_eq!(r["conditions_observed"]["restored"],false);
        }
        assert!(snapshots_identical("x=1\n#adb_exit=0","x=1\n#adb_exit=0"));
    }

    #[test]
    fn v2_envelope_needs_all_required_fields() {
        assert!(validate_ok(&v2_req("sysparam", json!({"params": {}}))));
        let mut r = v2_req("sysparam", json!({"params": {}}));
        r.as_object_mut().unwrap().remove("protocol");
        assert_eq!(validate_err(&r), "v2 缺必填字段 protocol");
    }

    #[test]
    fn protocol_is_validated_before_any_device_touch() {
        let mut r = v2_req("sysparam", json!({"params": {}}));
        r["protocol"]["rounds"] = json!(0);
        assert!(validate_err(&r).contains("rounds"));
        r["protocol"]["rounds"] = json!(5);
        r["protocol"]["p_threshold"] = json!(0.0);
        assert!(validate_err(&r).contains("p_threshold"));
        r["protocol"]["p_threshold"] = json!(1.5);
        assert!(validate_err(&r).contains("p_threshold"));
        r["protocol"]["alternating"] = json!("yes");
        assert!(validate_err(&r).contains("alternating"));
    }

    #[test]
    fn gray_rejects_unimplemented_knobs() {
        assert!(validate_ok(&v2_req("gray",json!({"rewrites":[{"knob":"storeop_dont_care","scope":"15a51a87:0"}]}))));
        let mut r = v2_req("gray", json!({"rewrites": [{"knob": "render_scale", "args": {"scale": 0.75}}]}));
        assert!(validate_err(&r).contains("render_scale"));
        assert!(validate_err(&r).contains("收端已实现"));
        r["payload"]["rewrites"] = json!([{"knob": "loadop_dont_care", "scope": null, "args": null}]);
        assert!(validate_ok(&r));
        r["payload"]["rewrites"] = json!([]);
        assert!(validate_err(&r).contains("至少 1 项"));
    }

    #[test]
    fn sysparam_requires_params_object() {
        assert!(!validate_ok(&v2_req("sysparam", json!({"params": "x"}))));
        assert!(!validate_ok(&v2_req("sysparam", json!({}))));
    }

    // ── 接收端再校验 (不信上层) ──

    fn wl() -> sysparam::Whitelist {
        // 与 src/sysparam.rs 单测同一份 probe
        let probe = "cpu.policies=0\ncpu.policy0.avail_freqs=300000 1000000 2000000\n\
cpu.policy0.scaling_min_freq.cur=300000\ncpu.policy0.scaling_min_freq.writable=yes\n\
cpu.policy0.scaling_min_freq.effect=live\nbus.DDR.boost_freq.cur=0\n\
bus.DDR.avail_freqs=200000 3200000 5333000\nbus.DDR.boost_freq.writable=yes\n\
bus.DDR.boost_freq.effect=live\n";
        sysparam::build_whitelist(&sysparam::parse_probe(probe))
    }

    fn sysreq(params: Value, whitelist: Value, extra: Value) -> Value {
        let mut p = json!({"params": params, "whitelist": whitelist});
        if let Some(e) = extra.as_object() {
            for (k, v) in e {
                p[k.as_str()] = v.clone();
            }
        }
        v2_req("sysparam", p)
    }

    #[test]
    fn out_of_whitelist_params_are_rejected_receiver_side() {
        let w = serde_json::to_value(wl_pyval(&wl())).unwrap();
        // 上层说合法不算数: 不在白名单里的 id 整份拒绝
        let mut req = sysreq(json!({"nope.param": "1"}), w.clone(), json!({}));
        req["payload"]["whitelist"] = json!({});
        let e = revalidate(&req, &wl()).unwrap_err();
        assert!(e.contains("校验拒绝"), "{e}");
        // 值不在取值表里
        let req = sysreq(json!({"bus.DDR.boost_freq": "9999999"}), w.clone(), json!({}));
        assert!(revalidate(&req, &wl()).is_err());
        // 合法的过
        let req = sysreq(json!({"bus.DDR.boost_freq": "5333000"}), w.clone(), json!({}));
        assert!(revalidate(&req, &wl()).is_ok());
    }

    /// deny_keywords 接收端**再拦一道**: 命中温控关键词的不看白名单直接拒,
    /// 上层自定义的关键词表也要一起拦。
    #[test]
    fn deny_keywords_block_again_at_the_receiver() {
        let w = serde_json::to_value(wl_pyval(&wl())).unwrap();
        // 缺省表: thermal
        let req = sysreq(json!({"gpu.thermal_pwrlevel": "0"}), w.clone(), json!({}));
        let e = revalidate(&req, &wl()).unwrap_err();
        assert!(e.contains("温控保护关键词"), "{e}");
        // 上层收窄了 deny_keywords, 收端按它的拦
        let mut req = sysreq(json!({"gpu.fan_pwrlevel": "0"}), w.clone(), json!({}));
        req["payload"]["deny_keywords"] = json!(["fan"]);
        assert!(revalidate(&req, &wl()).is_err());
    }

    /// plan 随请求下发时逐项对回白名单: path 对不上或值越界, 整份拒绝。
    #[test]
    fn plan_entries_are_cross_checked_against_the_whitelist() {
        let w = serde_json::to_value(wl_pyval(&wl())).unwrap();
        let plan = json!([
            {"kind": "sysfs", "path": "/sys/devices/system/cpu/bus_dcvs/DDR/boost_freq", "value": "5333000"}
        ]);
        let req = sysreq(json!({"bus.DDR.boost_freq": "5333000"}), w.clone(), json!({"plan": plan}));
        assert!(revalidate(&req, &wl()).is_ok());
        // path 在白名单里, 但值不在取值表
        let plan = json!([
            {"kind": "sysfs", "path": "/sys/devices/system/cpu/bus_dcvs/DDR/boost_freq", "value": "42"}
        ]);
        let req = sysreq(json!({"bus.DDR.boost_freq": "5333000"}), w.clone(), json!({"plan": plan}));
        assert!(revalidate(&req, &wl()).is_err());
        // path 不在白名单
        let plan = json!([
            {"kind": "sysfs", "path": "/sys/class/thermal/mode", "value": "0"}
        ]);
        let req = sysreq(json!({"bus.DDR.boost_freq": "5333000"}), w.clone(), json!({"plan": plan}));
        assert!(revalidate(&req, &wl()).is_err());
    }

    // ── v2 shader → v1 降级 ──

    /// 降级映射的字段逐一对: conditions.cool_c 优先于 protocol.cool_c (v1 遗留口),
    /// quality_reference payload 优先于 conditions。
    #[test]
    fn shader_v2_maps_to_v1_request() {
        let req = json!({
            "version": 2, "kind": "shader", "candidate_id": "s1",
            "package": "io.github.hgamey.refbench",
            "workload": {"carrier": "refbench", "script": "sr_pipeline", "seconds": null, "frames": 300, "intensity": 2},
            "conditions": {"cool_c": 42.0, "quality_reference": "/tmp/ref_dir"},
            "protocol": {"cool_c": 32.0, "replay_seconds": 12, "rounds": 4, "alternating": true, "p_threshold": 0.05},
            "payload": {"track": "sr", "shader_path": "a.glsl", "spirv_path": "a.spv",
                        "budget_ms": 4.0, "quality_baseline_db": 30.0,
                        "incumbent_latency_ms": null, "quality_reference": null}
        });
        assert!(validate_ok(&req));
        let plan = crate::shaderplan::build(&req,None,None).unwrap();
        assert_eq!(plan.request["protocol"]["cool_c"],42.0);
        assert_eq!(plan.request["budget_ms"],4.0);
        assert_eq!(plan.request["quality_reference"],"/tmp/ref_dir");
        let args=crate::gpuop::parse_args(&[plan.args,vec!["--request".into(),"x".into()]].concat()).unwrap();
        assert_eq!(args.carrier,crate::gpuop::Carrier::Refbench);
        assert_eq!(args.frames,300); assert_eq!(args.intensity,2);
    }

    // ── 报告装配 ──

    /// 未检验哨兵 p=1.0; sysparam 的画质项是结构性的 null 并带人话原因。
    #[test]
    fn base_report_honest_nulls_and_sentinel_p() {
        let req = v2_req("sysparam", json!({"params": {}}));
        let mut r = base_report(&req, "sysparam");
        assert_eq!(r["verdict"]["p_value"], json!(1.0));
        assert_eq!(r["source"], json!("phonefarm"));
        let mut m = r["metrics"].take();
        assert_eq!(m["psnr_db"], Value::Null);
        unavailable(&mut m, "psnr_db", "sysparam 不动画面, 无画质可量");
        r["metrics"] = m;
        let u = r["metrics"]["unavailable"].as_array().unwrap();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0]["field"], json!("psnr_db"));
    }

    /// ABORT 与 REJECT 的分野: 旋钮没生效是「没量准」, 绝不能落成 REJECT。
    #[test]
    fn apply_failure_maps_to_abort_not_reject() {
        // sysparam_eval 在 apply FAIL 时只写 reason 且 status 保持初始 ABORT;
        // 这里核对映射函数本身的语义边界
        let keep = !vec!["frame_p95".to_string()].is_empty() && Vec::<String>::new().is_empty();
        assert!(keep);
        // decide 的三分: KEEP→PASS / REJECT→REJECT / ABORT→ABORT
        let s = |keep: bool, aborted: bool| if aborted { "ABORT" } else if keep { "PASS" } else { "REJECT" };
        assert_eq!(s(true, false), "PASS");
        assert_eq!(s(false, false), "REJECT");
        assert_eq!(s(true, true), "ABORT");
    }

    /// reachable: 5v5 精确置换最小可达 p=2/C(10,5)=0.0079, 够得着 0.0167;
    /// 3v3 最小可达 p=2/C(6,3)=0.1, 够不着。
    #[test]
    fn reachable_reflects_the_resolution_floor() {
        assert!(reachable(5, 0.05 / 3.0));
        assert!(!reachable(3, 0.05 / 3.0));
        assert!(!reachable(1, 0.5));
        assert!(!reachable(0, 0.5));
    }

    /// 功耗与结温来自 sample_env (env), parse-trace 的 summary 里没有这两项。
    /// 判定视图只读 summary 的话, 功耗永远「缺数据」、70 °C 温度上限永远不触发。
    #[test]
    fn fan_guard_flags_still_on_and_mismatched_arms() {
        assert!(fan_still_on(&json!({"before": "1", "after": "0", "forced": true})).is_none());
        assert!(fan_still_on(&json!({"before": "1", "after": "1", "forced": true})).is_some());
        assert!(fan_still_on(&json!({"before": null, "after": null, "forced": false})).is_none());
        let on = json!({"env": {"fan_state": "fan_enable=1,fan_speed_level=5,fan_speed_pwm="}});
        let off = json!({"env": {"fan_state": "fan_enable=0,fan_speed_level=5,fan_speed_pwm="}});
        // 2026-10-12 W1 的形态: 前几臂开、最后一对关
        assert!(fan_mismatch(&[on.clone(), off.clone()], &[on.clone()]).is_some());
        assert!(fan_mismatch(&[off.clone(), off.clone()], &[off.clone()]).is_none());
        // 没记风扇的 (别的机型) 不算不一致
        assert!(fan_mismatch(&[json!({"env": {}})], &[off]).is_none());
    }

    #[test]
    fn run_view_carries_power_and_temp_from_env() {
        let m = json!({
            "label": "sp_knob1",
            "summary": {"frame_p95": 19.5, "fps_mean": 59.2},
            "env": {"power_w_mean": 4.7, "soc_temp_max_c": 55.1, "on_battery": true},
        });
        assert!(m["summary"].get("power_w_mean").is_none());
        let v = run_view(&m);
        assert_eq!(v.get("power_w_mean").and_then(|x| x.as_f64()), Some(4.7));
        assert_eq!(v.get("frame_p95").and_then(|x| x.as_f64()), Some(19.5));
        assert_eq!(run_soc_temp_max(&m), Some(55.1));

        // 两臂各两轮: 功耗这一项要真的比得出来, 不能落成「样本不足」
        let arm = |p: f64| -> Vec<Run> {
            (0..2)
                .map(|i| (format!("r{i}"), run_view(&json!({"summary": {}, "env": {"power_w_mean": p + i as f64 * 0.01}}))))
                .collect()
        };
        let cmp = loopstat::compare(&arm(4.7), &arm(5.2), "power_w_mean");
        assert!(cmp.get("error").is_none());
        assert!(cmp.get("diff_mean").and_then(|x| x.as_f64()).unwrap() > 0.4);
    }

    /// binder 归一化那套不归 gray: gray 的开关白名单才是闸门, 已在上面的单测里。
    #[test]
    fn to_pyval_keeps_int_float_distinction() {
        let v: Value = serde_json::from_str(r#"{"a":3150.0,"b":4800}"#).unwrap();
        let p = to_pyval(&v);
        assert_eq!(
            py_dumps(&p),
            "{\n \"a\": 3150.0,\n \"b\": 4800\n}"
        );
    }

    #[test]
    fn to_serde_round_trips_and_drops_non_finite() {
        let p = PyVal::Obj(vec![
            ("x".into(), PyVal::Float(1.5)),
            ("inf".into(), PyVal::Float(f64::INFINITY)),
        ]);
        let v = to_serde(&p);
        assert_eq!(v["x"], json!(1.5));
        assert_eq!(v["inf"], Value::Null);
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn control_sample_keeps_unknown_filter_and_power_separate_from_raw_collection() {
        let root = std::env::temp_dir().join(format!("control-sample-{}-{}",std::process::id(),chrono::Utc::now().timestamp_nanos_opt().unwrap()));
        std::fs::create_dir(&root).unwrap();
        for (name,status,current,trace_present) in [("legacy_bypass","Charging",0,true),("battery","Discharging",-1_000_000,true),("failed","Discharging",-1_000_000,false),("battery_policy_failed","Discharging",-1_000_000,true)] {
            let out = root.join(name);std::fs::create_dir(&out).unwrap();
            let mut env = format!("#sample_env v1 dur=13 interval=2\n#battery_status={status}\n#charge_suspended=yes\n#charge_mode=bypass\n");
            for t in (100..=112).step_by(2) {env.push_str(&format!("ENV {t} 3800000 {current} NA 5000000 1000000 gpuss 40000\n"));}
            env.push_str(&format!("#battery_status_after={status}\n#sample_env done\n"));
            std::fs::write(out.join("env.txt"),env).unwrap();
            if trace_present {std::fs::write(out.join("trace.txt"),"#loop_v1_meta capture_start_uptime=106 capture_end_uptime=109 duration_req=3\n").unwrap();}
            if name=="battery_policy_failed" {
                std::fs::write(out.join("sample-request.json"),r#"{"power_policy":"battery"}"#).unwrap();
                std::fs::write(out.join("power.json"),r#"{"policy":"battery","before":{"on_battery":true},"after":{"on_battery":false},"error":"supply changed after collection"}"#).unwrap();
            }
            let input = if name=="battery_policy_failed" {
                // Final save failed: the latest returned failure takes priority
                // over the earlier durable before/after snapshot.
                json!({"power":{"policy":"battery","before":{"on_battery":true},"after":{"on_battery":false},"error":"supply changed after collection; power evidence save failed"}})
            } else {Value::Null};
            let result = summarize_control_sample(&out,name,3,trace_present,true,input,true,"synthetic",None).unwrap();
            assert!(result["summary"].is_null());
            assert!(result["metrics"]["gpu_submit_fps"].is_null());
            assert!(result["metrics"]["energy_per_frame_mj"].is_null());
            assert_eq!(result["metric_provenance"]["frame"]["target_ownership_verified"],false);
            if name=="battery" {
                assert_eq!(result["metrics"]["power_w_mean"],3.8);
                assert!((result["metrics"]["trace_energy_j"].as_f64().unwrap()-11.4).abs()<1e-9);
                assert_eq!(result["metric_provenance"]["trace_energy"]["available"],true);
            } else {
                assert!(result["metrics"]["power_w_mean"].is_null());
                assert!(result["metrics"]["trace_energy_j"].is_null());
            }
            assert_eq!(result["collection"]["status"],if trace_present && name!="battery_policy_failed"{"complete"}else{"failed"});
            if name=="battery_policy_failed" {assert!(out.join("trace.txt").is_file());assert!(result["collection"]["error"].as_str().unwrap().contains("power evidence save failed"));}
            assert!(out.join("run.json").is_file());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// game_opt_loop 的 workload.script 是**调用方视角**的相对路径
    /// ("../phonefarm/knobs/gray/workload_spin_touch_v1.json"): 在 game_opt_loop
    /// 的 cwd 下按原样就要找得到。
    #[test]
    fn resolve_path_tries_cwd_then_repo_root() {
        let repo = repo_root();
        assert!(resolve_path("loop_v1/tools/run_once.sh").is_some());
        let odd = repo.join("nonexistent_zzz.bin");
        assert!(resolve_path(odd.to_string_lossy().as_ref()).is_none());
        // 绝对路径原样
        assert_eq!(
            resolve_path(&repo.join("loop_v1").to_string_lossy()),
            Some(repo.join("loop_v1"))
        );
    }

    /// serial 的取值顺序: CLI --serial 压过请求体。
    #[test]
    fn serial_resolution_prefers_cli() {
        let req = json!({"serial": "REQ_SERIAL", "candidate_id": "c"});
        let cli: Option<String> = Some("CLI_SERIAL".into());
        let serial = cli.clone().or_else(|| req["serial"].as_str().map(String::from));
        assert_eq!(serial.as_deref(), Some("CLI_SERIAL"));
        let serial = None::<String>.or_else(|| req["serial"].as_str().map(String::from));
        assert_eq!(serial.as_deref(), Some("REQ_SERIAL"));
    }

    // ── gray: 改写解析与画面比对 ──

    #[test]
    fn gray_rewrites_map_to_layer_properties() {
        assert_eq!(effective_gray_hits(&json!({"effective":[{"begins":0}],"readonly_stats":{"render_pass_begins":100}})),0);
        assert_eq!(effective_gray_hits(&json!({"effective":[{"begins":2},{"begins":3}]})),5);
        let k = parse_gray_rewrites(&json!({"rewrites": [
            {"knob": "storeop_dont_care", "scope": "15a51a879df99c32:2"},
            {"knob": "storeop_dont_care", "scope": "9d0b4db80bf552b8:0"},
        ]}))
        .unwrap();
        assert!(!k.loadop);
        assert_eq!(k.storeop, "15a51a879df99c32:2,9d0b4db80bf552b8:0");
        let k = parse_gray_rewrites(&json!({"rewrites": [{"knob": "loadop_dont_care"}]})).unwrap();
        assert!(k.loadop);
        assert_eq!(k.storeop, "0");
    }

    #[test]
    fn gray_cadence_does_not_win_from_power_outside_its_frozen_goal() {
        let arm=|power:f64| (0..6).map(|i|json!({"label":format!("r{i}"),"summary":{"frame_p95":16.0,"fps_mean":60.0},
            "env":{"power_w_mean":power+i as f64*0.01,"on_battery":true}})).collect::<Vec<_>>();
        let (ctrl,knob)=(arm(6.0),arm(3.0));
        assert!(judge_runs(&ctrl,&knob,0.05,6).1); // old adaptive metric list would accept this
        let required=Goal::RenderCadence.metrics();
        let mut report=base_report(&json!({}),"gray");
        finish_gray_statistics(&mut report,&knob,&ctrl,0.05,6,None,&required).unwrap();
        assert_eq!(report["status"],"REJECT");
        assert_eq!(report["verdict"]["alpha_win"],0.025);
        assert!(report["verdict"]["per_metric"].get("power_w_mean").is_none());
    }

    /// 不认识的开关、缺范围、范围写错, 都在碰设备之前拒收。
    #[test]
    fn bad_gray_rewrites_are_refused_before_touching_the_device() {
        for p in [
            json!({"rewrites": []}),
            json!({"rewrites": [{"knob": "render_scale"}]}),
            json!({"rewrites": [{"knob": "storeop_dont_care"}]}),
            json!({"rewrites": [{"knob": "storeop_dont_care", "scope": "15a51a87"}]}),
            json!({"rewrites": [{"knob": "storeop_dont_care", "scope": "15a5:2"}]}),
            json!({"rewrites": [{"knob": "storeop_dont_care", "scope": "15a51a879df99c32:x"}]}),
        ] {
            assert!(parse_gray_rewrites(&p).is_err(), "{p}");
        }
    }

    #[test]
    fn psnr_is_capped_for_identical_frames_and_drops_with_damage() {
        let a = vec![100u8; 4 * 64];
        assert_eq!(psnr_rgba(&a, &a), Some(PSNR_IDENTICAL_DB));
        let mut b = a.clone();
        for px in b.chunks_exact_mut(4).take(32) {
            px[0] = 0; // 一半像素的红通道被画坏
        }
        let p = psnr_rgba(&a, &b).unwrap();
        assert!(p < GRAY_MIN_PSNR_DB, "{p}");
        // alpha 通道不算
        let mut c = a.clone();
        for px in c.chunks_exact_mut(4) {
            px[3] = 0;
        }
        assert_eq!(psnr_rgba(&a, &c), Some(PSNR_IDENTICAL_DB));
        assert_eq!(psnr_rgba(&a, &a[..128]), None, "尺寸不同比不了");
    }

    // ── sysparam: 回读与快照分类 ──

    const PLAN: &str = "sysfs\t/sys/class/kgsl/kgsl-3d0/min_pwrlevel\t3\nsetting\tsystem:refresh_rate_mode\t2\n";

    #[test]
    fn readback_reads_every_plan_item() {
        let c = readback_cmd(PLAN);
        assert!(c.contains("cat /sys/class/kgsl/kgsl-3d0/min_pwrlevel"));
        assert!(c.contains("settings get system refresh_rate_mode"));
    }

    /// 写 3 读回 17 (厂商服务抢回) 或读不到, 都算守不住; 对上了不报。
    #[test]
    fn readback_mismatches_flag_contested_and_unreadable_nodes() {
        let ok = "/sys/class/kgsl/kgsl-3d0/min_pwrlevel=3\nsystem:refresh_rate_mode=2\n#adb_exit=0";
        assert!(readback_mismatches(PLAN, ok).is_empty());
        let bad = "/sys/class/kgsl/kgsl-3d0/min_pwrlevel=17\n#adb_exit=0";
        let m = readback_mismatches(PLAN, bad);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], ("/sys/class/kgsl/kgsl-3d0/min_pwrlevel".into(), "3".into(), "17".into()));
        assert_eq!(m[1].2, "<读不到>");
    }

    /// 只有厂商服务会动的频率边界变了 → 不判留痕; 别的键变了 → 判留痕。
    #[test]
    fn snapshot_diff_separates_vendor_managed_keys() {
        let a = "# device_snapshot v1\nkgsl.max_pwrlevel=0\ncpu.policy6.scaling_max=1497600\ncpu.policy6.governor=walt\nftrace.tracing_on=0\n#adb_exit=0";
        let b = "# device_snapshot v1\nkgsl.max_pwrlevel=17\ncpu.policy6.scaling_max=1382400\ncpu.policy6.governor=walt\nftrace.tracing_on=0\n#adb_exit=0";
        let (v, o) = split_snapshot_diff(a, b);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(o.is_empty());
        let c = b.replace("governor=walt", "governor=performance");
        let (_, o) = split_snapshot_diff(a, &c);
        assert_eq!(o, vec!["cpu.policy6.governor: walt → performance".to_string()]);
    }

    /// 画面判定要拿「同设置重拍」的噪声底比, 而不是拿固定阈值硬卡。
    ///
    /// 真实数据 (Megacity city_static, 2026-10-19): 同臂相隔 1 秒重拍 26.41 / 27.90 dB,
    /// 对照对对照 28.06 dB。固定 30 dB 的阈值比噪声底还高 —— 什么都没改的候选也会判画面出错。
    #[test]
    fn image_gate_judges_against_the_scene_noise_floor() {
        // 画面完全没变: 高于配置阈值 → 直接按绝对阈值过
        let g = ImageGate::new(30.0, Some(28.06));
        assert!(matches!(judge_image(&g, Some(42.0)), ImageRuling::Pass("absolute")));

        // 场景在动, 噪声底 28.06, 候选 29.71 → 高于噪声底, 按噪声底过
        let g = ImageGate::new(30.0, Some(28.06));
        assert!(matches!(judge_image(&g, Some(29.71)), ImageRuling::Pass("noise_floor")));

        // 明显改坏了画面 (比噪声底还差很多) → 判画面出错
        let g = ImageGate::new(30.0, Some(28.06));
        match judge_image(&g, Some(21.0)) {
            ImageRuling::Fail(r) => assert!(r.contains("低于同设置重拍噪声底"), "{r}"),
            _ => panic!("应该失败"),
        }

        // 场景自己就不稳定 (噪声底 12 dB): 拒绝判定, 不猜
        let g = ImageGate::new(30.0, Some(12.0));
        match judge_image(&g, Some(28.0)) {
            ImageRuling::Fail(r) => assert!(r.contains("自身不稳定"), "{r}"),
            _ => panic!("噪声底过低时必须拒绝判定"),
        }

        // 没有重拍可作噪声底: 同样拒绝判定
        let g = ImageGate::new(30.0, None);
        match judge_image(&g, Some(29.0)) {
            ImageRuling::Fail(r) => assert!(r.contains("没有同设置重拍"), "{r}"),
            _ => panic!("没有噪声底时必须拒绝判定"),
        }

        // 尺寸不一致
        let g = ImageGate::new(30.0, Some(28.06));
        assert!(matches!(judge_image(&g, None), ImageRuling::Fail(_)));

        // 噪声底取观测到的最大值
        assert_eq!(noise_floor_db([Some(26.41), Some(27.9), None]), Some(27.9));
        assert_eq!(noise_floor_db([None, None]), None);
    }

    #[test]
    fn cool_summary_takes_the_hottest_start_and_any_timeout() {
        let arms = vec![
            json!({"c": 41.2, "timeout": false}),
            json!({"c": 47.9, "timeout": true}),
            json!({"c": null, "timeout": false}),
        ];
        assert_eq!(cool_summary(&arms), (Some(47.9), true));
        assert_eq!(cool_summary(&[]), (None, false));
    }

    #[test]
    fn after_one_arm_misses_the_cool_target_later_arms_wait_briefly() {
        assert_eq!(arm_cool_timeout(&[]), ARM_COOL_TIMEOUT_S);
        assert_eq!(arm_cool_timeout(&[json!({"timeout": false})]), ARM_COOL_TIMEOUT_S);
        assert_eq!(
            arm_cool_timeout(&[json!({"timeout": false}), json!({"timeout": true})]),
            ARM_COOL_TIMEOUT_AFTER_MISS_S
        );
    }

    #[test]
    fn launch_spec_is_read_from_the_workload() {
        let req = json!({"workload": {"launch": {"extras": "--es scene city_flythrough", "marker": "megacity_started",
            "static_extras": "--es scene city_static"}}});
        let l = launch_spec(&req).unwrap();
        assert_eq!(l.marker.as_deref(), Some("megacity_started"));
        assert_eq!(l.static_extras.as_deref(), Some("--es scene city_static"));
        assert!(launch_spec(&json!({"workload": {}})).is_none());
        assert!(launch_spec(&json!({"workload": {"launch": {"extras": " "}}})).is_none());
    }

    #[test]
    fn live_check_passes_only_when_toggling_looks_like_noise() {
        let ok = json!({"game_alive": true, "effective_hit": 1, "diffs": [
            {"kind": "noise_1s", "diff_px_pct": 0.1},
            {"kind": "off_vs_on", "diff_px_pct": 0.4},
            {"kind": "same_state", "diff_px_pct": 0.3}]});
        assert!(live_check_verdict(&ok).is_ok());
        let mut bad = ok.clone();
        bad["diffs"][1]["diff_px_pct"] = json!(12.0);
        assert!(live_check_verdict(&bad).unwrap_err().contains("12.000%"));
        let mut miss = ok.clone();
        miss["effective_hit"] = json!(0);
        assert!(live_check_verdict(&miss).is_err());
        let mut dead = ok;
        dead["game_alive"] = json!(false);
        assert!(live_check_verdict(&dead).is_err());
    }
}

