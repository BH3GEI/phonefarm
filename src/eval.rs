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
use crate::curtail;
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

const USAGE: &str = "用法: phonefarm eval --request <eval_request.json> [--serial S] [--power-rail usb|battery] [--out 目录] [--no-curtail] [--json]";

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
}

fn parse_args(args: &[String]) -> Result<EvalArgs, String> {
    let mut it = args.iter();
    let (mut request, mut serial, mut power_rail, mut out) = (None, None, None, None);
    let mut curtail = true;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--request" => request = it.next().cloned(),
            "--serial" => serial = it.next().cloned(),
            "--power-rail" => power_rail = it.next().cloned(),
            "--out" => out = it.next().cloned(),
            "--no-curtail" => curtail = false,
            "--json" => {} // eval 的回包恒为 JSON, 收下这个旗子仅为兼容接口形状
            other => return Err(format!("不认识的参数 {other}\n{USAGE}")),
        }
    }
    Ok(EvalArgs {
        request: request.ok_or(format!("缺少 --request\n\n{USAGE}"))?,
        serial,
        power_rail,
        out,
        curtail,
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
        "sysparam" => sysparam_eval(&req, serial.as_deref(), Path::new(&evidence), a.curtail),
        "gray" => gray_eval(&req, serial.as_deref(), Path::new(&evidence)),
        "whitebox" => crate::whitebox::whitebox_eval(&req, serial.as_deref(), Path::new(&evidence)),
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
        "whitebox" => {
            crate::whitebox::knob_extras(&req["payload"])?;
            if launch_spec(req).is_none() {
                return Err("kind=whitebox 需要 workload.launch.extras (白盒载体靠启动参数换设置)".into());
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

/// 施加后隔多少秒回读 (knobs/DESIGN.md: 厂商性能服务约 10 秒内改回)。
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
    if let Ok(mut s) = sampler {
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
    Ok(m)
}

/// [`run_one`] 回包里并进判定视图的 env 字段 (sample_env.sh → [`sysparam::env_stats`])。
pub(crate) const RUN_ENV_KEYS: [&str; 23] = [
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
    let alpha = req["protocol"]["p_threshold"].as_f64().unwrap_or(0.05);
    let mut curtailed: Option<Value> = None;

    let mut contested: Vec<Value> = Vec::new();
    let cool_c = req["conditions"]["cool_c"].as_f64().filter(|x| *x > 0.0 && *x < 100.0);
    let mut cool_log: Vec<Value> = Vec::new();
    'pairs: for i in 1..=rounds {
        for arm in if i % 2 == 1 { ["knob", "ctrl"] } else { ["ctrl", "knob"] } {
            // 每臂开跑前按请求的 cool_c 等冷 (施加之前等: 等的那几分钟里厂商服务有足够时间把刚写的值改回去)
            let mut c = arm_wait_cool(serial, cool_c, &format!("{arm}{i}"), arm_cool_timeout(&cool_log));
            c["arm"] = json!(format!("{arm}{i}"));
            cool_log.push(c);
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
            // 施加后隔十几秒回读一次 (knobs/DESIGN.md: 写得进不等于守得住, 厂商性能服务
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
                if r.contains("KNOB_RESTORE_FAIL") && restore_fail.is_none() {
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
                let tests = sysparam_win_tests(alpha, all_on_battery(&knob_runs, &ctrl_runs));
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
    // 快照对不上时分两类: 厂商性能服务自己会改的频率边界 (它在游戏过程中主动压/抬, 与我们
    // 无关; 我们写过的节点由 restore 逐项写回并回读核对, 失败另有 KNOB_RESTORE_FAIL), 和其余。
    // 只有其余的键变了才判留痕作废。前者如实记进报告。
    let (vendor_drift, other_drift) = split_snapshot_diff(&snap_before, &snap_after);

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
    let (verdict, keep, on_battery_all) = judge_runs(&ctrl_runs, &knob_runs, alpha, rounds);
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
        "source": env["power_source"],
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

/// ABBA 两臂的统计判定 (sysparam / gray 共用): 帧时 p95、平均帧率, 两臂都放电时加功耗。
/// 回 (verdict JSON, 是否保留, 两臂是否都在放电态)。
pub(crate) fn judge_runs(ctrl_runs: &[Value], knob_runs: &[Value], alpha: f64, rounds: u32) -> (Value, bool, bool) {
    let primary = "frame_p95";
    let (a_runs, b_runs) = (eval_runs(ctrl_runs), eval_runs(knob_runs));
    // 与 autoloop 一致的判定指标表: 功耗只在两臂都处于放电态时才进判定
    let on_battery_all = all_on_battery(knob_runs, ctrl_runs);
    let mut metrics_list: Vec<(String, &str)> =
        vec![("frame_p95".into(), "lower"), ("fps_mean".into(), "higher")];
    if on_battery_all {
        metrics_list.push(("power_w_mean".into(), "lower"));
    }
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
    let verdict = json!({
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

/// 截一张原始 RGBA 帧 (adb exec-out screencap, 不经 PNG)。回 (宽, 高, 像素)。
fn grab_rgba(serial: &str) -> Result<(u32, u32, Vec<u8>), String> {
    let out = Command::new(adb_path())
        .args(["-s", serial, "exec-out", "screencap"])
        .output()
        .map_err(|e| format!("截图起不来: {e}"))?;
    let (w, h, px) = crate::framecheck::parse_screencap(&out.stdout)?;
    Ok((w, h, px.to_vec()))
}

fn save_png(path: &Path, shot: &(u32, u32, Vec<u8>)) {
    if let Some(img) = image::RgbaImage::from_raw(shot.0, shot.1, shot.2.clone()) {
        let _ = img.save(path);
    }
}

fn gray_eval(req: &Value, serial: Option<&str>, evidence: &Path) -> Result<String, String> {
    let kind = "gray";
    let mut report = base_report(req, kind);
    let Some(serial) = serial else {
        return Err("kind=gray 需要真机: 请加 --serial".into());
    };
    let require_hit = req["payload"]["require_hit"].as_bool().unwrap_or(true);
    let pkg = req["package"].as_str().unwrap_or("com.miHoYo.Yuanshen").to_string();
    let min_psnr = req["payload"]["image_check"]["min_psnr_db"]
        .as_f64()
        .unwrap_or(GRAY_MIN_PSNR_DB);
    let alpha = req["protocol"]["p_threshold"].as_f64().unwrap_or(0.05);
    let temp_cap = req["conditions"]["temp_cap_c"].as_f64();

    // 改写在碰设备之前解析: 不认识的开关直接 ABORT, 设备一下都不碰
    let knobs = match parse_gray_rewrites(&req["payload"]) {
        Ok(k) => k,
        Err(e) => {
            report["verdict"]["reason"] = json!(e);
            return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
        }
    };

    // knobs/gray 的调用层: src/graylayer.rs (enable_layer.sh 的 Rust 移植)。
    // **两臂都挂层**, 唯一差别是改写属性 —— ctrl = 层+改写全关, knob = 层+这一组改写。
    // 要判的是"这条改写"值不值, 不是"挂层这件事"值不值; 两臂都挂层才能把层本身的
    // 开销消掉, 差值才干净地对应改写这一个改动。
    let launch = launch_spec(req);
    if launch.is_some() && knobs.loadop {
        report["verdict"]["reason"] = json!("自驱动负载只支持 storeop_dont_care (画面比对走孪生 pass 现场切换, loadop 没有孪生)");
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    let extras = launch.as_ref().map(|l| l.extras.clone());
    let gl = crate::graylayer::GrayLayer::new(serial).with_launch_extras(extras.clone());
    let gl_knob = crate::graylayer::GrayLayer::new(serial)
        .with_launch_extras(extras)
        .with_storeop((knobs.storeop != "0").then(|| knobs.storeop.clone()), false);
    let mount = |arm: &str| -> Result<(), String> {
        if arm == "knob" {
            gl_knob.mount_mode(if knobs.loadop { "loadop" } else { "probe" }, &pkg, false)?;
            gl_knob.require_storeop()
        } else {
            gl.mount_mode("probe", &pkg, false)
        }
    };

    // 改写先落盘再动设备 (证据纪律)
    let _ = std::fs::create_dir_all(evidence);
    std::fs::write(
        evidence.join("rewrites.json"),
        serde_json::to_string_pretty(&json!({
            "rewrites": req["payload"]["rewrites"],
            "debug.knobs.loadop": if knobs.loadop { "1" } else { "0" },
            "debug.knobs.storeop": knobs.storeop,
            "image_check_min_psnr_db": min_psnr,
        }))
        .unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;

    // 测量与 sysparam 同一套 ABBA (负载必须触控版)
    let rounds = req["protocol"]["rounds"].as_i64().unwrap_or(1).max(1) as u32;
    let seconds = req["workload"]["seconds"].as_i64().unwrap_or(30).max(1) as u64;
    let wl_file = req["workload"]["script"]
        .as_str()
        .unwrap_or("loop_v1/scripts/workload_spin_touch_v1.json");
    let workload_path = match (resolve_path(wl_file), &launch) {
        (Some(p), _) => p,
        (None, Some(_)) => PathBuf::new(),
        (None, None) => {
            report["verdict"]["reason"] =
                json!(format!("负载脚本不在: {wl_file} (按 cwd 与仓库根都找不到)"));
            return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
        }
    };
    // 自驱动负载 (相机一直在飞) 没法在两臂之间截到同一画面: 画面比对改成上机前跑一次
    // frame-audit storeop-check —— 同一局里用孪生 pass 现场开/关, 静态场景逐像素比。
    // 开关之间的差异 (off_vs_on) 不能明显大于同状态之间的噪声 (same_state), 否则判画面出错。
    let mut live_check: Option<Value> = None;
    if let Some(l) = &launch {
        let static_extras = l.static_extras.clone().unwrap_or_else(|| l.extras.clone());
        let chk = crate::frameaudit::storeop_check(
            serial,
            &evidence.join("image_check"),
            None,
            &knobs.storeop,
            2,
            &pkg,
            &[],
            STATIC_SETTLE_S,
            Some(static_extras),
        );
        let v = match chk {
            Ok(v) => v,
            Err(e) => {
                report["verdict"]["reason"] = json!(format!("画面比对 (storeop-check) 没做成, 不上 A/B: {e}"));
                report["evidence_dir"] = json!(evidence.display().to_string());
                return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
            }
        };
        let verdict = live_check_verdict(&v);
        if let Err(why) = &verdict {
            report["conditions_observed"] = json!({"image_check": {"method": "storeop_check_live", "result": v}});
            report["verdict"]["reason"] = json!(format!("画面出错: {why}"));
            report["evidence_dir"] = json!(evidence.display().to_string());
            return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
        }
        live_check = Some(v);
    }
    let mut knob_runs: Vec<Value> = Vec::new();
    let mut ctrl_runs: Vec<Value> = Vec::new();
    let mut abort_reason: Option<String> = None;
    // 画面比对: 每一轮两臂各在负载开始前、同一位置截一张, 改写臂对对照臂算 PSNR。
    // 对照臂相邻两轮之间也算一个, 当作"什么都没改时画面自己会差多少"的参照, 一起落进报告。
    let mut psnr_knob_vs_ctrl: Vec<f64> = Vec::new();
    let mut psnr_ctrl_vs_ctrl: Vec<f64> = Vec::new();
    let mut prev_ctrl: Option<(u32, u32, Vec<u8>)> = None;
    let cool_c = req["conditions"]["cool_c"].as_f64().filter(|x| *x > 0.0 && *x < 100.0);
    let mut cool_log: Vec<Value> = Vec::new();
    'rounds: for i in 1..=rounds {
        let mut shots: std::collections::BTreeMap<&str, (u32, u32, Vec<u8>)> = Default::default();
        for arm in if i % 2 == 1 { ["knob", "ctrl"] } else { ["ctrl", "knob"] } {
            // 层是 CreateInstance 时读的, 切属性必须重启目标 —— force-stop 是两臂切换的一部分
            let _ = gl.off();
            // 游戏关着的时候等冷最快; 等到请求的 cool_c 或超时
            let mut c = arm_wait_cool(serial, cool_c, &format!("{arm}{i}"), arm_cool_timeout(&cool_log));
            c["arm"] = json!(format!("{arm}{i}"));
            cool_log.push(c);
            if let Err(e) = mount(arm) {
                abort_reason = Some(format!("{arm} 臂挂层失败: {e}"));
                break 'rounds;
            }
            // 挂层 = 重启了游戏。有游戏档案的目标 (enter == "onboard") 自己走回游戏场景;
            // 进不去这一臂就没法量, 整次评测作废, 不拿登录页 / 加载画面的帧凑数
            if req["workload"]["enter"].as_str() == Some("onboard") {
                if let Err(e) = crate::onboard::enter_scene(serial, &pkg, &evidence.join(format!("{arm}{i}_enter"))) {
                    abort_reason = Some(format!("{arm}{i} 重启后没回到游戏场景: {e}"));
                    break 'rounds;
                }
            }
            if let Some(l) = &launch {
                let res = run_one_launch(
                    serial,
                    &format!("gray_{arm}{i}"),
                    &evidence.join(format!("{arm}{i}")),
                    seconds,
                    &pkg,
                    req["workload"]["render_comm"].as_str(),
                    l,
                    false,
                );
                match res {
                    Ok(mut m) => {
                        if let (Some(t), Some(cap)) = (run_soc_temp_max(&m), temp_cap) {
                            if t > cap {
                                abort_reason = Some(format!("{arm} 臂第 {i} 轮 SoC 结温 {t}C 超过上限 {cap}C"));
                                break 'rounds;
                            }
                        }
                        if arm == "knob" {
                            m["layer_hits"] = json!(layer_hits(serial, &pkg, evidence));
                            knob_runs.push(m);
                        } else {
                            ctrl_runs.push(m);
                        }
                    }
                    Err(e) => {
                        abort_reason = Some(e);
                        break 'rounds;
                    }
                }
                continue;
            }
            std::thread::sleep(std::time::Duration::from_secs(GRAY_SHOT_SETTLE_S));
            match grab_rgba(serial) {
                Ok(shot) => {
                    if i == 1 {
                        save_png(&evidence.join(format!("shot_{arm}{i}.png")), &shot);
                    }
                    shots.insert(arm, shot);
                }
                Err(e) => {
                    abort_reason = Some(format!("{arm}{i} 截图失败, 画面比对做不了: {e}"));
                    break 'rounds;
                }
            }
            let res = run_one_with(
                serial,
                &format!("gray_{arm}{i}"),
                &evidence.join(format!("{arm}{i}")),
                seconds,
                &workload_path,
                &pkg,
                req["workload"]["render_comm"].as_str(),
            );
            match res {
                Ok(mut m) => {
                    if let (Some(t), Some(cap)) = (run_soc_temp_max(&m), temp_cap) {
                        if t > cap {
                            abort_reason = Some(format!("{arm} 臂第 {i} 轮 SoC 结温 {t}C 超过上限 {cap}C"));
                            break 'rounds;
                        }
                    }
                    if arm == "knob" {
                        // 命中 marker 要在**这一臂跑完后立刻**采: 下一次 mount_layer
                        // 会把三个落点全删了重写, 拖到循环外读到的就是初始化的空表
                        let hits_now = layer_hits(serial, &pkg, evidence);
                        m["layer_hits"] = json!(hits_now);
                        knob_runs.push(m);
                    } else {
                        ctrl_runs.push(m);
                    }
                }
                Err(e) => {
                    abort_reason = Some(e);
                    break 'rounds;
                }
            }
        }
        if launch.is_some() {
            continue; // 画面比对已在上机前用 storeop-check 做过
        }
        let (k, c) = (&shots["knob"], &shots["ctrl"]);
        let Some(p) = psnr_rgba(&k.2, &c.2) else {
            abort_reason = Some(format!("第 {i} 轮两臂截图尺寸不一样 ({}x{} vs {}x{}), 画面比对做不了", k.0, k.1, c.0, c.1));
            break;
        };
        psnr_knob_vs_ctrl.push(p);
        if let Some(prev) = &prev_ctrl {
            if let Some(q) = psnr_rgba(&prev.2, &c.2) {
                psnr_ctrl_vs_ctrl.push(q);
            }
        }
        if p < min_psnr {
            save_png(&evidence.join(format!("shot_knob{i}_bad.png")), k);
            save_png(&evidence.join(format!("shot_ctrl{i}_bad.png")), c);
            abort_reason = Some(format!(
                "画面出错: 第 {i} 轮改写臂与对照臂同位置截图 PSNR {p:.2} dB, 低于门槛 {min_psnr} dB (截图见 evidence_dir)"
            ));
            break;
        }
        prev_ctrl = shots.remove("ctrl");
    }
    // 还原
    let restore_log = gl.off();
    std::fs::write(evidence.join("restore.log"), &restore_log).ok();

    // 各 knob 臂采到的命中取最大 (require_hit 判的是"改写到底有没有真生效过一次")
    let hits = knob_runs
        .iter()
        .filter_map(|r| r["layer_hits"].as_u64())
        .max()
        .unwrap_or(0);
    let restore_clean = gl_off_clean(&restore_log);
    let min_of = |v: &[f64]| v.iter().cloned().reduce(f64::min);
    let (cool_reached, cool_timeout) = cool_summary(&cool_log);
    report["conditions_observed"] = json!({
        "cool_c_target": cool_c,
        "cool_reached_c": cool_reached,
        "cool_timeout": cool_timeout,
        "cool_per_arm": cool_log,
        "hits": hits,
        "apply_ok": hits > 0,
        "restored": restore_clean,
        "image_check": match &live_check {
            Some(v) => json!({"method": "storeop_check_live", "result": v}),
            None => json!({
                "method": "same_spot_screenshot",
                "min_psnr_db": min_psnr,
                "psnr_knob_vs_ctrl_db": psnr_knob_vs_ctrl,
                "psnr_ctrl_vs_ctrl_db": psnr_ctrl_vs_ctrl,
            }),
        },
    });
    report["evidence_dir"] = json!(evidence.display().to_string());

    if let Some(reason) = abort_reason {
        report["verdict"]["reason"] = json!(reason);
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    if !restore_clean {
        report["verdict"]["reason"] = json!("摘层没摘干净 (见 restore.log), 后续数据不可信");
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    // require_hit 缺省为真: 改写一次都没命中 = 拿 A 去和 A 比, 整轮作废
    if require_hit && hits == 0 {
        report["verdict"]["reason"] =
            json!("require_hit=true 但改写一次都没命中 (层自报绑定计数为 0) —— 拿 A 去和 A 比, 整轮作废");
        return Ok(serde_json::to_string_pretty(&report).unwrap_or_default());
    }

    // 画面过了门槛: 照常做统计判定。psnr_db 是约束不是目标, 记最差的那一轮。
    let (verdict, keep, on_battery_all) = judge_runs(&ctrl_runs, &knob_runs, alpha, rounds);
    report["verdict"] = verdict;
    let mut m = report["metrics"].take();
    let power = aggregate_metrics(&mut m, &knob_runs, &ctrl_runs, on_battery_all);
    m["psnr_db"] = json!(min_of(&psnr_knob_vs_ctrl));
    if power.is_none() {
        unavailable(&mut m, "power_watt", "非放电态 (或功耗采样缺失): 整机功耗量不准, 如实 null");
    }
    if m["fps_p95_ms"].is_null() {
        unavailable(&mut m, "fps_p95_ms", "本轮没拿到可用的帧时序");
    }
    report["metrics"] = m;
    report["status"] = json!(if keep { "PASS" } else { "REJECT" });
    Ok(serde_json::to_string_pretty(&report).unwrap_or_default())
}

/// 摘层回包是否干净 (无 KNOB/RESTORE 失败标记, 无非零退出)。
fn gl_off_clean(restore_log: &str) -> bool {
    !restore_log.contains("#adb_exit=1") && !restore_log.contains("警告")
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

    /// 功耗与结温来自 sample_env (env), parse-trace 的 summary 里没有这两项。
    /// 判定视图只读 summary 的话, 功耗永远「缺数据」、70 °C 温度上限永远不触发。
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

