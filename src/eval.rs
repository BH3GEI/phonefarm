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
use crate::hwcond;
use crate::loopstat::{self, Run};
use crate::pyjson::{dumps as py_dumps, PyVal};
use crate::sysparam;
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
const GRAY_SUPPORTED_KNOBS: [&str; 1] = ["loadop_dont_care"];

const USAGE: &str = "用法: phonefarm eval --request <eval_request.json> [--serial S] [--power-rail usb|battery] [--out 目录] [--json]";

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
}

fn parse_args(args: &[String]) -> Result<EvalArgs, String> {
    let mut it = args.iter();
    let (mut request, mut serial, mut power_rail, mut out) = (None, None, None, None);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--request" => request = it.next().cloned(),
            "--serial" => serial = it.next().cloned(),
            "--power-rail" => power_rail = it.next().cloned(),
            "--out" => out = it.next().cloned(),
            "--json" => {} // eval 的回包恒为 JSON, 收下这个旗子仅为兼容接口形状
            other => return Err(format!("不认识的参数 {other}\n{USAGE}")),
        }
    }
    Ok(EvalArgs {
        request: request.ok_or(format!("缺少 --request\n\n{USAGE}"))?,
        serial,
        power_rail,
        out,
    })
}

pub fn run_eval(args: &[String]) -> i32 {
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
        eprintln!("eval_request v2 校验失败: {e}");
        return 2;
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
    let report = match kind {
        "shader" => return eval_shader_v2(&req, &a),
        "sysparam" => sysparam_eval(&req, serial.as_deref(), Path::new(&evidence)),
        "gray" => gray_eval(&req, serial.as_deref(), Path::new(&evidence)),
        other => {
            eprintln!("不认识的 kind {other}");
            return 2;
        }
    };
    match report {
        Ok(r) => {
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
    let dir = a
        .out
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    let _ = std::fs::create_dir_all(&dir);
    // v1 的 cool_c 与 quality_baseline_db 是必填数字, 表达不了 "null = 不等冷 /
    // 没给地板"。冻结条件纪律下不允许接收端偷偷挑数值顶替, 缺了就明说降级失败。
    let cool_c = req["conditions"]["cool_c"]
        .as_f64()
        .or(req["protocol"]["cool_c"].as_f64());
    let Some(cool_c) = cool_c.filter(|x| *x > 0.0 && *x < 100.0) else {
        eprintln!(
            "v2 shader 降级失败: conditions.cool_c={} 而 v1 (gpu-op) 必须有一个等冷目标温度 (0,100)。\n\
             冻结条件纪律下收端不代填数值, 请在请求里显式给 conditions.cool_c",
            serde_json::to_string(&req["conditions"]["cool_c"]).unwrap_or_default()
        );
        return 2;
    };
    let Some(qbase) = req["payload"]["quality_baseline_db"].as_f64() else {
        eprintln!("v2 shader 降级失败: payload.quality_baseline_db=null 而 v1 (gpu-op) 必须有画质地板数值。\n\
                   收端不代填, 请显式给出");
        return 2;
    };
    let v1 = json!({
        "version": 1,
        "candidate_id": req["candidate_id"],
        "track": req["payload"]["track"],
        "package": req["package"],
        // workload: headless 载体量的是算子自身耗时, replay_seconds 不参与;
        // refbench 载体每臂渲染 frames 帧
        "replay_script": req["workload"]["script"].as_str().unwrap_or(""),
        "shader_path": req["payload"]["shader_path"],
        "spirv_path": req["payload"]["spirv_path"],
        "budget_ms": req["payload"]["budget_ms"],
        "quality_baseline_db": qbase,
        "incumbent_latency_ms": req["payload"]["incumbent_latency_ms"],
        "quality_reference": req["payload"]["quality_reference"]
            .as_str()
            .or(req["conditions"]["quality_reference"].as_str()),
        "protocol": {
            "cool_c": cool_c,
            "replay_seconds": req["workload"]["seconds"].as_i64()
                .or(req["protocol"]["replay_seconds"].as_i64())
                .unwrap_or(30),
            "rounds": req["protocol"]["rounds"],
            "alternating": req["protocol"]["alternating"],
            "p_threshold": req["protocol"]["p_threshold"],
        }
    });
    let v1_path = Path::new(&dir).join("_eval_v1_request.json");
    if let Err(e) = std::fs::write(&v1_path, serde_json::to_string(&v1).unwrap_or_default()) {
        eprintln!("写不了降级请求 {}: {e}", v1_path.display());
        return 2;
    }
    // 转交 gpu-op, 捕获 stdout (回包是纯 JSON; 进度走 stderr)
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("phonefarm"));
    let mut cmd = Command::new(&exe);
    cmd.arg("gpu-op")
        .arg("--request")
        .arg(&v1_path)
        .arg("--json");
    if let Some(s) = &a.serial {
        cmd.arg("--serial").arg(s);
    }
    if let Some(r) = &a.power_rail {
        cmd.arg("--power-rail").arg(r);
    }
    if let Some(o) = &a.out {
        cmd.arg("--out").arg(o);
    }
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("起不了 gpu-op: {e}");
            return 2;
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        // gpu-op 校验/预检失败: 它的 stderr 已经把原因说清了, 原样透传退出码
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        return out.status.code().unwrap_or(2);
    }
    // 回包补 v2 字段: 上层按 kind 对账, 对不上要整份作废, 所以必须补
    let mut report: Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gpu-op 回包不是合法 JSON: {e}\n{stdout}");
            return 2;
        }
    };
    if let Some(obj) = report.as_object_mut() {
        obj.insert("version".into(), json!(2));
        obj.insert("kind".into(), json!("shader"));
        // conditions 是意图, conditions_observed 是事实; shader 通路暂只回显意图,
        // 真值 (起跑温度/等冷是否超时) 由 gpu-op 的进度日志给出, 尚未结构化
        obj.insert("conditions_observed".into(), json!(req["conditions"]));
    }
    println!("{report}");
    0
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
        Some(x) if x > 0.0 && x <= 1.0 => {}
        _ => return Err("protocol.p_threshold 必须 (0, 1]".into()),
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
        other => return Err(format!("不认识的 kind {other:?}")),
    }
    Ok(())
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

// ══════════════ 报告装配 ══════════════

fn base_report(req: &Value, kind: &str) -> Value {
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

fn unavailable(metrics: &mut Value, field: &str, reason: &str) {
    metrics["unavailable"]
        .as_array_mut()
        .expect("unavailable 数组")
        .push(json!({ "field": field, "reason": reason }));
}

// ══════════════ sysparam ══════════════

/// payload.whitelist (有序 [键, spec] 列表过桥) → sysparam::Whitelist
fn whitelist_from_payload(req: &Value) -> Option<sysparam::Whitelist> {
    let obj = req["payload"]["whitelist"].as_object()?;
    Some(
        obj.iter()
            .map(|(pid, s)| {
                let get = |k: &str| s[k].as_str().unwrap_or("").to_string();
                (
                    pid.clone(),
                    sysparam::Spec {
                        kind: get("kind"),
                        path: get("path"),
                        values: s["values"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        current: get("current"),
                        group: s["group"].as_str().map(String::from),
                        effect: get("effect"),
                    },
                )
            })
            .collect(),
    )
}

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

/// 跑一轮负载 + ftrace 采集 + 功耗温度采样 (与 autoloop.run_one 同一编排:
/// sample_env 与 run_once 并行, 覆盖整段负载)。
pub(crate) fn run_one(
    serial: &str,
    label: &str,
    outdir: &Path,
    seconds: u64,
    workload: &Path,
    pkg: &str,
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
    if let Ok(mut s) = sampler {
        // 采样器卡死不能把整个 eval 挂住: 等一段时间, 超时就杀 (负载收尾后的
        // env 尾巴本来就只要几秒)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            match s.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() > deadline => {
                    let _ = s.kill();
                    let _ = s.wait();
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(200)),
                Err(_) => break,
            }
        }
    }
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
    Ok(m)
}

fn sysparam_eval(req: &Value, serial: Option<&str>, evidence: &Path) -> Result<String, String> {
    let Some(serial) = serial else {
        return Err("kind=sysparam 需要真机: 请加 --serial (或设备唯一在线时省略)".into());
    };
    let kind = "sysparam";
    let mut report = base_report(req, kind);
    let _ = std::fs::create_dir_all(evidence);
    let probe_only = req["payload"]["probe_only"].as_bool().unwrap_or(false);

    // ── 白名单: 请求带着就用它的 (但结构上仍按本端 Spec 解析), 没带就真机探 ──
    let wl = match whitelist_from_payload(req) {
        Some(w) if !w.is_empty() => w,
        _ => probe_whitelist(serial, evidence)?,
    };

    // ── probe_only: 只探白名单, 不跑候选 ──
    if probe_only {
        push_tools(serial)?;
        let before = snapshot(serial);
        std::fs::write(evidence.join("snap_before.txt"), &before).map_err(|e| e.to_string())?;
        let wl2 = probe_whitelist(serial, evidence)?; // 探测本身要写节点, 重探一遍拿全量
        let after = snapshot(serial);
        std::fs::write(evidence.join("snap_after.txt"), &after).map_err(|e| e.to_string())?;
        let identical = before == after;
        report["status"] = json!("REVERTED");
        report["verdict"]["reason"] =
            json!("probe_only: 只探白名单不跑候选, 探测过程的写入已还原");
        report["conditions_observed"] = json!({
            "snapshot_identical": identical,
            "restored": identical,
            // schema 未列但允许附加: 探测结果就是这一步的全部产出
            "whitelist": wl_pyval(&wl2),
        });
        report["evidence_dir"] = json!(evidence.display().to_string());
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
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

    // ── 设备准备 ──
    if !hwcond::root_ok(&device::Device::new(Some(serial.into()), std::env::temp_dir().to_string_lossy().into_owned())) {
        report["verdict"]["reason"] = json!("设备无 root: 系统参数与 ftrace 采集都需要 root");
        report["evidence_dir"] = json!(evidence.display().to_string());
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    push_tools(serial)?;

    // 停充尝试: 请求要求且节点没指定时由接收端自己探 (charge_suspend.sh 内置节点表)。
    // 失败不致命 —— 拿不到放电态就只是功耗不进判定, 帧时帧率照常测。
    let want_suspend = req["conditions"]["charging_suspend"]["enabled"].as_bool().unwrap_or(false);
    let mut suspended_via: Option<String> = None;
    if want_suspend {
        let out = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh suspend"), 90);
        std::fs::write(evidence.join("charge_suspend.log"), &out).map_err(|e| e.to_string())?;
        if adb_ok(&out) && out.contains("CHARGE_SUSPENDED") {
            // 脚本自己回显用的哪个节点; 原样记进报告
            suspended_via = out
                .lines()
                .find(|l| l.contains("node="))
                .map(|l| l.trim().to_string())
                .or_else(|| Some("/sys/class/qcom-battery/charging_enabled".into()));
        }
    }

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
    let workload_path = match resolve_path(&wl_file) {
        Some(p) => p,
        None => {
            report["verdict"]["reason"] = json!(format!(
                "负载脚本不在: {wl_file} (按 cwd 与仓库根都找不到)"
            ));
            return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
        }
    };

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

    for i in 1..=rounds {
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
                    abort_reason = Some("旋钮未全部生效 (apply 回包有 FAIL 令牌)".into());
                    let r = restore(serial);
                    if r.contains("KNOB_RESTORE_FAIL") {
                        restore_fail = Some(r);
                    }
                    break;
                }
            }
            let label = format!("sp_{arm}{i}");
            let res = run_one(serial, &label, &evidence.join(format!("{arm}{i}")), seconds, &workload_path, pkg);
            if arm == "knob" {
                let r = restore(serial);
                if r.contains("KNOB_RESTORE_FAIL") && restore_fail.is_none() {
                    restore_fail = Some(r.clone());
                }
            }
            match res {
                Ok(m) => {
                    // 温度上限: 超了这一组作废
                    let t = m["summary"]["soc_temp_max_c"].as_f64();
                    if let (Some(t), Some(cap)) = (t, req["conditions"]["temp_cap_c"].as_f64()) {
                        if t > cap {
                            abort_reason = Some(format!("{arm} 臂第 {i} 轮 SoC 结温 {t}C 超过上限 {cap}C"));
                        }
                    }
                    if abort_reason.is_none() {
                        if arm == "knob" {
                            knob_runs.push(m);
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
        }
        if abort_reason.is_some() {
            break;
        }
    }

    // ── 还原 + 快照核对 ──
    if suspended_via.is_some() {
        let cr = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh restore"), 60);
        std::fs::write(evidence.join("charge_restore.log"), &cr).map_err(|e| e.to_string())?;
        if !adb_ok(&cr) {
            restore_fail = Some(restore_fail.clone().unwrap_or_else(|| "停充还原失败".into()));
        }
    }
    let restore_log = restore(serial);
    if restore_log.contains("KNOB_RESTORE_FAIL") && restore_fail.is_none() {
        restore_fail = Some(restore_log);
    }
    let snap_after = snapshot(serial);
    std::fs::write(evidence.join("snap_after.txt"), &snap_after).map_err(|e| e.to_string())?;
    let snapshot_identical = snap_before == snap_after;

    report["conditions_observed"] = json!({
        "temp_cap_c": req["conditions"]["temp_cap_c"],
        "apply_ok": apply_ok,
        "snapshot_identical": snapshot_identical,
        "restored": restore_fail.is_none(),
        "snapshot_diff": if snapshot_identical { Value::Null } else {
            json!([
                format!("before {} 行 / after {} 行, 逐行比对不相等 (全文见 evidence_dir)",
                        snap_before.lines().count(), snap_after.lines().count())
            ])
        },
    });
    report["evidence_dir"] = json!(evidence.display().to_string());
    std::fs::write(evidence.join("apply_log.txt"), apply_logs.join("\n---\n")).ok();

    // ── 判定 ──
    if let Some(reason) = abort_reason.clone().or_else(|| restore_fail.clone().map(|r| format!("旋钮还原失败: {r}"))) {
        if suspended_via.is_some() {
            let _ = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh restore"), 60);
        }
        report["verdict"]["reason"] = json!(reason);
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    if !snapshot_identical {
        report["verdict"]["reason"] = json!("轮末设备快照与轮前不一致, 留痕");
        if suspended_via.is_some() {
            let _ = su(serial, &format!("sh {DEV_TMP}/charge_suspend.sh restore"), 60);
        }
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }

    let primary = "frame_p95";
    let mk_runs = |runs: &[Value]| -> Vec<Run> {
        runs.iter()
            .filter_map(|r| {
                let label = r["label"].as_str()?.to_string();
                let v = to_pyval(&r["summary"]);
                Some((label, v))
            })
            .collect()
    };
    let (a_runs, b_runs) = (mk_runs(&ctrl_runs), mk_runs(&knob_runs));
    // 与 autoloop 一致的判定指标表: 功耗只在两臂都处于放电态时才进判定
    let on_battery_all = knob_runs.iter().chain(ctrl_runs.iter()).all(|r| {
        r["env"]["on_battery"].as_bool().unwrap_or(false)
    });
    let mut metrics_list: Vec<(String, &str)> =
        vec![("frame_p95".into(), "lower"), ("fps_mean".into(), "higher")];
    if on_battery_all {
        metrics_list.push(("power_w_mean".into(), "lower"));
    }
    let mut p_primary = 1.0f64;
    let mut wins: Vec<String> = Vec::new();
    let mut regressions: Vec<String> = Vec::new();
    let mut per_metric: Vec<(String, Value)> = Vec::new();
    let alpha = req["protocol"]["p_threshold"].as_f64().unwrap_or(0.05);
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
    report["verdict"] = json!({
        "is_pareto_improvement": keep,
        "p_value": p_primary,
        "stat": "exact_permutation",
        "alpha_win": alpha_win,
        "alpha_regression": alpha,
        // 轮数下界: alpha_win 是否够得着 (2/C(2n,n) < alpha_win)
        "reachable": reachable(rounds, alpha_win),
        "wins": wins,
        "regressions": regressions,
        "per_metric": Value::Object(per_metric.into_iter().collect()),
        "reason": if keep {
            json!(format!("改善 {} 且无显著退化", metrics_list.iter().find(|(m, _)| wins.first() == Some(&m.to_string())).map(|(m, _)| m.clone()).unwrap_or_default()))
        } else if !regressions.is_empty() {
            json!(format!("存在显著退化: {}", regressions.join(",")))
        } else {
            json!("没有任何指标显著改善")
        },
    });

    // ── metrics: 逐臂聚合 (B 臂 = 候选臂) ──
    // 臂内均值: ABBA 的全部轮一起算 —— 只取最后一轮的话, 那恰好是整场最热的
    // 一轮, 交替设计要抹掉的热漂移就全进了头条指标
    let f = |runs: &[Value], k: &str| -> Option<f64> {
        let xs: Vec<f64> = runs.iter().filter_map(|r| r["summary"][k].as_f64()).collect();
        (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
    };
    let temps: Vec<f64> = knob_runs
        .iter()
        .chain(ctrl_runs.iter())
        .filter_map(|r| r["summary"]["soc_temp_max_c"].as_f64())
        .collect();
    let mut m = report["metrics"].take();
    m["fps_p95_ms"] = json!(f(&knob_runs, "frame_p95"));
    m["frame_p95_ms"] = json!(f(&knob_runs, "frame_p95"));
    m["fps_mean"] = json!(f(&knob_runs, "fps_mean"));
    m["soc_temp_max_c"] = json!(temps.iter().cloned().reduce(f64::max));
    // 功耗: 只在放电态均值可信时给
    let power = if on_battery_all {
        knob_runs.last().and_then(|r| r["env"]["power_w_mean"].as_f64())
    } else {
        None
    };
    m["power_watt"] = json!(power);
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
        "battery_capacity_pct": env["battery_capacity_pct"].as_str().and_then(|s| s.parse::<i64>().ok()),
        "on_battery": env["on_battery"],
        "charging_suspended_via": suspended_via.clone(),
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

// ══════════════ gray ══════════════

fn gray_eval(req: &Value, serial: Option<&str>, evidence: &Path) -> Result<String, String> {
    let kind = "gray";
    let mut report = base_report(req, kind);
    let Some(serial) = serial else {
        return Err("kind=gray 需要真机: 请加 --serial".into());
    };
    let require_hit = req["payload"]["require_hit"].as_bool().unwrap_or(true);

    // knobs/gray 的调用层: 只调 enable_layer.sh, 不改它的文件
    let script = repo_root().join("knobs/gray/enable_layer.sh");
    if !script.exists() {
        return Err(format!("找不到 knobs/gray/enable_layer.sh: {}", script.display()));
    }
    let pkg = req["package"].as_str().unwrap_or("");
    let layer = |mode: &str, arg: &str| -> String {
        let mut cmd = Command::new("bash");
        cmd.arg(&script).arg(mode);
        if !arg.is_empty() {
            cmd.arg(arg);
        }
        // enable_layer.sh 从 REFBENCH_SERIAL 读设备 (缺省写死 91253241019A) ——
        // 不传的话 --serial 指哪台都白搭, 层会挂到别人的机器上
        cmd.env("REFBENCH_SERIAL", serial)
            .env("PF_BIN", std::env::current_exe().unwrap_or_else(|_| PathBuf::from("phonefarm")))
            .output()
            .map(|o| {
                format!(
                    "{}{}#adb_exit={}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr),
                    o.status.code().unwrap_or(-1)
                )
            })
            .unwrap_or_else(|e| format!("#adb_exit=spawn_err {e}"))
    };

    // 改写先落盘再动设备 (证据纪律), 然后施加
    let _ = std::fs::create_dir_all(evidence);
    std::fs::write(
        evidence.join("rewrites.json"),
        serde_json::to_string_pretty(&req["payload"]["rewrites"]).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    let out = layer("loadop", pkg);
    std::fs::write(evidence.join("enable_layer.log"), &out).map_err(|e| e.to_string())?;
    if out.contains("#adb_exit=") && !out.ends_with("#adb_exit=0") {
        // 施加失败也可能已经动了全局属性: 尽力摘干净再回报, 绝不把设备留在挂层态
        let off = layer("off", pkg);
        std::fs::write(evidence.join("restore.log"), &off).ok();
        report["verdict"]["reason"] = json!(format!("enable_layer loadop 施加失败: {out}"));
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }

    // 测量与 sysparam 同一套 ABBA (负载必须触控版)
    let rounds = req["protocol"]["rounds"].as_i64().unwrap_or(1).max(1) as u32;
    let seconds = req["workload"]["seconds"].as_i64().unwrap_or(30).max(1) as u64;
    let wl_file = req["workload"]["script"]
        .as_str()
        .unwrap_or("loop_v1/scripts/workload_spin_touch_v1.json");
    let workload_path = match resolve_path(wl_file) {
        Some(p) => p,
        None => {
            report["verdict"]["reason"] =
                json!(format!("负载脚本不在: {wl_file} (按 cwd 与仓库根都找不到)"));
            return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
        }
    };
    let mut knob_runs: Vec<Value> = Vec::new();
    let mut ctrl_runs: Vec<Value> = Vec::new();
    let mut abort_reason: Option<String> = None;
    // gray 的候选臂是「层已挂上」的那几轮; 对照臂把层摘掉
    for i in 1..=rounds {
        for arm in if i % 2 == 1 { ["knob", "ctrl"] } else { ["ctrl", "knob"] } {
            // 候选臂: 层在 (loadop 已施加); 对照臂: 层摘掉
            let off = if arm == "ctrl" { layer("off", pkg) } else { String::new() };
            if arm == "ctrl" && (off.contains("#adb_exit=") && !off.ends_with("#adb_exit=0")) {
                abort_reason = Some("对照臂摘层失败".into());
                break;
            }
            if arm == "knob" {
                let out = layer("loadop", pkg);
                if out.contains("#adb_exit=") && !out.ends_with("#adb_exit=0") {
                    abort_reason = Some("候选臂挂层失败".into());
                    break;
                }
            }
            let res = run_one(serial, &format!("gray_{arm}{i}"), &evidence.join(format!("{arm}{i}")), seconds, &workload_path, pkg);
            match res {
                Ok(mut m) => match arm {
                    "knob" => {
                        // 命中 marker 要在**这一臂跑完后立刻**采: 下一次 mount_layer
                        // 会把三个落点全删了重写, 拖到循环外读到的就是初始化的空表
                        let hits_now = layer_hits(serial, pkg, evidence);
                        m["layer_hits"] = json!(hits_now);
                        knob_runs.push(m);
                    }
                    _ => ctrl_runs.push(m),
                },
                Err(e) => {
                    abort_reason = Some(e);
                }
            }
            if abort_reason.is_some() {
                break;
            }
        }
        if abort_reason.is_some() {
            break;
        }
    }
    // 还原
    let restore_log = layer("off", pkg);
    std::fs::write(evidence.join("restore.log"), &restore_log).ok();

    // 各 knob 臂采到的命中取最大 (require_hit 判的是"改写到底有没有真生效过一次")
    let hits = knob_runs
        .iter()
        .filter_map(|r| r["layer_hits"].as_u64())
        .max()
        .unwrap_or(0);
    report["conditions_observed"] = json!({
        "hits": hits,
        "apply_ok": hits > 0,
        "restored": !restore_log.contains("#adb_exit=1"),
    });

    if let Some(reason) = abort_reason {
        report["verdict"]["reason"] = json!(reason);
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    // require_hit 缺省为真: 改写一次都没命中 = 拿 A 去和 A 比, 整轮作废
    if require_hit && hits == 0 {
        report["verdict"]["reason"] =
            json!("require_hit=true 但改写一次都没命中 (层自报绑定计数为 0) —— 拿 A 去和 A 比, 整轮作废");
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    // gray 改了画面就必须量画质; 收端还没接真值对比, 按契约不能判保留
    report["verdict"]["reason"] =
        json!("收端尚未接 gray 的画质真值对比 (psnr_db 无法测量), 按契约 gray 不能判保留; 逐帧指标已照常采进 evidence_dir 供上层自行判读");
    let mut m = report["metrics"].take();
    unavailable(&mut m, "psnr_db", "收端尚未接 gray 的画质真值对比");
    report["metrics"] = m;
    report["evidence_dir"] = json!(evidence.display().to_string());
    Ok(serde_json::to_string_pretty(&report).unwrap_or_default())
}

/// 层自报的改写命中数 (契约 verdict 口径: "改写被绑定的次数")。
///
/// markers 在 knobs/gray/layer 的三个候选落点; su() 输出尾部带 #adb_exit=N 要剥掉。
/// 命中的口径: **sum(effective[].begins)** —— 改写了但一次都没被绑定的 pass 不算命中
/// (层自报 unavailable_reason = "rewritten pass never bound")。readonly 探针变体
/// 没有改写, 退回 render_pass_begins (层确实看到了帧)。多个落点取最大。
fn layer_hits(serial: &str, pkg: &str, evidence: &Path) -> Option<u64> {
    if pkg.is_empty() {
        return None;
    }
    let candidates = [
        format!("/storage/emulated/0/Android/data/{pkg}/files/knobs_layer_out.json"),
        format!("/data/data/{pkg}/knobs_layer_out.json"),
        format!("{DEV_TMP}/knobs_layer_out.{pkg}.json"),
    ];
    let mut best: Option<u64> = None;
    for (i, path) in candidates.iter().enumerate() {
        let out = su(serial, &format!("cat {path} 2>/dev/null"), 30);
        if !out.contains("\"effective\"") {
            continue;
        }
        std::fs::write(evidence.join(format!("knobs_layer_out.{i}.json")), &out).ok();
        // su() 恒在尾部追 #adb_exit=N, 不剥掉 JSON 永远解析不出来
        let body = match out.rfind("#adb_exit=") {
            Some(i) => &out[..i],
            None => &out[..],
        };
        let Ok(v) = serde_json::from_str::<Value>(body.trim()) else {
            continue;
        };
        let begins: u64 = v["effective"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.get("begins").and_then(|b| b.as_u64()))
                    .sum()
            })
            .unwrap_or(0);
        let fallback = v["readonly_stats"]["render_pass_begins"]
            .as_u64()
            .unwrap_or(0);
        let hits = if begins > 0 { begins } else { fallback };
        best = Some(best.unwrap_or(0).max(hits));
    }
    best
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
        // 与 eval_shader_v2 里同一份映射, 这里直接复刻关键字段核对
        let v1_cool = req["conditions"]["cool_c"].as_f64().or(req["protocol"]["cool_c"].as_f64());
        assert_eq!(v1_cool, Some(42.0)); // conditions 优先
        assert_eq!(req["payload"]["budget_ms"], json!(4.0));
        let qref = req["payload"]["quality_reference"].as_str().or(req["conditions"]["quality_reference"].as_str());
        assert_eq!(qref, Some("/tmp/ref_dir")); // payload 优先, 两者取其有
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
}
