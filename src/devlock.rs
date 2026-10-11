//! 设备锁接进代码: 碰设备的子命令自己拿锁、心跳、释放。
//!
//! 锁本体是农场上大家共用的文件锁 `tools/devlock` (原子 mkdir + 心跳 + 排队)。这里
//! **不重新实现锁协议**, 只调那个脚本 —— 两边的语义由构造保证一致, 没有第二份实现
//! 可以和它跑偏。
//!
//! 流程 (`devlock run` 的进程内版本):
//!
//! 1. 先 `devlock renew <label>`: 成功 = 这把锁已经在 `<label>` 手里 (外层的
//!    game_opt_loop optimize、外层的 `devlock run <label>`, 或调起我们的父 phonefarm)。
//!    这时**原地执行**, 心跳归外层管, 收尾也不释放 —— 否则就是自己等自己。
//! 2. 否则 `devlock acquire <label>`, 没轮到就每 45 秒重试 (重试不丢队位), 等到
//!    `--lock-wait` 用完还没拿到就退出 2, **一个字节都不碰设备**。
//! 3. 拿到之后把同一条命令作为子进程再跑一遍 (环境里带 `DEVLOCK_LABEL=<label>`, 子进程
//!    走第 1 步原地执行), 父进程每 60 秒续一次心跳, 子进程退出就释放, 退出码原样透传。
//!    子进程里任何一处 `process::exit` 或 panic 都不会漏掉释放 —— 释放在父进程里。
//!
//! label 的来源: `--lock-label` > 环境 `DEVLOCK_LABEL` > `phonefarm`。lead 预排了队列时
//! 必须用队列里那个名字, 自己起一个新名字会排到队尾。
//!
//! 不拿锁的子命令: `fleet` (只读快照, 本身就按锁自动降级)、`devices` (只问 adb)、
//! `keepalive` (常驻保活, 拿锁就把别人全挡住)、`serve`, 以及所有纯离线的解析/报告类。

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 没给 label 时用的名字。
pub const DEFAULT_LABEL: &str = "phonefarm";
/// 缺省最多排多久。共享手机上单个持锁者合法地跑 45 分钟以上, 等两轮是常态。
pub const DEFAULT_WAIT: Duration = Duration::from_secs(120 * 60);
const POLL: Duration = Duration::from_secs(45);
const HEARTBEAT: Duration = Duration::from_secs(60);

/// 会碰设备、要拿锁的子命令。
const LOCKED: [&str; 21] = [
    "run",
    "onboard",
    "benchmark",
    "parallel",
    "script",
    "bench",
    "gray-ab",
    "gray-layer",
    "autoloop",
    "eval",
    "gpu-op",
    "perf",
    "capture",
    "test-batch",
    "cts-fetch",
    "quest",
    "probe",
    "exec",
    "experiment",
    "selftest",
    "frame-audit",
];

/// 从参数里剥下来的锁选项。
#[derive(Debug, Clone, PartialEq)]
pub struct LockOpts {
    pub label: Option<String>,
    pub wait: Duration,
    pub disabled: bool,
}

/// 这一条命令要不要拿锁。
///
/// 只问用法 (`--help`) 的、以及评测入口拿着一个**不存在**的请求路径来的 (上层探活
/// `eval --request /nonexistent/...` 就是这么问"认不认这个参数"的, 它在读文件那一步
/// 就退出, 碰不到设备) 都不拿 —— 否则别人持锁时连探活都要排队。
pub fn needs_lock(sub: &str, rest: &[String]) -> bool {
    if !LOCKED.contains(&sub) {
        return false;
    }
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        return false;
    }
    // selftest report 只读本地证据目录、不带子命令只打用法, 都不碰设备
    if sub == "selftest" && matches!(rest.first().map(|s| s.as_str()), None | Some("report")) {
        return false;
    }
    // frame-audit 只有 capture 碰设备, analyze 纯离线
    if sub == "frame-audit" && rest.first().map(|s| s.as_str()) != Some("capture") {
        return false;
    }
    if matches!(sub, "eval" | "gpu-op") {
        // 这两个入口只有两种情况会碰设备: `--unlock` 回滚遗留锁频态, 或 `--request`
        // 指着一个真实存在的文件。其余 (裸调用、缺参数、请求路径不存在) 都在解析
        // 那一步就退出 —— 上层探活 (`phonefarm gpu-op` 裸调看认不认子命令) 不能因为
        // 别人持锁就排两小时队。
        if rest.iter().any(|a| a == "--unlock") {
            return true;
        }
        return match rest.iter().position(|a| a == "--request") {
            Some(i) => rest
                .get(i + 1)
                .is_some_and(|p| std::path::Path::new(p).exists()),
            None => false,
        };
    }
    true
}

/// 解析 `45m` / `2h` / `900s` / 裸数字 (按分钟)。
pub fn parse_wait(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        _ => (s, 60),
    };
    num.trim()
        .parse::<u64>()
        .map(|n| Duration::from_secs(n * mult))
        .map_err(|_| format!("--lock-wait 看不懂 {s:?} (要 45m / 2h / 900s / 裸数字按分钟)"))
}

/// 把 `--lock-label X` / `--lock-wait D` / `--no-devlock` 从子命令参数里剥掉
/// (子命令各自的解析器不认识它们)。`args[0]` 是子命令名, 不动。
pub fn strip_lock_args(args: &mut Vec<String>) -> Result<LockOpts, String> {
    let mut o = LockOpts {
        label: None,
        wait: DEFAULT_WAIT,
        disabled: false,
    };
    let mut kept = Vec::with_capacity(args.len());
    let mut it = std::mem::take(args).into_iter();
    if let Some(sub) = it.next() {
        kept.push(sub);
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--lock-label" => {
                o.label = Some(it.next().ok_or("--lock-label 要一个名字")?);
            }
            "--lock-wait" => {
                o.wait = parse_wait(&it.next().ok_or("--lock-wait 要一个时长")?)?;
            }
            "--no-devlock" => o.disabled = true,
            _ => kept.push(a),
        }
    }
    *args = kept;
    Ok(o)
}

/// 锁脚本的位置: `DEVLOCK_BIN` > 仓库里的 `tools/devlock`。
pub fn script_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DEVLOCK_BIN") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let p = crate::eval::repo_root().join("tools/devlock");
    p.is_file().then_some(p)
}

fn call(script: &PathBuf, args: &[&str]) -> (bool, String) {
    match Command::new(script).args(args).output() {
        Ok(o) => (
            o.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
            .trim()
            .to_string(),
        ),
        Err(e) => (false, format!("调不起 {}: {e}", script.display())),
    }
}

/// 拿锁的结果。
pub enum Gate {
    /// 原地执行 (锁已在本 label 手里, 或显式 `--no-devlock`)。
    Proceed,
    /// 已经在子进程里跑完了, 带着它的退出码退出。
    Exit(i32),
}

/// 子命令入口前的那一道。`args` 是去掉程序名后的整条参数 (`args[0]` = 子命令)。
pub fn gate(args: &mut Vec<String>) -> Gate {
    let opts = match strip_lock_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return Gate::Exit(2);
        }
    };
    if opts.disabled {
        eprintln!("⚠ --no-devlock: 不拿设备锁直接碰设备。共享手机上两边的数都可能作废。");
        return Gate::Proceed;
    }
    let label = opts
        .label
        .clone()
        .or_else(|| std::env::var("DEVLOCK_LABEL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| DEFAULT_LABEL.to_string());
    let Some(script) = script_path() else {
        eprintln!(
            "找不到设备锁脚本 (DEVLOCK_BIN 未设, {} 也不在)。没拿到锁不碰设备; \
             确实没有别人共用这台手机时可加 --no-devlock",
            crate::eval::repo_root().join("tools/devlock").display()
        );
        return Gate::Exit(2);
    };
    // 1. 已经在自己这一侧手里: 原地跑, 心跳与释放归外层
    if call(&script, &["renew", &label]).0 {
        std::env::set_var("DEVLOCK_LABEL", &label);
        return Gate::Proceed;
    }
    // 2. 排队拿锁
    if let Err(e) = acquire(&script, &label, opts.wait) {
        eprintln!("{e}");
        return Gate::Exit(2);
    }
    // 3. 子进程里跑, 父进程心跳 + 释放
    let stop = Arc::new(AtomicBool::new(false));
    let hb = heartbeat(script.clone(), label.clone(), stop.clone());
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("phonefarm"));
    let code = match Command::new(&exe)
        .args(args.iter())
        .env("DEVLOCK_LABEL", &label)
        .status()
    {
        Ok(s) => s.code().unwrap_or(2),
        Err(e) => {
            eprintln!("起不了 {}: {e}", exe.display());
            2
        }
    };
    stop.store(true, Ordering::SeqCst);
    let _ = hb.join();
    let (ok, out) = call(&script, &["release", &label]);
    if !ok {
        eprintln!("设备锁释放失败 ({label}): {out}");
    }
    Gate::Exit(code)
}

/// 排队直到拿到, 或等满 `wait`。
fn acquire(script: &PathBuf, label: &str, wait: Duration) -> Result<(), String> {
    let t0 = Instant::now();
    loop {
        let (ok, out) = call(script, &["acquire", label]);
        if ok {
            eprintln!("设备锁: 已拿到 ({label})");
            return Ok(());
        }
        if t0.elapsed() + POLL > wait {
            return Err(format!(
                "设备锁: 等了 {} 秒还没轮到 {label}, 没碰设备就退出 ({out})。\
                 --lock-wait 可以放长; lead 预排了队列时 --lock-label 要用队列里的名字",
                t0.elapsed().as_secs()
            ));
        }
        eprintln!(
            "设备锁: 还没轮到 {label} ({out}); {} 秒后重试, 已等 {} 秒",
            POLL.as_secs(),
            t0.elapsed().as_secs()
        );
        std::thread::sleep(POLL);
    }
}

/// 后台心跳。续期失败 (锁被当成废弃接管了) 只喊一次 —— 子进程已经在跑, 杀它会把
/// 设备留在半施加态, 比让它收尾更糟。
fn heartbeat(script: PathBuf, label: String, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut warned = false;
        loop {
            let t = Instant::now();
            while t.elapsed() < HEARTBEAT {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            let (ok, out) = call(&script, &["renew", &label]);
            if !ok && !warned {
                eprintln!("⚠ 设备锁续期失败 ({label}): {out} —— 锁可能已被接管, 这一轮数据要存疑");
                warned = true;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lock_flags_are_stripped_before_the_subcommand_sees_them() {
        let mut a = v(&["eval", "--request", "r.json", "--lock-label", "core", "--json", "--lock-wait", "10m"]);
        let o = strip_lock_args(&mut a).unwrap();
        assert_eq!(a, v(&["eval", "--request", "r.json", "--json"]));
        assert_eq!(o.label.as_deref(), Some("core"));
        assert_eq!(o.wait, Duration::from_secs(600));
        assert!(!o.disabled);
    }

    #[test]
    fn a_dangling_lock_flag_is_an_error() {
        assert!(strip_lock_args(&mut v(&["eval", "--lock-label"])).is_err());
        assert!(strip_lock_args(&mut v(&["eval", "--lock-wait", "soon"])).is_err());
    }

    #[test]
    fn wait_durations_parse_like_the_budget() {
        assert_eq!(parse_wait("45m").unwrap(), Duration::from_secs(2700));
        assert_eq!(parse_wait("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_wait("900s").unwrap(), Duration::from_secs(900));
        assert_eq!(parse_wait("5").unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn only_device_touching_subcommands_take_the_lock() {
        assert!(needs_lock("gray-layer", &v(&["off"])));
        assert!(needs_lock("perf", &v(&["--serial", "S"])));
        assert!(needs_lock("selftest", &v(&["run", "--out", "d"])));
        assert!(!needs_lock("selftest", &v(&["report", "--root", "d"])));
        assert!(!needs_lock("selftest", &v(&[])));
        for s in ["fleet", "devices", "keepalive", "serve", "parse-trace", "analyze", "report"] {
            assert!(!needs_lock(s, &[]), "{s}");
        }
        assert!(!needs_lock("gpu-op", &v(&["--help"])));
    }

    /// game_opt_loop 的 harness 探活是**裸调** `phonefarm gpu-op` (看认不认这个子命令)。
    /// 它在解析参数那一步就退出, 碰不到设备; 曾经因为要锁而在别人持锁时排队两小时,
    /// 把 game_opt_loop 的 cargo test 整个挂住。
    #[test]
    fn a_bare_or_requestless_eval_call_does_not_queue() {
        assert!(!needs_lock("gpu-op", &[]));
        assert!(!needs_lock("eval", &[]));
        assert!(!needs_lock("gpu-op", &v(&["--serial", "S", "--json"])));
        assert!(!needs_lock("eval", &v(&["--request"])));
    }

    /// 上层探活拿一个不存在的请求路径问 eval 认不认 `--request`: 它碰不到设备,
    /// 不该在别人持锁时被挡在队里。真请求 (文件在) 照样要锁。
    #[test]
    fn an_eval_probe_with_a_missing_request_does_not_queue() {
        assert!(!needs_lock("eval", &v(&["--request", "/nonexistent/x.json", "--json"])));
        let f = std::env::temp_dir().join(format!("pf_devlock_req_{}.json", std::process::id()));
        std::fs::write(&f, "{}").unwrap();
        assert!(needs_lock("eval", &v(&["--request", &f.to_string_lossy()])));
        std::fs::remove_file(&f).ok();
        // --unlock 回滚锁频态是写设备
        assert!(needs_lock("gpu-op", &v(&["--serial", "S", "--unlock"])));
    }
}
