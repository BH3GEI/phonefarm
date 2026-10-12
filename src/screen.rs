//! 设备屏幕观战窗口 —— 零 Token，只读。
//!
//! 移植自 iPhone-use 的实时屏幕 widget
//! (<https://github.com/zhongerxin/iPhone-use>, MIT, © 2026 ZHONG XIN, commit 08ed5b0):
//! 它的 `ui/`（手机外壳、设备状态、操作光效、点击/拖动提示、刷新/主屏/截图按钮、
//! 暂停恢复、锁屏态区分）与 `server/wda_screen.py`（屏幕流不落盘、断开与暂停要分开）
//! 定义了这套东西该长什么样。**只借语义, 不搬代码** —— 那边是 TypeScript + Node,
//! 这边是一段自包含 HTML 加一个 std-only 的 HTTP 服务。登记见 THIRD_PARTY_NOTICES.md。
//!
//! # 它为什么存在
//!
//! 自动化跑起来之后看不见设备是最难受的一件事: 局在跑, 你只知道它在跑。
//! 这个窗口把「设备现在长什么样」和「harness 刚才点了哪」放在同一张图上。
//!
//! # 三条纪律
//!
//! 1. **只读**。它不改设备状态, 也不拿设备锁 —— 拿锁会让它没法在别人跑局时观战,
//!    而观战正是它的全部用途。
//! 2. **但它不是免费的**。每秒一次 `screencap` 会动 GPU 与 CPU。**做判定性测量时不要开着它**,
//!    这一条写在 `--help`、HTML 页面上和这里, 三处都写。
//! 3. **不重放**。流断了就如实显示「断开」并继续重试取帧, 不去补历史帧。
//!    看到的就是当下 —— 与 `collect` 的不重放同源。

use crate::device::Device;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 自包含的观战页面(单文件, 无外链、无构建步骤)。
const WIDGET_HTML: &str = include_str!("screen_widget.html");
/// 取帧间隔下限。再密就是拿设备当靶子打, 观战不该有那个代价。
const MIN_INTERVAL_MS: u64 = 200;
const DEFAULT_INTERVAL_MS: u64 = 700;
const DEFAULT_PORT: u16 = 8899;
/// 环形保留的动作条数(页面只画最近十几条, 留点余量给 since= 查询)。
const EVENT_KEEP: usize = 64;

// ══════════════ 共享状态 ══════════════

struct Shared {
    jpeg: Mutex<Vec<u8>>,
    frame_seq: AtomicU64,
    last_ok_ms: AtomicU64,
    paused: AtomicBool,
    /// 设备像素尺寸(动作坐标按它归一化)
    dev_w: AtomicU64,
    dev_h: AtomicU64,
    serial: Mutex<String>,
    error: Mutex<Option<String>>,
    interval_ms: u64,
    /// 动作喂给谁: None = 没接(m页面不画涟漪)
    feed: Option<Arc<Feed>>,
}

struct Feed {
    events: Mutex<Vec<Value>>,
    seq: AtomicU64,
    dev_w: AtomicU64,
    dev_h: AtomicU64,
}

impl Feed {
    fn push(&self, rec: &Value) {
        let dev_w = self.dev_w.load(Ordering::Relaxed).max(1) as f64;
        let dev_h = self.dev_h.load(Ordering::Relaxed).max(1) as f64;
        let num = |k: &str| rec.get(k).and_then(|v| v.as_f64());
        let a = rec.get("a").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if a.is_empty() {
            return;
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        // 坐标一律归一化到 0~1 —— 换算在服务端做, 页面不猜缩放。
        let mut ev = json!({"seq": seq, "a": a});
        if let Some(x) = num("x") {
            ev["x"] = json!((x / dev_w).clamp(0.0, 1.0));
        }
        if let Some(y) = num("y") {
            ev["y"] = json!((y / dev_h).clamp(0.0, 1.0));
        }
        if let Some(x2) = num("x2") {
            ev["x2"] = json!((x2 / dev_w).clamp(0.0, 1.0));
        }
        if let Some(y2) = num("y2") {
            ev["y2"] = json!((y2 / dev_h).clamp(0.0, 1.0));
        }
        for k in ["text", "what"] {
            if let Some(s) = rec.get(k).and_then(|v| v.as_str()) {
                ev[k] = json!(s);
            }
        }
        let mut e = self.events.lock().unwrap();
        e.push(ev);
        if e.len() > EVENT_KEEP {
            let drop = e.len() - EVENT_KEEP;
            e.drain(..drop);
        }
    }
    fn since(&self, after: u64) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["seq"].as_u64().unwrap_or(0) > after)
            .cloned()
            .collect()
    }
}

// ══════════════ 子命令 ══════════════

fn usage() -> &'static str {
    "用法: phonefarm screen [--serial <设备>] [--port N] [--interval-ms N]\n\
     \x20                   [--actions <log.jsonl>] [--from-start] [--out <目录>]\n\
     \x20 在浏览器里看设备屏幕 + harness 刚才点了哪(零 Token, 只读)。\n\
     \x20 --interval-ms: 取帧间隔, 缺省 700, 下限 200\n\
     \x20 --actions    : 尾随一份 log.jsonl, 把 {\"r\":\"act\"} 记录画成点击/拖动提示\n\
     \x20 --from-start : 从头读动作(默认为实时, 只读新产生的)\n\
     \x20 --out        : 「存图」按钮把截图写到哪(缺省 <tmp>/screen-shots)\n\
     \x20 ⚠ 不做判定性测量时再开它: 每秒一次 screencap 会影响帧时与功耗。\n\
     \x20 ⚠ 它不拿设备锁 —— 拿锁就没法在别人跑局时观战, 而观战正是它的用途。"
}

pub fn run_screen(args: &[String]) -> i32 {
    let mut serial: Option<String> = None;
    let mut port = DEFAULT_PORT;
    let mut interval_ms = DEFAULT_INTERVAL_MS;
    let mut actions: Option<String> = None;
    let mut from_start = false;
    let mut out_dir: Option<String> = None;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        let need = |it: &mut std::slice::Iter<String>, flag: &str| -> Option<String> {
            it.next().cloned().or_else(|| {
                eprintln!("{flag} 需要一个值");
                None
            })
        };
        match a.as_str() {
            "--serial" => serial = need(&mut it, "--serial"),
            "--port" => {
                port = need(&mut it, "--port").and_then(|v| v.parse().ok()).unwrap_or(0);
                if port == 0 {
                    eprintln!("--port 需要一个端口号\n{}", usage());
                    return 2;
                }
            }
            "--interval-ms" => {
                interval_ms = need(&mut it, "--interval-ms")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(MIN_INTERVAL_MS)
                    .max(MIN_INTERVAL_MS);
            }
            "--actions" => actions = need(&mut it, "--actions"),
            "--from-start" => from_start = true,
            "--out" => out_dir = need(&mut it, "--out"),
            "-h" | "--help" => {
                println!("{}", usage());
                return 2;
            }
            other => {
                eprintln!("未知参数 {other}\n{}", usage());
                return 2;
            }
        }
    }

    let tmp = std::env::var("PHONEFARM_TMP").unwrap_or_else(|_| "/tmp/phonefarm".to_string());
    let _ = std::fs::create_dir_all(&tmp);
    let shots = out_dir.unwrap_or_else(|| format!("{tmp}/screen-shots"));
    let _ = std::fs::create_dir_all(&shots);

    let phone = Arc::new(Device::new(serial.clone(), tmp.clone()));
    let (dw, dh) = phone.size();
    let feed = actions.as_ref().map(|_| {
        Arc::new(Feed {
            events: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
            dev_w: AtomicU64::new(dw.max(1) as u64),
            dev_h: AtomicU64::new(dh.max(1) as u64),
        })
    });

    let sh = Arc::new(Shared {
        jpeg: Mutex::new(Vec::new()),
        frame_seq: AtomicU64::new(0),
        last_ok_ms: AtomicU64::new(0),
        paused: AtomicBool::new(false),
        dev_w: AtomicU64::new(dw.max(1) as u64),
        dev_h: AtomicU64::new(dh.max(1) as u64),
        serial: Mutex::new(serial.clone().unwrap_or_else(|| "自动".into())),
        error: Mutex::new(None),
        interval_ms,
        feed: feed.clone(),
    });

    // ── 采集线程: 唯一碰设备的线程 ──
    {
        let sh = Arc::clone(&sh);
        let phone = Arc::clone(&phone);
        let tmp = tmp.clone();
        std::thread::spawn(move || {
            let path = format!("{tmp}/screen-widget.jpg");
            loop {
                if sh.paused.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(150));
                    continue;
                }
                match phone.screen(&path) {
                    Some(_) => match std::fs::read(&path) {
                        Ok(bytes) if !bytes.is_empty() => {
                            *sh.jpeg.lock().unwrap() = bytes;
                            sh.frame_seq.fetch_add(1, Ordering::Relaxed);
                            sh.last_ok_ms.store(now_ms(), Ordering::Relaxed);
                            *sh.error.lock().unwrap() = None;
                        }
                        _ => {}
                    },
                    None => {
                        *sh.error.lock().unwrap() =
                            Some("取帧失败: 设备可能离线、被别的进程占着, 或 adb 不在 PATH".into());
                    }
                }
                std::thread::sleep(Duration::from_millis(sh.interval_ms));
            }
        });
    }

    // ── 动作线程: 尾随 log.jsonl, 把动作喂给页面 ──
    if let (Some(path), Some(feed)) = (actions.clone(), feed.clone()) {
        std::thread::spawn(move || {
            let mut pos = 0u64;
            if !from_start {
                pos = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            }
            loop {
                if let Ok(mut f) = std::fs::File::open(&path) {
                    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                    if len < pos {
                        // 文件被换过(新的一局), 从头读 —— 不追旧文件的位置
                        pos = 0;
                    }
                    if len > pos {
                        use std::io::Seek;
                        if f.seek(std::io::SeekFrom::Start(pos)).is_ok() {
                            let mut br = BufReader::new(&mut f);
                            let mut line = String::new();
                            while br.read_line(&mut line).unwrap_or(0) > 0 {
                                if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                                    if v.get("r").and_then(|r| r.as_str()) == Some("act") {
                                        feed.push(&v);
                                    }
                                }
                                line.clear();
                            }
                            pos = br.stream_position().unwrap_or(len);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
    }

    // ── HTTP: std-only, 够用就好 ──
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("端口 {port} 起不来: {e}");
            return 1;
        }
    };
    println!("设备屏幕观战窗口: http://127.0.0.1:{port}/");
    println!("  设备 {dw}×{dh}, 取帧间隔 {interval_ms}ms{}", match &actions {
        Some(p) => format!(", 动作源 {p}{}", if from_start { " (从头)" } else { " (实时)" }),
        None => ", 未接动作源(不画点击提示)".into(),
    });
    println!("  ⚠ 只读, 不拿设备锁; 但每秒一次 screencap 会影响帧时与功耗 —— 做判定性测量时别开着它。");
    println!("  Ctrl-C 退出。");

    for conn in listener.incoming() {
        match conn {
            Ok(c) => {
                let sh = Arc::clone(&sh);
                let shots = shots.clone();
                std::thread::spawn(move || {
                    let _ = handle(c, &sh, &shots);
                });
            }
            Err(_) => continue,
        }
    }
    0
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ══════════════ HTTP ══════════════

fn handle(mut c: TcpStream, sh: &Shared, shots: &str) -> std::io::Result<()> {
    c.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(c.try_clone()?);
    let mut req_line = String::new();
    if reader.read_line(&mut req_line)? == 0 {
        return Ok(());
    }
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    // 头读完(不解析, 但必须消费掉, 否则 keep-alive 客户端会串包)
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };

    match (method.as_str(), path.as_str()) {
        ("GET", "/") => {
            let cfg = json!({
                "interval_ms": sh.interval_ms,
                "actions_wired": sh.feed.is_some(),
            });
            // 把配置注入页面(在 <head> 后面插一段, 不改页面的其余部分)
            let html = WIDGET_HTML.replace(
                "<head>",
                &format!("<head>\n<script>window.__PF__={cfg};</script>"),
            );
            respond(&mut c, 200, "text/html; charset=utf-8", html.as_bytes())
        }
        ("GET", "/frame.jpg") => {
            let jpeg = sh.jpeg.lock().unwrap().clone();
            if jpeg.is_empty() {
                respond(&mut c, 503, "text/plain; charset=utf-8", b"no frame yet")
            } else {
                respond(&mut c, 200, "image/jpeg", &jpeg)
            }
        }
        ("GET", "/state.json") => {
            let body = json!({
                "serial": sh.serial.lock().unwrap().clone(),
                "width": sh.dev_w.load(Ordering::Relaxed),
                "height": sh.dev_h.load(Ordering::Relaxed),
                "frames": sh.frame_seq.load(Ordering::Relaxed),
                "paused": sh.paused.load(Ordering::Relaxed),
                "interval_ms": sh.interval_ms,
                "last_frame_ms": sh.last_ok_ms.load(Ordering::Relaxed),
                "error": sh.error.lock().unwrap().clone(),
            });
            respond(&mut c, 200, "application/json", body.to_string().as_bytes())
        }
        ("GET", "/events.json") => {
            let after = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("since="))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let events = sh.feed.as_ref().map(|f| f.since(after)).unwrap_or_default();
            respond(
                &mut c,
                200,
                "application/json",
                json!({"events": events}).to_string().as_bytes(),
            )
        }
        ("POST", "/screenshot") => {
            let jpeg = sh.jpeg.lock().unwrap().clone();
            if jpeg.is_empty() {
                return respond(
                    &mut c,
                    200,
                    "application/json",
                    json!({"ok": false, "reason": "还没有帧"}).to_string().as_bytes(),
                );
            }
            let name = format!("shot-{}.jpg", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
            let p = std::path::Path::new(shots).join(&name);
            let ok = std::fs::write(&p, &jpeg).is_ok();
            respond(
                &mut c,
                200,
                "application/json",
                json!({"ok": ok, "path": if ok { p.display().to_string() } else { String::new() }})
                    .to_string()
                    .as_bytes(),
            )
        }
        _ => respond(&mut c, 404, "text/plain; charset=utf-8", b"not found"),
    }
}

fn respond(c: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    c.write_all(head.as_bytes())?;
    c.write_all(body)?;
    c.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_widget_page_is_self_contained() {
        // 不能有外链: 观战窗口在离线设备网里也要能开
        assert!(WIDGET_HTML.contains("<html"), "得是一份完整 HTML");
        assert!(!WIDGET_HTML.contains("http://"), "不得引用外部资源");
        assert!(!WIDGET_HTML.contains("https://"), "不得引用外部资源");
        assert!(!WIDGET_HTML.contains("<script src"), "不得外链脚本");
        assert!(!WIDGET_HTML.contains("<link rel=\"stylesheet\""), "不得外链样式");
        // 配置注入点必须存在, 否则服务端注不进去
        assert!(WIDGET_HTML.contains("<head>"), "注入点 <head> 必须存在");
        // 三条纪律要写在页面上, 不能只写在 Rust 注释里
        assert!(WIDGET_HTML.contains("screencap"), "页面要提醒它的代价");
        assert!(WIDGET_HTML.contains("只读"), "页面要写明只读");
    }

    fn feed() -> Feed {
        Feed {
            events: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
            dev_w: AtomicU64::new(1080),
            dev_h: AtomicU64::new(2340),
        }
    }

    #[test]
    fn action_coordinates_are_normalized_server_side() {
        let f = feed();
        f.push(&json!({"r":"act","a":"tap","x":540,"y":1170}));
        f.push(&json!({"r":"act","a":"swipe","x":0,"y":2340,"x2":1080,"y2":0}));
        let ev = f.since(0);
        assert_eq!(ev.len(), 2);
        // 页面不猜缩放: 服务端已经换成 0~1
        assert_eq!(ev[0]["x"], 0.5);
        assert_eq!(ev[0]["y"], 0.5);
        assert_eq!(ev[1]["x"], 0.0);
        assert_eq!(ev[1]["y"], 1.0);
        assert_eq!(ev[1]["x2"], 1.0);
        assert_eq!(ev[1]["y2"], 0.0);
        assert_eq!(ev[1]["a"], "swipe");
    }

    #[test]
    fn out_of_range_coordinates_are_clamped_not_wrapped() {
        let f = feed();
        // 越界坐标(设备换了分辨率 / 坐标来自另一台机)不能画到框外去
        f.push(&json!({"r":"act","a":"tap","x":99999,"y":-50}));
        let ev = f.since(0);
        assert_eq!(ev[0]["x"], 1.0);
        assert_eq!(ev[0]["y"], 0.0);
    }

    #[test]
    fn only_action_records_become_events_and_seq_is_monotonic() {
        let f = feed();
        f.push(&json!({"r":"hook","kind":"budget"}));      // 不是动作
        f.push(&json!({"r":"act"}));                       // 没有 a: 跳过
        f.push(&json!({"r":"act","a":"back"}));            // 有 a 没坐标: 也算一条
        f.push(&json!({"r":"act","a":"type","text":"你好"}));
        let ev = f.since(0);
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert_eq!(ev[0]["a"], "back");
        assert_eq!(ev[1]["text"], "你好");
        // since= 是「严格大于」, 所以拿最后一条的 seq 再问只该拿到空
        let last = ev[1]["seq"].as_u64().unwrap();
        assert!(f.since(last).is_empty());
    }

    #[test]
    fn the_ring_keeps_a_bounded_number_of_events() {
        let f = feed();
        for i in 0..(EVENT_KEEP + 20) {
            f.push(&json!({"r":"act","a":"tap","x":i,"y":i}));
        }
        let ev = f.since(0);
        assert_eq!(ev.len(), EVENT_KEEP, "环形缓冲必须有上限, 否则长时间观战会吃内存");
        // 留下的是最新的那批
        assert!(ev[0]["seq"].as_u64().unwrap() > 1);
    }
}
