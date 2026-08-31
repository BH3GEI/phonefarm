//! 六步循环 + 记录流(契约v1) + 确定性检查 + 时间点表。
//! v0.3: 多步计划(一次调用1~plan_max个动作,逐步验证,遇挫弃约) + 双采集并行。
//! v0.4: 定格从"固定等待"改为"等画面安静"(连续两张小图一致即稳,上限settle_ms);
//!       不安静的屏记为动态(noise=背景动静幅度),像素差异需压过背景才算有效,
//!       边界差异(刚过背景一点)经仲裁裁定,每局限5次。
//! 程序只写 tasks/<任务>/ 之下: runs/<局>/log.jsonl、runs/<局>/step*.jpg、lessons.jsonl、params.toml。
use crate::brain::Brain;
use crate::device::{frames_diff_pct, mode_gray, patch_stats, thumb_gray, Adb, Node};
use crate::tree::Tree;
use crate::Config;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs;
use std::io::Write;

const NORM: i64 = 999;
/// 小图差异≤该值(%)视为"没动": 静态屏的采样抖动(状态栏时钟等)在此之下
const QUIET_PCT: f32 = 0.6;

struct Cap {
    seq: u32,
    els: Vec<Node>, // 设备像素空间
    img: String,    // jpg 绝对路径
    img_rel: String,
    w: i32,
    h: i32,
    thumb: Option<Vec<u8>>,
    noise: f32,       // 采集时量到的背景动静幅度(%);0=画面安静
    pkg: String,      // 前台应用包名(采不到为空)
    els_pkg: String,  // 元素树的多数包名
    suspect: bool,    // 假树: 树包名≠前台包名且重抓无效 → 本步文字通道不可信
    ocr: bool,        // 本帧清单来自截图文字识别(UI树为空时的备胎)
}

#[derive(Clone, Default)]
struct ActN {
    a: String,
    x: Option<i64>, // 0~999,过门后转图片像素
    y: Option<i64>,
    x2: Option<i64>,
    y2: Option<i64>,
    text: Option<String>,
    what: Option<String>, // tap 的点名: 意图点中的元素文字,门按清单吸附到其准确中心
}

struct Ban {
    x: i32, // 图片像素空间
    y: i32,
    rad: i32,
    why: String,
}

struct Log {
    f: Option<fs::File>,
}
impl Log {
    fn put(&mut self, v: Value) {
        if let Some(f) = self.f.as_mut() {
            let _ = writeln!(f, "{}", v);
        }
    }
}

pub fn tcut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 从模型原文里扫出所有平衡的 JSON 对象
fn extract_objs(text: &str) -> Vec<Value> {
    let bytes: Vec<char> = text.chars().collect();
    let mut objs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '{' {
            let mut depth = 0i32;
            let mut in_str = false;
            let mut esc = false;
            let mut j = i;
            while j < bytes.len() {
                let c = bytes[j];
                if esc { esc = false; }
                else if in_str {
                    if c == '\\' { esc = true; }
                    else if c == '"' { in_str = false; }
                } else if c == '"' { in_str = true; }
                else if c == '{' { depth += 1; }
                else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        let s: String = bytes[i..=j].iter().collect();
                        if let Ok(v) = serde_json::from_str::<Value>(&s) {
                            objs.push(v);
                        }
                        i = j;
                        break;
                    }
                }
                j += 1;
            }
        }
        i += 1;
    }
    objs
}

fn num(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64))
}

fn now_tag() -> String {
    let out = std::process::Command::new("date").arg("+%Y%m%d-%H%M%S").output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "run".into())
}

/// 后台包是否可安全掐死(force-stop): 目标应用、系统/谷歌家族包、桌面一律不碰
fn pkg_killable(pkg: &str, target: Option<&str>) -> bool {
    !pkg.is_empty()
        && target.map_or(true, |t| pkg != t)
        && pkg != "android"
        && !pkg.starts_with("com.android.")
        && !pkg.starts_with("com.google.")
        && !pkg.contains("launcher")
}

/// 双采集(截图与元素列表并行) + 等画面安静:
/// 连拍小图,连续两张一致(≤QUIET_PCT)即认为停稳(noise=0);
/// 到 cap_ms 上限仍不一致 → 动态屏,期间小图两两差异的最大值记为背景动静幅度(noise>0)。
fn capture(phone: &Adb, run_dir: &str, seq: u32, cfg: &Config, tmp: &str, cap_ms: u64,
           realw: i32, realh: i32, target: Option<&str>) -> Option<Cap> {
    let img_rel = format!("step{seq}.jpg");
    let img = format!("{run_dir}/{img_rel}");
    let (wh, elsres, noise) = std::thread::scope(|s| {
        let h_els = s.spawn(|| phone.els(cfg.els_timeout_ms));
        let mut prev = phone.quick_thumb("a");
        let mut noise = 0.0f32;
        let t0 = std::time::Instant::now();
        while t0.elapsed().as_millis() < cap_ms as u128 {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let cur = phone.quick_thumb("b");
            if let (Some(a), Some(b)) = (&prev, &cur) {
                let pct = frames_diff_pct(a, b);
                if pct <= QUIET_PCT {
                    noise = 0.0; // 停稳:此前的不一致是过渡帧,不算背景动静
                    break;
                }
                if pct > noise { noise = pct; }
            }
            prev = cur;
        }
        let wh = phone.screen(&img);
        (wh, h_els.join().unwrap_or_default(), noise)
    });
    let (w, h) = wh?;
    let (mut els, mut els_pkg) = elsres;
    let mut fg = phone.foreground_pkg();
    let mut suspect = false;
    // 假树识别: uiautomator 在应用切换瞬间会吐出上一个应用的陈旧树。
    // 画面安静(像素)≠树已刷新;树的多数包名与前台不符即判陈旧,重抓至多2次。
    if !fg.is_empty() && !els.is_empty() && !els_pkg.is_empty() && els_pkg != fg {
        // 治本: 假树来自后台应用且可杀 → force-stop 掐死事件风暴源头,账本立刻翻页
        if pkg_killable(&els_pkg, target) {
            println!("      🔪 假树源头在后台({els_pkg}),force-stop 掐死再重抓");
            phone.force_stop(&els_pkg);
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        for retry in 1..=2 {
            println!("      👻 元素树({els_pkg})≠前台({fg}),疑似陈旧树,重抓#{retry}");
            std::thread::sleep(std::time::Duration::from_millis(250));
            let (e2, p2) = phone.els(cfg.els_timeout_ms);
            if !e2.is_empty() { els = e2; els_pkg = p2; }
            let f2 = phone.foreground_pkg();
            if !f2.is_empty() { fg = f2; }
            if els.is_empty() || els_pkg.is_empty() || els_pkg == fg { break; }
        }
        suspect = !els.is_empty() && !els_pkg.is_empty() && els_pkg != fg;
        if suspect {
            println!("      👻 重抓后仍不符,本步文字通道降级(只信截图)");
        }
    }
    // 文字备胎: UI树为空(游戏/自绘界面/dump失败)→ 用本机Vision识别截图文字补清单,
    // 框换回设备像素空间,点名吸附/页面身份证/熟路小地图全部照常工作
    let mut ocr = false;
    if els.is_empty() {
        let o = ocr_els(&img, w, h, realw, realh);
        if !o.is_empty() {
            println!("      🔤 UI树为空,OCR文字备胎({}条)", o.len());
            els = o;
            ocr = true;
        }
    }
    let thumb = thumb_gray(&img, tmp, &format!("{}", seq % 2));
    Some(Cap { seq, els, img, img_rel, w, h, thumb, noise, pkg: fg, els_pkg, suspect, ocr })
}

/// OCR文字备胎: UI树为空时调本机Vision(ocr二进制,由ocr.swift编译)识别截图文字。
/// 框从图片像素换回设备像素(与el_img_space互逆),els契约不变;
/// 任何失败静默返回空——备胎坏了不能拖垮主流程。
fn ocr_els(img: &str, capw: i32, caph: i32, realw: i32, realh: i32) -> Vec<Node> {
    // "./ocr": 仓库根目录下的本地工具(不在PATH里;目录契约就是二进制+工具+tasks/同层)
    let Ok(o) = std::process::Command::new("./ocr").arg(img).output() else { return vec![] };
    if !o.status.success() { return vec![] }
    let sx = |v: i64| (v * realw as i64 / capw.max(1) as i64) as i32;
    let sy = |v: i64| (v * realh as i64 / caph.max(1) as i64) as i32;
    let mut v: Vec<Node> = String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| {
        let j: Value = serde_json::from_str(l).ok()?;
        let t = j["t"].as_str()?.trim().to_string();
        let b: Vec<i64> = j["b"].as_array()?.iter().filter_map(|x| x.as_i64()).collect();
        if t.is_empty() || b.len() != 4 { return None; }
        Some(Node { t, b: [sx(b[0]), sy(b[1]), sx(b[2]), sy(b[3])] })
    }).collect();
    v.sort_by_key(|n| ((n.b[1] + n.b[3]) / 2, (n.b[0] + n.b[2]) / 2)); // 上到下、左到右
    v.truncate(80);
    v
}

/// 元素框: 设备像素 → 图片像素
fn el_img_space(n: &Node, cap: &Cap, realw: i32, realh: i32) -> [i32; 4] {
    let sx = |v: i32| (v as i64 * cap.w as i64 / realw.max(1) as i64) as i32;
    let sy = |v: i32| (v as i64 * cap.h as i64 / realh.max(1) as i64) as i32;
    [sx(n.b[0]), sy(n.b[1]), sx(n.b[2]), sy(n.b[3])]
}

/// 图片像素 → 0~999
fn to_norm(v: i32, span: i32) -> i64 {
    (v as i64 * NORM / span.max(1) as i64).clamp(0, NORM)
}

/// 元素框(设备px) → 图片px
fn el_img_of(n: &Node, cap: &Cap, realw: i32, realh: i32) -> [i32; 4] {
    el_img_space(n, cap, realw, realh)
}

/// 元素框 → 0~999 空间
fn el_norm(n: &Node, cap: &Cap, realw: i32, realh: i32) -> [i64; 4] {
    let b = el_img_space(n, cap, realw, realh);
    [to_norm(b[0], cap.w), to_norm(b[1], cap.h), to_norm(b[2], cap.w), to_norm(b[3], cap.h)]
}

/// 按文字找元素: 精确等值优先,退而含串(≥2字)。
/// 多个命中取离 (hx,hy)@0~999 最近者 —— 模型给的坐标是"哪一个同名元素"的线索。
/// 返回 (元素, 图片px框)。
fn find_el<'a>(els: &'a [Node], w: &str, hx: i64, hy: i64, cap: &Cap, realw: i32, realh: i32)
    -> Option<(&'a Node, [i32; 4])>
{
    let wt = w.trim();
    let mut pool: Vec<&Node> = els.iter().filter(|n| n.t.trim() == wt).collect();
    if pool.is_empty() && wt.chars().count() >= 2 {
        pool = els.iter().filter(|n| n.t.contains(wt)).collect();
    }
    pool.into_iter()
        .min_by_key(|n| {
            let b = el_norm(n, cap, realw, realh);
            ((b[0] + b[2]) / 2 - hx).abs() + ((b[1] + b[3]) / 2 - hy).abs()
        })
        .map(|n| (n, el_img_of(n, cap, realw, realh)))
}

/// (x,y)@0~999 落在哪个元素框内 → 该框(图片px);取最小框(最内层元素)
fn box_at(els: &[Node], x: i64, y: i64, cap: &Cap, realw: i32, realh: i32) -> Option<[i32; 4]> {
    els.iter()
        .filter(|n| {
            let b = el_norm(n, cap, realw, realh);
            x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]
        })
        .map(|n| (el_norm(n, cap, realw, realh), el_img_of(n, cap, realw, realh)))
        .min_by_key(|(bn, _)| (bn[2] - bn[0]) * (bn[3] - bn[1]))
        .map(|(_, bi)| bi)
}

/// 附近元素候选(驳回时给模型指路): 距 (x,y)@0~999 最近的3个 "文字(x,y)"
fn nearby_els(els: &[Node], x: i64, y: i64, cap: &Cap, realw: i32, realh: i32) -> String {
    let mut v: Vec<(i64, String)> = els
        .iter()
        .map(|n| {
            let b = el_norm(n, cap, realw, realh);
            let d = ((b[0] + b[2]) / 2 - x).abs() + ((b[1] + b[3]) / 2 - y).abs();
            (d, format!("{}({},{})", tcut(n.t.trim(), 10), (b[0] + b[2]) / 2, (b[1] + b[3]) / 2))
        })
        .collect();
    v.sort_by_key(|(d, _)| *d);
    v.into_iter().take(3).map(|(_, s)| s).collect::<Vec<_>>().join("、")
}

/// 本帧画面入记录流(seq 去重)。主循环顶部、goto跳段、终局三处共用同一节奏。
fn log_screen(log: &mut Log, cap: &Cap, n: u32, realw: i32, realh: i32, logged_seq: &mut u32) {
    if cap.seq == *logged_seq { return; }
    let els_img: Vec<Value> = cap
        .els
        .iter()
        .map(|e| json!({"t": e.t, "b": el_img_space(e, cap, realw, realh)}))
        .collect();
    let mut rec = json!({"r":"screen","n":n,"els":els_img,"img":cap.img_rel,"pkg":cap.pkg});
    if cap.suspect { rec["suspect"] = json!(true); }
    if cap.ocr { rec["ocr"] = json!(true); }
    log.put(rec);
    *logged_seq = cap.seq;
}

/// 熟路边 → 可执行动作。点[文字] 在当前清单里现找按钮(位置会漂,文字不漂);
/// 找不到按钮返回 None(这条边现在走不了,交还模型)。
fn act_from_edge(e: &crate::tree::Edge, cap: &Cap, realw: i32, realh: i32) -> Option<ActN> {
    match e.label.as_str() {
        "返回" => Some(ActN { a: "back".into(), ..Default::default() }),
        "上滑" => Some(ActN { a: "scroll_up".into(), ..Default::default() }),
        "下滑" => Some(ActN { a: "scroll_down".into(), ..Default::default() }),
        "回桌面" => Some(ActN { a: "home".into(), ..Default::default() }),
        label => {
            let t = Tree::tap_text(label)?;
            let (node, bi) = find_el(&cap.els, t, 500, 500, cap, realw, realh)?;
            Some(ActN {
                a: "tap".into(),
                x: Some(((bi[0] + bi[2]) / 2) as i64),
                y: Some(((bi[1] + bi[3]) / 2) as i64),
                what: Some(node.t.trim().to_string()),
                ..Default::default()
            })
        }
    }
}

fn act_line(n: u32, a: &ActN) -> String {
    match a.a.as_str() {
        "tap" => format!(
            "act#{n} tap({},{}){}",
            a.x.unwrap_or(-1), a.y.unwrap_or(-1),
            a.what.as_deref().map(|w| format!("[{}]", tcut(w, 10))).unwrap_or_default()
        ),
        "swipe" => format!(
            "act#{n} swipe({},{}→{},{})",
            a.x.unwrap_or(-1), a.y.unwrap_or(-1), a.x2.unwrap_or(-1), a.y2.unwrap_or(-1)
        ),
        "type" => format!("act#{n} type({})", tcut(a.text.as_deref().unwrap_or(""), 20)),
        "launch" => format!("act#{n} launch({})", tcut(a.text.as_deref().unwrap_or(""), 40)),
        "goto" => format!("act#{n} goto({})", tcut(a.text.as_deref().unwrap_or(""), 16)),
        o => format!("act#{n} {o}"),
    }
}

/// ② 上下文组装: goal + 通用/任务经验 + 安装清单 + 最近act/diff窗 + ban + note + 小地图 + 前台应用 + 屏幕
/// 静态段(goal/经验/apps)在前,动态段在后,保住能保的提示词缓存前缀。
fn render_ctx(goal: &str, glessons: &[Value], lessons: &[Value], apps_line: &str,
              map_line: &str, window: &[(String, String)], bans: &[Ban],
              note: &str, cap: &Cap, realw: i32, realh: i32) -> String {
    let mut s = format!("goal: {goal}\n");
    for l in glessons {
        s.push_str(&format!("lesson(通用): {}\n", l["t"].as_str().unwrap_or("")));
    }
    for l in lessons {
        s.push_str(&format!(
            "lesson#{}(win{}/lose{}): {}\n",
            l["id"].as_i64().unwrap_or(0),
            l["win"].as_i64().unwrap_or(0),
            l["lose"].as_i64().unwrap_or(0),
            l["t"].as_str().unwrap_or("")
        ));
    }
    if !apps_line.is_empty() {
        s.push_str(&format!("apps(可launch的包名): {apps_line}\n"));
    }
    for (a, d) in window {
        s.push_str(&format!("{a} → diff: {d}\n"));
    }
    for b in bans.iter().rev().take(8) {
        s.push_str(&format!(
            "ban: tap({},{})±{} — {}\n",
            to_norm(b.x, cap.w), to_norm(b.y, cap.h),
            b.rad as i64 * NORM / cap.w.max(1) as i64, b.why
        ));
    }
    if !note.is_empty() {
        s.push_str(&format!("note: {note}\n"));
    }
    if !map_line.is_empty() {
        s.push_str(&format!(
            "{map_line} | goto直达: {{\"r\":\"act\",\"a\":\"goto\",\"text\":\"P页号\"}}\n"
        ));
    }
    if cap.noise > 0.0 {
        s.push_str(&format!(
            "(本画面持续在动:视频/动画,背景动静约{:.0}%。你的动作效果会按扣除该背景来判断,正常继续遍历即可;若确认是视频页可在note记下)\n",
            cap.noise
        ));
    }
    if !cap.pkg.is_empty() {
        let desk = if cap.pkg.contains("launcher") { " (即桌面)" } else { "" };
        s.push_str(&format!("app: {}{desk}\n", cap.pkg));
    }
    s.push_str("screen:\n");
    if cap.suspect {
        s.push_str(&format!(
            "(元素列表不可信:树来自{}而前台是{},本步只看截图判断)\n",
            cap.els_pkg, cap.pkg
        ));
    } else {
        if cap.ocr {
            s.push_str("(元素列表来自截图文字识别OCR:坐标即屏幕文字位置;无文字的图标/图片按钮仍只能按截图定位)\n");
        }
        for n in cap.els.iter().take(60) {
            let bi = el_img_space(n, cap, realw, realh);
            s.push_str(&format!(
                "{} [{},{},{},{}]\n",
                n.t,
                to_norm(bi[0], cap.w), to_norm(bi[1], cap.h),
                to_norm(bi[2], cap.w), to_norm(bi[3], cap.h)
            ));
        }
        if cap.els.is_empty() {
            s.push_str("(元素列表为空,只看截图)\n");
        }
    }
    s.push_str("(当前截图见图)");
    s
}

/// ③ 决策调用: 返回按序计划(1~plan_max个act,done截断) + note。换人重问最多3家。
fn plan_call(brain: &mut Brain, cfg: &Config, user: &str, img: &str)
    -> Result<(Vec<ActN>, Option<String>, String, u64), String>
{
    let sys = cfg.prompts.get("step").map(|s| s.as_str()).unwrap_or("");
    for attempt in 0..3 {
        let out = brain.call(sys, user, &[img], 500, attempt)?;
        let objs = extract_objs(&out.text);
        let mut acts: Vec<ActN> = Vec::new();
        // note 两种来法都收: 正常的 r=note,以及把便签误当动作发的 r=act,a=note(契约口子,不判死)
        let mut note = objs.iter().find(|o| o["r"] == "note")
            .and_then(|o| o["t"].as_str()).map(String::from);
        for o in &objs {
            if o["r"] != "act" { continue; }
            let name = o["a"].as_str().unwrap_or("").to_string();
            if name.is_empty() { continue; }
            if name == "note" {
                let t = o["t"].as_str().or_else(|| o["text"].as_str()).map(String::from);
                if t.is_some() && note.is_none() { note = t; }
                continue;
            }
            let is_done = name == "done";
            acts.push(ActN {
                a: name,
                x: num(&o["x"]),
                y: num(&o["y"]),
                x2: num(&o["x2"]),
                y2: num(&o["y2"]),
                text: o["text"].as_str().map(String::from),
                what: o["what"].as_str().map(String::from),
            });
            if is_done || acts.len() >= cfg.plan_max { break; }
        }
        if !acts.is_empty() {
            // done 只能单发: 计划里 done 前面还有动作时,截到 done 之前——
            // 前面的动作执行完画面会变,必须重新看过画面才能宣判(防 [back,done] 落在登录层)。
            if let Some(di) = acts.iter().position(|a| a.a == "done") {
                acts.truncate(if di == 0 { 1 } else { di });
            }
            // goto 只能是最后一步: 它会连跳几屏,排在后面的动作全成马后炮
            if let Some(gi) = acts.iter().position(|a| a.a == "goto") {
                acts.truncate(gi + 1);
            }
            return Ok((acts, note, out.by, out.ms));
        }
        if let Some(t) = note {
            // 只发了便签没发动作: 记下便签原地等一拍,重新决策(此前这里会"连续3家无法解析"判死)
            println!("      ({} 只给了note没给act,记便签原地等待)", out.by);
            return Ok((vec![ActN { a: "wait".into(), ..Default::default() }], Some(t), out.by, out.ms));
        }
        println!("      ({} 回复不含act记录,换下一家)", out.by);
        brain.blame(&out.by);
    }
    Err("连续3家回复都无法解析".into())
}

/// ④ 确定性门(值域/点名吸附/前科/连点/空白/launch目标)。通过返回 None, 驳回返回原因。坐标就地转成图片像素。
/// 点名制: tap 附 what=元素文字 → 吸附到该元素(清单)的准确中心;找不到则驳回并附附近候选。
/// 未点名但落点在某元素框内 → 也吸附中心(修手偏,零成本)。清单不可信(假树)时不吸附。
fn gate(a: &mut ActN, cap: &Cap, bans: &[Ban], taps: &VecDeque<(i32, i32)>, cfg: &Config, tmp: &str,
        apps: &[(String, String)], realw: i32, realh: i32) -> Option<String> {
    let known = ["tap", "swipe", "scroll_up", "scroll_down", "type", "back", "home", "launch", "goto", "wait", "done"];
    if !known.contains(&a.a.as_str()) {
        return Some(format!("未知动作:{}", tcut(&a.a, 12)));
    }
    let chk = |v: Option<i64>, name: &str| -> Result<i64, String> {
        match v {
            Some(x) if (-1..=1100).contains(&x) => Ok(x.clamp(0, NORM)),
            Some(x) => Err(format!("出界:{name}={x}")),
            None => Err(format!("缺{name}")),
        }
    };
    match a.a.as_str() {
        "tap" => {
            let (x, y) = match (chk(a.x, "x"), chk(a.y, "y")) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => return Some(e),
            };
            let mut xi = (x * cap.w as i64 / NORM) as i32;
            let mut yi = (y * cap.h as i64 / NORM) as i32;
            if cap.suspect {
                // 假树: 清单不可信,不吸附不点名,按原坐标走(空白/前科检查照常)
            } else if let Some(w) = a.what.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
                match find_el(&cap.els, w, x, y, cap, realw, realh) {
                    Some((_, bi)) => {
                        xi = (bi[0] + bi[2]) / 2;
                        yi = (bi[1] + bi[3]) / 2;
                    }
                    None => {
                        return Some(format!(
                            "点名'{}'不在screen清单;附近元素: {}",
                            tcut(w, 12),
                            nearby_els(&cap.els, x, y, cap, realw, realh)
                        ));
                    }
                }
            } else if let Some(bi) = box_at(&cap.els, x, y, cap, realw, realh) {
                xi = (bi[0] + bi[2]) / 2;
                yi = (bi[1] + bi[3]) / 2;
            }
            if let Some(b) = bans.iter().find(|b| (b.x - xi).abs() < b.rad && (b.y - yi).abs() < b.rad) {
                return Some(format!("前科:距ban({},{})不足{}px", to_norm(b.x, cap.w), to_norm(b.y, cap.h), b.rad));
            }
            // 同点连点: 前两个已执行动作都是tap且都落在同一点 → 这将是第3次,直接驳回。
            // 证据: 2048抖动、"我的"登录振荡、(70,88)直播页连点,三次同型。
            let near = |p: &(i32, i32)| (p.0 - xi).abs() < cfg.ban_radius && (p.1 - yi).abs() < cfg.ban_radius;
            let mut rev = taps.iter().rev();
            if let (Some(p1), Some(p2)) = (rev.next(), rev.next()) {
                if near(p1) && near(p2) {
                    return Some("同点连点3次:换个目标,或用back/scroll_down离开当前位置".into());
                }
            }
            if let (Some((sd, mean)), Some(bg)) =
                (patch_stats(&cap.img, xi, yi, tmp), cap.thumb.as_ref().map(|t| mode_gray(t) as f32))
            {
                if sd < 6.0 && (mean - bg).abs() < 22.0 {
                    return Some(format!(
                        "空白背景:σ{sd:.1}亮度{mean:.0}≈背景{bg:.0};清单附近元素: {}",
                        nearby_els(&cap.els, x, y, cap, realw, realh)
                    ));
                }
            }
            a.x = Some(xi as i64);
            a.y = Some(yi as i64);
        }
        "swipe" => {
            let vs = match (chk(a.x, "x"), chk(a.y, "y"), chk(a.x2, "x2"), chk(a.y2, "y2")) {
                (Ok(x), Ok(y), Ok(x2), Ok(y2)) => (x, y, x2, y2),
                (Err(e), _, _, _) | (_, Err(e), _, _) | (_, _, Err(e), _) | (_, _, _, Err(e)) => {
                    return Some(e)
                }
            };
            if (vs.0 - vs.2).abs() + (vs.1 - vs.3).abs() < 50 {
                return Some(format!("滑动距离过短:({},{})→({},{})", vs.0, vs.1, vs.2, vs.3));
            }
            a.x = Some(vs.0 * cap.w as i64 / NORM);
            a.y = Some(vs.1 * cap.h as i64 / NORM);
            a.x2 = Some(vs.2 * cap.w as i64 / NORM);
            a.y2 = Some(vs.3 * cap.h as i64 / NORM);
        }
        "type" => {
            if a.text.as_deref().unwrap_or("").is_empty() {
                return Some("type缺text".into());
            }
        }
        "goto" => {
            if a.text.as_deref().unwrap_or("").trim().is_empty() {
                return Some("goto缺text(目标页号如P5,取自map行)".into());
            }
        }
        "launch" => {
            let t = a.text.as_deref().unwrap_or("").trim().to_string();
            if t.is_empty() {
                return Some("launch缺text(目标包名,从apps清单里选)".into());
            }
            if apps.is_empty() {
                // 清单采集失败的兜底: 只要求形如包名
                if !t.contains('.') {
                    return Some(format!("launch目标'{}'不是包名", tcut(&t, 30)));
                }
            } else if let Some((p, _)) = apps.iter().find(|(p, _)| *p == t) {
                a.text = Some(p.clone());
            } else {
                // 唯一子串匹配放行(模型偶尔只写后半段)
                let subs: Vec<&(String, String)> =
                    apps.iter().filter(|(p, _)| p.contains(&t)).collect();
                if subs.len() == 1 {
                    a.text = Some(subs[0].0.clone());
                } else {
                    return Some(format!("launch目标'{}'不在apps清单", tcut(&t, 30)));
                }
            }
        }
        _ => {}
    }
    // 字段归位: 与动作无关的字段不入日志(模型偶尔多填,空间口径会混)
    match a.a.as_str() {
        "tap" => { a.x2 = None; a.y2 = None; }
        "swipe" => {}
        _ => { a.x = None; a.y = None; a.x2 = None; a.y2 = None; }
    }
    None
}

/// ⑤ 执行(图片像素 → 设备像素)
fn exec(phone: &Adb, a: &ActN, cap: &Cap, realw: i32, realh: i32, apps: &[(String, String)]) {
    let sx = |v: Option<i64>| (v.unwrap_or(0) * realw as i64 / cap.w.max(1) as i64) as i32;
    let sy = |v: Option<i64>| (v.unwrap_or(0) * realh as i64 / cap.h.max(1) as i64) as i32;
    match a.a.as_str() {
        "tap" => phone.tap(sx(a.x), sy(a.y)),
        "swipe" => phone.swipe(sx(a.x), sy(a.y), sx(a.x2), sy(a.y2)),
        "scroll_up" => phone.scroll_up(),
        "scroll_down" => phone.scroll_down(),
        "back" => phone.back(),
        "home" => phone.home(),
        "launch" => {
            let pkg = a.text.as_deref().unwrap_or("");
            let comp = apps.iter().find(|(p, _)| p == pkg)
                .map(|(_, c)| c.as_str()).unwrap_or("");
            phone.launch(pkg, comp);
        }
        "type" => phone.type_text(a.text.as_deref().unwrap_or("")),
        "wait" => std::thread::sleep(std::time::Duration::from_millis(2500)),
        _ => {}
    }
}

/// 仲裁: 前后两张截图给模型,问"有没有真实变化"。返回 (changed, desc, 由谁判)。
fn arbit_changed(brain: &mut Brain, cfg: &Config, img_a: &str, img_b: &str)
    -> Option<(bool, String, String)>
{
    let p = cfg.hook_for("pre_ban").and_then(|h| cfg.prompt_of(h))?;
    let u = "第一张为点击前截图,第二张为点击后截图。".to_string();
    for att in 0..2 {
        if let Ok(out) = brain.call(p, &u, &[img_a, img_b], 150, att) {
            let objs = extract_objs(&out.text);
            if let Some(o) = objs.iter().find(|o| o["r"] == "hook" && o["kind"] == "arbit") {
                return Some((
                    o["changed"].as_bool().unwrap_or(false),
                    o["desc"].as_str().unwrap_or("").to_string(),
                    out.by.clone(),
                ));
            }
            brain.blame(&out.by);
        }
    }
    None
}

/// ⑥ 画面差异: els 集合差 / 像素通道回退。
/// 动态屏(任一端noise>0)下像素差异需压过背景才算有效:
///   ≤bg+5% → none;bg+5%~bg+13% → 边界区(返回border=true,交仲裁);>bg+13% → 有效。
/// 返回 (差异串, 是否边界区)。
fn diff_of(before: &Cap, after: &Cap, channel: &str) -> (String, bool) {
    // 假树(suspect)的元素集不可用于集合差 —— 用它算 diff 会把上个应用的文字当成本步变化
    // OCR清单同理: 识别帧间有抖动(同一处文字两帧识成两样),集合差会凭空出假差异 → 走像素
    let use_pixel = channel == "pixel" || before.els.is_empty() || after.els.is_empty()
        || before.suspect || after.suspect || before.ocr || after.ocr;
    if !use_pixel {
        let count = |v: &Vec<Node>| {
            let mut m = std::collections::HashMap::new();
            for n in v { *m.entry(n.t.clone()).or_insert(0i32) += 1; }
            m
        };
        let (mb, ma) = (count(&before.els), count(&after.els));
        let mut added: Vec<String> = Vec::new();
        let mut removed: Vec<String> = Vec::new();
        for (t, ca) in &ma {
            let cb = mb.get(t).unwrap_or(&0);
            if ca > cb { added.push(tcut(t, 18)); }
        }
        for (t, cb) in &mb {
            let ca = ma.get(t).unwrap_or(&0);
            if cb > ca { removed.push(tcut(t, 18)); }
        }
        if added.is_empty() && removed.is_empty() {
            // 矛盾检测: 树说"没变"但像素大变(压过背景) → 同应用内树未刷新,改采像素结论,
            // 防止把真实生效的动作记成空击、误落 ban
            if let (Some(a), Some(b)) = (&before.thumb, &after.thumb) {
                let pct = frames_diff_pct(a, b);
                let bg = before.noise.max(after.noise);
                if pct > bg + 13.0 && pct > 10.0 {
                    return (format!("pixel({}%,元素树未更新)", pct.round() as i32), false);
                }
            }
            return ("none".into(), false);
        }
        added.sort();
        removed.sort();
        added.truncate(4);
        removed.truncate(4);
        let mut s = String::new();
        if !added.is_empty() { s.push_str(&format!("+[{}]", added.join(","))); }
        if !removed.is_empty() {
            if !s.is_empty() { s.push(' '); }
            s.push_str(&format!("-[{}]", removed.join(",")));
        }
        (tcut(&s, 160), false)
    } else {
        match (&before.thumb, &after.thumb) {
            (Some(a), Some(b)) => {
                let pct = frames_diff_pct(a, b);
                let bg = before.noise.max(after.noise);
                if bg <= 0.0 {
                    let s = if pct <= QUIET_PCT {
                        "none".into()
                    } else {
                        format!("pixel({}%)", pct.round() as i32)
                    };
                    (s, false)
                } else if pct <= bg + 5.0 {
                    ("none".into(), false) // 未压过背景: 记没变化(空击/ban防线照常工作)
                } else {
                    let s = format!("pixel({}%)>bg({:.0}%)", pct.round() as i32, bg);
                    (s, pct <= bg + 13.0) // 刚过背景一点 → 边界区,交给仲裁
                }
            }
            _ => ("none".into(), false),
        }
    }
}

/// ms 仅在计划首个动作上出现(那是模型调用的真实耗时);队列内后续动作无模型开销,不写 ms
fn log_act(log: &mut Log, n: u32, a: &ActN, by: &str, ms: Option<u64>) {
    let mut m = serde_json::Map::new();
    m.insert("r".into(), json!("act"));
    m.insert("n".into(), json!(n));
    m.insert("a".into(), json!(a.a));
    if let Some(v) = a.x { m.insert("x".into(), json!(v)); }
    if let Some(v) = a.y { m.insert("y".into(), json!(v)); }
    if let Some(v) = a.x2 { m.insert("x2".into(), json!(v)); }
    if let Some(v) = a.y2 { m.insert("y2".into(), json!(v)); }
    if let Some(v) = &a.text { m.insert("text".into(), json!(v)); }
    if let Some(v) = &a.what { m.insert("what".into(), json!(v)); }
    m.insert("by".into(), json!(by));
    if let Some(v) = ms { m.insert("ms".into(), json!(v)); }
    log.put(Value::Object(m));
}

fn load_lessons(path: &str) -> Vec<Value> {
    let Ok(s) = fs::read_to_string(path) else { return vec![] };
    s.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["r"] == "lesson" && v["t"].as_str().map(|t| !t.is_empty()).unwrap_or(false))
        .collect()
}

#[derive(serde::Deserialize, Default)]
struct ParamsFile {
    settle_ms: Option<u64>,
    diff_channel: Option<String>,
    origin_null_rate: Option<f32>,
}

pub fn episode(cfg: &Config, task: &str, goal: &str, serial: Option<String>,
               endless: bool, budget: u32, app: Option<String>) -> bool {
    // ── 目录与文件 ──
    let task_dir = format!("{}/tasks/{}", cfg.data_dir.trim_end_matches('/'), task);
    let run_dir = format!("{}/runs/{}", task_dir, now_tag());
    if fs::create_dir_all(&run_dir).is_err() {
        eprintln!("✗ 建不了运行目录 {run_dir}");
        return false;
    }
    let tmp = std::env::temp_dir().join(format!("phonefarm-{}", std::process::id()));
    let tmp = tmp.to_string_lossy().to_string();
    let _ = fs::create_dir_all(&tmp);
    let mut log = Log {
        f: fs::OpenOptions::new().create(true).append(true)
            .open(format!("{run_dir}/log.jsonl")).ok(),
    };
    log.put(json!({"v": 1}));
    log.put(json!({"r": "goal", "t": goal}));

    let lessons = load_lessons(&format!("{task_dir}/lessons.jsonl"));
    // 全局经验: 跨任务共享(tasks/_global/),"游戏里用home逃生"这类学费只交一次
    let global_dir = format!("{}/tasks/_global", cfg.data_dir.trim_end_matches('/'));
    let glessons = load_lessons(&format!("{global_dir}/lessons.jsonl"));
    // 交互网(build_tree.py 离线产出,局末自动重算): 页面身份证+熟路。没有也能跑,只是没有goto
    let tree = Tree::load(&format!("{task_dir}/tree.json"));
    if tree.is_some() { println!("(载入交互网: {}页/{}边)", tree.as_ref().unwrap().pages.len(), tree.as_ref().unwrap().edges.len()); }
    // OCR文字备胎自举: 没编译过就现场编一次(几秒;失败不碍事,该通道自动关闭)
    if !std::path::Path::new("ocr").exists() && std::path::Path::new("ocr.swift").exists() {
        println!("(编译OCR文字备胎 ocr.swift…)");
        let _ = std::process::Command::new("swiftc").args(["-O", "ocr.swift", "-o", "ocr"]).output();
    }
    let ep_no = fs::read_dir(format!("{task_dir}/runs")).map(|d| d.count()).unwrap_or(1) as i64;

    // ── 感知参数(params.toml 白名单) ──
    let params_path = format!("{task_dir}/params.toml");
    let mut settle_ms = cfg.settle_ms;
    let mut channel = "els".to_string();
    let mut origin_null: Option<f32> = None;
    let mut params_active = false;
    if let Ok(s) = fs::read_to_string(&params_path) {
        if let Ok(p) = toml::from_str::<ParamsFile>(&s) {
            if let Some(v) = p.settle_ms { settle_ms = v.clamp(800, 4000); }
            if let Some(c) = p.diff_channel {
                if c == "els" || c == "pixel" { channel = c; }
            }
            origin_null = p.origin_null_rate;
            params_active = true;
            println!("(载入 params.toml: settle={settle_ms}ms 通道={channel})");
        }
    }

    let phone = Adb::new(serial, tmp.clone());
    let mut brain = Brain::new(cfg.providers.clone(), tmp.clone());
    let (realw, realh) = phone.size();

    // ── 安装清单(局内静态,注入上下文供 launch 选择) ──
    let apps = phone.launchable_apps();
    let apps_line = apps.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(",");

    // ── 开局归位(确定性,零模型调用): 声明了目标应用且前台不符 → 掐掉可杀的前台,回桌面 ──
    if let Some(target) = &app {
        let fg = phone.foreground_pkg();
        if !fg.is_empty() && fg != *target && !fg.contains("launcher") {
            let kill = pkg_killable(&fg, Some(target));
            let act = if kill { "force-stop+home" } else { "home" };
            println!("(开局归位: 前台{fg} ≠ 目标{target},{act})");
            if kill { phone.force_stop(&fg); }
            phone.home();
            std::thread::sleep(std::time::Duration::from_millis(900));
            log.put(json!({"r":"hook","kind":"orient","from":fg,"target":target,"act":act}));
        }
    }

    // ── 局内状态 ──
    let mut window: Vec<(String, String)> = Vec::new(); // (act行, diff) 0~999 空间
    let mut diffs_all: Vec<String> = Vec::new();
    let mut stream: Vec<String> = Vec::new(); // 复盘材料(全量紧凑流)
    let mut bans: Vec<Ban> = Vec::new();
    let mut clusters: Vec<(i32, i32, u32)> = Vec::new(); // 空击计数簇(图片像素)
    let mut note = String::new();
    let mut reject_streak = 0u32;
    let mut stall = 0u32;
    let mut stall_anchor: Option<String> = None;
    let mut heal_used = false;
    let mut heal_origin = "";
    let mut ban_count = 0u32;
    let mut exec_steps = 0u32;
    let mut null_steps = 0u32;
    let mut dyn_steps = 0u32;   // 定帧时画面未安静的步数
    let mut arbit_left: u32 = 5; // 边界仲裁余额(每局至多5次)
    let mut escapes_left: u32 = 10; // 内容过滤盲移余额(每局至多10次)
    let mut stop_reason: Option<&str> = None;
    let mut done_claim = false;
    let mut logged_seq = 0u32;
    let mut capseq = 1u32;

    // ── 计划队列 ──
    let mut queue: VecDeque<ActN> = VecDeque::new();
    let mut taps_hist: VecDeque<(i32, i32)> = VecDeque::new(); // 已执行的连续tap落点(图片像素)
    let mut plan_by = String::new();
    let mut plan_ms: Option<u64> = None; // 只随计划首动作入日志
    let mut pending_note: Option<String> = None;

    let Some(mut cap) = capture(&phone, &run_dir, capseq, cfg, &tmp, settle_ms, realw, realh, app.as_deref()) else {
        eprintln!("✗ 首屏采集失败");
        return false;
    };
    stream.push(format!("goal: {goal}"));
    println!("任务[{task}] 局{ep_no} | {goal}");
    println!("屏{realw}x{realh} 图{}x{} | 通道{channel} 等安静上限{settle_ms}ms 计划上限{} | 经验{}条+通用{}条 | 应用{}个",
        cap.w, cap.h, cfg.plan_max, lessons.len(), glessons.len(), apps.len());
    println!("{}", "=".repeat(56));

    let mut n = 0u32;
    loop {
        n += 1;
        if endless {
            if brain.calls >= budget && queue.is_empty() { stop_reason = Some("budget"); break; }
        } else if n > cfg.max_steps {
            stop_reason = Some("max_steps");
            break;
        }

        // ① 本轮画面记录(采集在上一动作末尾或开局完成)
        log_screen(&mut log, &cap, n, realw, realh, &mut logged_seq);

        // ②③ 队列空则组装上下文并要一份计划
        if queue.is_empty() {
            // 本地小地图: 当前页的熟路邻居(供模型 goto 直达)
            let map_line = match tree.as_ref().and_then(|t| t.page_of(&cap.els)) {
                Some(p) => tree.as_ref().unwrap().map_line(p.id).unwrap_or_default(),
                None => String::new(),
            };
            let user = render_ctx(goal, &glessons, &lessons, &apps_line, &map_line, &window, &bans, &note, &cap, realw, realh);
            let _ = fs::OpenOptions::new().create(true).append(true)
                .open(format!("{tmp}/ctx.log"))
                .map(|mut f| writeln!(f, "── 步#{n} ──\n{user}\n"));
            match plan_call(&mut brain, cfg, &user, &cap.img) {
                Ok((acts, note_new, by, ms)) => {
                    if acts.len() > 1 { println!("      📋 {}给出{}步计划", by, acts.len()); }
                    queue = acts.into();
                    plan_by = by;
                    plan_ms = Some(ms);
                    pending_note = note_new;
                }
                Err(e) if e == "内容过滤" && escapes_left > 0 => {
                    // 画面被安全审核拒绝(确定性): 不等模型,盲滑一步离开,重新采集再决策
                    escapes_left -= 1;
                    println!("      🚧 画面被内容过滤拒绝,盲移离开(余{escapes_left}次)");
                    let esc = ActN { a: "scroll_down".into(), ..Default::default() };
                    exec(&phone, &esc, &cap, realw, realh, &apps);
                    log_act(&mut log, n, &esc, "(盲移)", None);
                    let d = "escape(内容过滤)".to_string();
                    log.put(json!({"r":"diff","n":n,"d":d}));
                    println!("[{n}] ·盲移 | scroll_down → {d}");
                    window.push((format!("act#{n} scroll_down(盲移)"), d.clone()));
                    if window.len() > cfg.window_pairs { window.remove(0); }
                    diffs_all.push(d.clone());
                    stream.push(format!("act#{n} scroll_down → escape(内容过滤)"));
                    stall += 1;
                    if stall >= cfg.stall_limit { stop_reason = Some("watchdog"); break; }
                    capseq += 1;
                    match capture(&phone, &run_dir, capseq, cfg, &tmp, settle_ms, realw, realh, app.as_deref()) {
                        Some(c) => cap = c,
                        None => { stop_reason = Some("capture_fail"); break; }
                    }
                    continue;
                }
                Err(e) => {
                    println!("✗ 大脑失联: {e}");
                    stop_reason = Some("model_fail");
                    break;
                }
            }
        }
        let mut act = queue.pop_front().unwrap();
        let aline = act_line(n, &act); // 0~999 空间的展示行(在gate换算前)
        let ms_field = plan_ms.take();
        let ms_disp = ms_field.map(|v| format!("{v}ms")).unwrap_or_else(|| "·计划".into());

        // ④ 门
        if let Some(reason) = gate(&mut act, &cap, &bans, &taps_hist, cfg, &tmp, &apps, realw, realh) {
            log_act(&mut log, n, &act, &plan_by, ms_field);
            let d = format!("rejected({reason})");
            log.put(json!({"r":"diff","n":n,"d":d}));
            println!("[{n}] {ms_disp} {plan_by} | {aline} ⛔ {d}");
            window.push((aline.clone(), d.clone()));
            if window.len() > cfg.window_pairs { window.remove(0); }
            diffs_all.push(d.clone());
            stream.push(format!("{aline} → {d}"));
            reject_streak += 1;
            if let Some(t) = pending_note.take() {
                let t = tcut(&t, cfg.note_max_chars);
                log.put(json!({"r":"note","t":t}));
                note = t;
            }
            // 连续驳回:多半是坐标点在文字旁的空白。给一次强提示,让模型对准或改策略。
            if reject_streak >= 2 {
                note = format!("你已连续{reject_streak}次点在空白处被驳回。把坐标对准某个可见文字/图标的正中心,或改用 scroll_down 翻出新内容,不要在原位微调。");
            }
            if !queue.is_empty() { println!("      ⏸ 剩余{}步计划作废", queue.len()); queue.clear(); }
            stall += 1;
            if stall >= cfg.stall_limit { stop_reason = Some("watchdog"); break; }
            capseq += 1;
            match capture(&phone, &run_dir, capseq, cfg, &tmp, settle_ms, realw, realh, app.as_deref()) {
                Some(c) => cap = c,
                None => { stop_reason = Some("capture_fail"); break; }
            }
            continue; // 回到第①步
        }
        reject_streak = 0; // 过门即清零

        // 🧭 goto: 沿熟路零模型调用直达。计划里 goto 已被截为最后一步;
        // 走完/走断都把画面交还模型重新决策(网是参考不是权威)。
        if act.a == "goto" {
            let tgt = act.text.clone().unwrap_or_default();
            let route = tree.as_ref().and_then(|t| {
                let to = t.resolve(&tgt)?;
                let from = t.page_of(&cap.els).map(|p| p.id)?;
                t.route(from, to).map(|r| (r, to))
            });
            let Some((hops, to)) = route else {
                // 当页不在网中/目标不存在/无熟路: 驳回交还模型(画面没动,不重采)
                log_act(&mut log, n, &act, &plan_by, ms_field);
                let d = format!(
                    "rejected(goto {}不可达: {};自行导航)",
                    tcut(&tgt, 8),
                    if tree.as_ref().and_then(|t| t.page_of(&cap.els)).is_some() {
                        "无熟路连通"
                    } else {
                        "当前页不在网中"
                    }
                );
                log.put(json!({"r":"diff","n":n,"d":d}));
                println!("[{n}] {ms_disp} {plan_by} | {aline} ⛔ {d}");
                window.push((aline.clone(), d.clone()));
                if window.len() > cfg.window_pairs { window.remove(0); }
                diffs_all.push(d.clone());
                stream.push(format!("{aline} → {d}"));
                stall += 1;
                if stall >= cfg.stall_limit { stop_reason = Some("watchdog"); break; }
                continue;
            };
            if hops.is_empty() {
                println!("      🧭 goto P{to}: 已在目的地");
            } else {
                println!("      🧭 goto P{to}: {}段熟路,零模型调用", hops.len());
            }
            let mut ms_left = ms_field; // goto那次模型调用的耗时记在首段上
            let mut walked_all = true;
            for hop in &hops {
                log_screen(&mut log, &cap, n, realw, realh, &mut logged_seq);
                let Some(hact) = act_from_edge(hop, &cap, realw, realh) else {
                    println!("      🧭 熟路断了: 当前清单里找不到 {}", hop.label);
                    let d = format!("树✗(找不到按钮{})", hop.label);
                    log.put(json!({"r":"diff","n":n,"d":d}));
                    println!("[{n}] ·熟路 | goto断 → {d}");
                    window.push((format!("act#{n} goto·断"), d.clone()));
                    if window.len() > cfg.window_pairs { window.remove(0); }
                    diffs_all.push(d.clone());
                    stream.push(format!("act#{n} goto断({}) → {d}", hop.label));
                    walked_all = false;
                    break;
                };
                let aline_h = act_line(n, &hact);
                let ms_h = ms_left.take();
                exec(&phone, &hact, &cap, realw, realh, &apps);
                if hact.a == "tap" {
                    taps_hist.push_back((hact.x.unwrap_or(0) as i32, hact.y.unwrap_or(0) as i32));
                    while taps_hist.len() > 4 { taps_hist.pop_front(); }
                } else {
                    taps_hist.clear();
                }
                exec_steps += 1;
                capseq += 1;
                // 跳段用短定格: 到达判定靠页面身份证,不必等画面完全安静
                let Some(nc) = capture(&phone, &run_dir, capseq, cfg, &tmp, 1200, realw, realh, app.as_deref()) else {
                    stop_reason = Some("capture_fail");
                    break;
                };
                let landed = tree.as_ref().and_then(|t| t.page_of(&nc.els).map(|p| (p.id, p.name.clone())));
                let (d, hop_ok) = match &landed {
                    Some((pid, pname)) if *pid == hop.to =>
                        (format!("树✓P{}[{}]", hop.to, tcut(pname, 10)), true),
                    Some((pid, pname)) => (format!("树✗到了P{}[{}]", pid, tcut(pname, 10)), false),
                    None => ("树✗陌生页".to_string(), false),
                };
                log_act(&mut log, n, &hact, "(树)", ms_h);
                log.put(json!({"r":"diff","n":n,"d":d}));
                println!("[{n}] ·熟路 | {aline_h} → {d}");
                window.push((aline_h.clone(), d.clone()));
                if window.len() > cfg.window_pairs { window.remove(0); }
                diffs_all.push(d.clone());
                stream.push(format!("{aline_h} → {d}"));
                cap = nc;
                n += 1;
                if !hop_ok {
                    walked_all = false;
                    break;
                }
            }
            if stop_reason.is_some() { break; }
            if walked_all {
                stall = 0;
                stall_anchor = None;
            } else {
                stall += 1;
                if stall >= cfg.stall_limit { stop_reason = Some("watchdog"); break; }
            }
            if let Some(t) = pending_note.take() {
                let t = tcut(&t, cfg.note_max_chars);
                log.put(json!({"r":"note","t":t}));
                note = t;
            }
            continue;
        }

        // done: 记录后离开循环, 复核在局末
        if act.a == "done" {
            log_act(&mut log, n, &act, &plan_by, ms_field);
            println!("[{n}] {ms_disp} {plan_by} | done: {}", act.text.as_deref().unwrap_or(""));
            stream.push(format!("act#{n} done({})", act.text.as_deref().unwrap_or("")));
            if let Some(t) = pending_note.take() {
                let t = tcut(&t, cfg.note_max_chars);
                log.put(json!({"r":"note","t":t}));
            }
            done_claim = true;
            break;
        }

        // ⑤ 执行(定格并入第⑥步: 采集时等画面安静)
        exec(&phone, &act, &cap, realw, realh, &apps);
        if act.a == "tap" {
            taps_hist.push_back((act.x.unwrap_or(0) as i32, act.y.unwrap_or(0) as i32));
            while taps_hist.len() > 4 { taps_hist.pop_front(); }
        } else {
            taps_hist.clear(); // 连点只算"连续tap",夹其他动作即重新数
        }
        exec_steps += 1;

        // ⑥ 等画面安静后定帧 + 实测差异(与拉元素列表并行)
        capseq += 1;
        let Some(newcap) = capture(&phone, &run_dir, capseq, cfg, &tmp, settle_ms, realw, realh, app.as_deref()) else {
            stop_reason = Some("capture_fail");
            break;
        };
        if newcap.noise > 0.0 { dyn_steps += 1; }
        let (d_raw, border) = diff_of(&cap, &newcap, &channel);
        let d = if border {
            // 边界区: 差异刚压过背景一点,问一次仲裁(限额内)
            if arbit_left > 0 {
                arbit_left -= 1;
                match arbit_changed(&mut brain, cfg, &cap.img, &newcap.img) {
                    Some((true, desc, by)) => {
                        log.put(json!({"r":"hook","kind":"arbit","changed":true,"desc":desc,"by":by}));
                        println!("      ⚖ 边界仲裁({by}): 变化成立 {desc}");
                        d_raw
                    }
                    Some((false, desc, by)) => {
                        log.put(json!({"r":"hook","kind":"arbit","changed":false,"desc":desc,"by":by}));
                        println!("      ⚖ 边界仲裁({by}): 判为背景动静 {desc}");
                        "none".into()
                    }
                    None => {
                        println!("      ⚖ 边界仲裁调用失败,按背景动静处理");
                        "none".into()
                    }
                }
            } else {
                "none".into() // 仲裁额度用尽: 保守按没变化
            }
        } else {
            d_raw
        };
        log_act(&mut log, n, &act, &plan_by, ms_field);
        log.put(json!({"r":"diff","n":n,"d":d}));
        println!("[{n}] {ms_disp} {plan_by} | {aline} → {d}");
        if let Some(t) = pending_note.take() {
            let t = tcut(&t, cfg.note_max_chars);
            log.put(json!({"r":"note","t":t}));
            note = t;
        }
        window.push((aline.clone(), d.clone()));
        if window.len() > cfg.window_pairs { window.remove(0); }
        diffs_all.push(d.clone());
        stream.push(format!("{aline} → {d}"));

        let is_null = d == "none";
        if is_null {
            null_steps += 1;
            stall += 1;
            if stall == 1 { stall_anchor = Some(cap.img.clone()); } // 无进展段起点画面
            if !queue.is_empty() { println!("      ⏸ 动作无变化,剩余{}步计划作废", queue.len()); queue.clear(); }
        } else {
            stall = 0;
            stall_anchor = None;
        }

        // 空击计数 → 预ban仲裁 → ban
        if act.a == "tap" && is_null {
            let (xi, yi) = (act.x.unwrap_or(0) as i32, act.y.unwrap_or(0) as i32);
            let hit = clusters.iter_mut()
                .find(|(cx, cy, _)| (*cx - xi).abs() < cfg.ban_radius && (*cy - yi).abs() < cfg.ban_radius);
            let count = match hit {
                Some((_, _, c)) => { *c += 1; *c }
                None => { clusters.push((xi, yi, 1)); 1 }
            };
            if count >= cfg.ban_strikes {
                let mut banned = false;
                if cfg.hook_for("pre_ban").and_then(|h| cfg.prompt_of(h)).is_some() {
                    match arbit_changed(&mut brain, cfg, &cap.img, &newcap.img) {
                        Some((changed, desc, aby)) => {
                            log.put(json!({"r":"hook","kind":"arbit","changed":changed,"desc":desc,"by":aby}));
                            stream.push(format!("hook arbit: changed={changed} {desc}"));
                            println!("      ⚖ 仲裁({aby}): changed={changed} {desc}");
                            banned = !changed;
                        }
                        None => println!("      ⚖ 仲裁调用失败,本轮不落ban"),
                    }
                } else {
                    banned = true; // 未配置仲裁钩子: 两次空击直接ban
                }
                if banned {
                    let why = format!("同点{count}次空击,仲裁确认无变化");
                    log.put(json!({"r":"ban","a":"tap","x":xi,"y":yi,"rad":cfg.ban_radius,"why":why}));
                    stream.push(format!("ban: tap({},{}) {why}", to_norm(xi, cap.w), to_norm(yi, cap.h)));
                    println!("      🚫 ban: tap({xi},{yi})±{}", cfg.ban_radius);
                    bans.push(Ban { x: xi, y: yi, rad: cfg.ban_radius, why });
                    ban_count += 1;
                }
                clusters.retain(|(cx, cy, _)| (*cx - xi).abs() >= cfg.ban_radius || (*cy - yi).abs() >= cfg.ban_radius);
            }
        }

        // 感知自愈(每局一次): ban 累积到阈值 → 清ban + 加长定格 + 换差异通道
        if ban_count >= cfg.heal_ban_threshold && !heal_used {
            heal_used = true;
            heal_origin = "ban溢出";
            let cleared = bans.len();
            bans.clear();
            clusters.clear();
            settle_ms = if settle_ms < 2000 { 2000 } else { 3000 };
            channel = if channel == "els" { "pixel".into() } else { "els".into() };
            log.put(json!({"r":"hook","kind":"heal","cleared":cleared,"settle_ms":settle_ms,"channel":channel}));
            stream.push(format!("hook heal: cleared={cleared} settle={settle_ms} channel={channel}"));
            println!("      🩹 感知自愈: 清{cleared}条ban, settle={settle_ms}ms, 通道={channel}");
        }

        // 看门狗将触发时先仲裁一次: 通道说无进展但画面实际在变 → 通道盲区, 就地自愈
        if stall >= cfg.stall_limit {
            let mut rescued = false;
            if !heal_used {
                if let (Some(anchor), Some(h)) = (stall_anchor.clone(), cfg.hook_for("pre_ban")) {
                    if let Some(p) = cfg.prompt_of(h) {
                        let u = "第一张为若干步之前的截图,第二张为当前截图。".to_string();
                        for att in 0..2 {
                            if let Ok(out) = brain.call(p, &u, &[&anchor, &newcap.img], 150, att) {
                                let objs = extract_objs(&out.text);
                                if let Some(o) = objs.iter().find(|o| o["r"] == "hook" && o["kind"] == "arbit") {
                                    let changed = o["changed"].as_bool().unwrap_or(false);
                                    let desc = o["desc"].as_str().unwrap_or("");
                                    log.put(json!({"r":"hook","kind":"arbit","changed":changed,"desc":desc,"by":out.by}));
                                    stream.push(format!("hook arbit(停机前): changed={changed} {desc}"));
                                    println!("      ⚖ 停机前仲裁({}): changed={changed} {desc}", out.by);
                                    if changed {
                                        heal_used = true;
                                        heal_origin = "通道盲区";
                                        channel = if channel == "els" { "pixel".into() } else { "els".into() };
                                        log.put(json!({"r":"hook","kind":"heal","cleared":0,"settle_ms":settle_ms,"channel":channel}));
                                        stream.push(format!("hook heal: 差异通道盲区 → channel={channel}"));
                                        println!("      🩹 感知自愈: 通道盲区, 切换差异通道 → {channel}");
                                        stall = 0;
                                        stall_anchor = None;
                                        rescued = true;
                                    }
                                    break;
                                }
                                brain.blame(&out.by);
                            }
                        }
                    }
                }
            }
            if !rescued {
                stop_reason = Some("watchdog");
                cap = newcap;
                break;
            }
        }
        cap = newcap; // 本帧复用为下一动作第①步
    }

    // ── 终局画面补记 ──
    log_screen(&mut log, &cap, n, realw, realh, &mut logged_seq);

    // ── 时间点: 非done终止 → budget 记录 ──
    if let Some(sr) = stop_reason {
        let limit = if endless { budget } else { cfg.max_steps };
        log.put(json!({"r":"hook","kind":"budget","spent":brain.calls,"limit":limit,"stop":sr,"stall":stall}));
        stream.push(format!("hook budget: stop={sr} spent={}", brain.calls));
        println!("      ⏱ 终止: {sr} (调用{}次)", brain.calls);
    }

    // ── 时间点: done复核(终局一律复核, 不采信执行者自述) ──
    let mut achieved = false;
    let mut verdict_line = "(复核未执行)".to_string();
    if let Some(h) = cfg.hook_for("done") {
        if let Some(p) = cfg.prompt_of(h) {
            let tail: Vec<String> = diffs_all.iter().rev().take(5).rev().cloned().collect();
            // 覆盖便签一并给复核:执行者的主张,复核仍须与最终画面/差异相互印证,不是照抄。
            // (此前复核只看得到最近5条差异,停在非汇总页时无法核对覆盖主张,产生过误判)
            let mut u = format!(
                "任务: {goal}\n执行者主张的覆盖清单(便签,需与画面印证,不可单方面采信): {}\n最近差异: {}\n最终画面元素:\n",
                if note.is_empty() { "(无)" } else { &note },
                tail.join(" | ")
            );
            if cap.suspect {
                // 假树不给复核员 —— 上一版曾把上个应用的陈旧树当"最终画面元素"送审
                u.push_str(&format!("(元素列表不可信:树来自{}而前台是{},请只以最终截图为准)\n",
                    cap.els_pkg, cap.pkg));
            } else {
                for e in cap.els.iter().take(60) {
                    let bi = el_img_space(e, &cap, realw, realh);
                    u.push_str(&format!("{} [{},{},{},{}]\n", e.t, bi[0], bi[1], bi[2], bi[3]));
                }
            }
            if !cap.pkg.is_empty() {
                u.push_str(&format!("当前前台应用: {}\n", cap.pkg));
            }
            u.push_str("(最终截图见图)");
            let mut got = false;
            for att in 0..2 {
                if let Ok(out) = brain.call(p, &u, &[&cap.img], 200, att) {
                    let objs = extract_objs(&out.text);
                    if let Some(o) = objs.iter().find(|o| o["r"] == "hook" && o["kind"] == "verdict") {
                        achieved = o["achieved"].as_bool().unwrap_or(false);
                        let reason = o["reason"].as_str().unwrap_or("");
                        log.put(json!({"r":"hook","kind":"verdict","achieved":achieved,"reason":reason,"by":out.by}));
                        verdict_line = format!("achieved={achieved} {reason}");
                        stream.push(format!("hook verdict: {verdict_line}"));
                        println!("      ✔ 复核({}): {verdict_line}", out.by);
                        got = true;
                        break;
                    }
                    brain.blame(&out.by);
                }
            }
            if !got {
                verdict_line = "(复核调用失败)".into();
                println!("      ✔ 复核失败,无verdict记录");
            }
        }
    }

    // ── 时间点: 局末复盘 → lessons.jsonl ──
    if let Some(h) = cfg.hook_for("episode_end") {
        if let Some(p) = cfg.prompt_of(h) {
            let mut u = format!("任务: {goal}\n本局序号: {ep_no}\n复核结论: {verdict_line}\n本局记录:\n{}\n现有经验:\n",
                stream.join("\n"));
            if lessons.is_empty() {
                u.push_str("(无)\n");
            } else {
                for l in &lessons {
                    u.push_str(&format!("{}\n", serde_json::to_string(l).unwrap_or_default()));
                }
            }
            if !glessons.is_empty() {
                u.push_str("现有通用经验(跨任务共享,已存在,勿重复输出):\n");
                for l in &glessons {
                    u.push_str(&format!("{}\n", l["t"].as_str().unwrap_or("")));
                }
            }
            let mut newles: Vec<Value> = Vec::new();
            for att in 0..2 {
                if let Ok(out) = brain.call(p, &u, &[], 900, att) {
                    let objs = extract_objs(&out.text);
                    let les: Vec<Value> = objs.into_iter()
                        .filter(|o| o["r"] == "lesson" && o["t"].as_str().map(|t| !t.is_empty()).unwrap_or(false))
                        .collect();
                    if !les.is_empty() {
                        newles = les;
                        println!("      📔 复盘({}): 经验 {}条 → lessons.jsonl", out.by, newles.len());
                        break;
                    }
                    brain.blame(&out.by);
                }
            }
            // scope=global 的条目不占本任务名额,分流进共享经验库(上限10条,按内容去重)
            let (glob_new, task_new): (Vec<Value>, Vec<Value>) =
                newles.into_iter().partition(|l| l["scope"] == "global");
            let mut newles = task_new;
            if !glob_new.is_empty() {
                let _ = fs::create_dir_all(&global_dir);
                let gpath = format!("{global_dir}/lessons.jsonl");
                let mut gall = load_lessons(&gpath);
                let mut added = 0usize;
                for l in glob_new {
                    let t = tcut(l["t"].as_str().unwrap_or(""), 120);
                    if t.is_empty() || gall.iter().any(|e| e["t"].as_str() == Some(t.as_str())) {
                        continue;
                    }
                    if gall.len() >= 10 { break; }
                    gall.push(json!({"r":"lesson","scope":"global","t":t,"from":task}));
                    added += 1;
                }
                if added > 0 {
                    let mut body = String::from("{\"v\":1}\n");
                    for l in &gall { body.push_str(&format!("{l}\n")); }
                    let gtmp = format!("{gpath}.tmp");
                    if fs::write(&gtmp, &body).is_ok() {
                        let _ = fs::rename(&gtmp, &gpath);
                        println!("      📔 通用经验 +{added}条 → tasks/_global/lessons.jsonl");
                    }
                }
            }
            if !newles.is_empty() {
                newles.truncate(cfg.lesson_max_items);
                // 兜底: reflect 偶尔静默丢掉"本局未涉及"的旧经验(实测连丢三轮收尾教训)。
                // 漏带的旧条原样补回;要淘汰某条须显式重写其内容,不能靠不写。
                let new_ids: std::collections::HashSet<i64> =
                    newles.iter().filter_map(|l| l["id"].as_i64()).collect();
                for l in &lessons {
                    if let Some(id) = l["id"].as_i64() {
                        if !new_ids.contains(&id) && newles.len() < cfg.lesson_max_items {
                            newles.push(l.clone());
                        }
                    }
                }
                let mut body = String::from("{\"v\":1}\n");
                for (i, l) in newles.iter().enumerate() {
                    let o = json!({
                        "r": "lesson",
                        "id": l["id"].as_i64().unwrap_or(i as i64 + 1),
                        "t": tcut(l["t"].as_str().unwrap_or(""), 120),
                        "born": l["born"].as_i64().unwrap_or(ep_no),
                        "win": l["win"].as_i64().unwrap_or(0),
                        "lose": l["lose"].as_i64().unwrap_or(0)
                    });
                    body.push_str(&format!("{o}\n"));
                }
                let tmp_path = format!("{task_dir}/lessons.jsonl.tmp");
                if fs::write(&tmp_path, &body).is_ok() {
                    let _ = fs::rename(&tmp_path, format!("{task_dir}/lessons.jsonl"));
                }
            } else {
                println!("      📔 复盘无有效经验输出,保留原 lessons.jsonl");
            }
        }
    }

    // ── 局末: 交互网重算(build_tree.py 汇总全部runs含本局,<1s;下一局开场即用上新路) ──
    if std::path::Path::new("build_tree.py").exists()
        && fs::metadata(format!("{task_dir}/runs")).is_ok()
    {
        if let Ok(o) = std::process::Command::new("python3")
            .arg("build_tree.py")
            .arg(&task_dir)
            .output()
        {
            if o.status.success() {
                log.put(json!({"r":"hook","kind":"tree","rebuilt":true}));
                println!("      🧭 交互网已重算 → {task_dir}/tree.json");
            }
        }
    }

    // ── 感知参数固化 / 回退 ──
    let null_rate = null_steps as f32 / exec_steps.max(1) as f32;
    if params_active {
        if let Some(o) = origin_null {
            if null_rate > o + 0.05 {
                let _ = fs::remove_file(&params_path);
                println!("      ⚙ params.toml 回退删除(空击率{:.0}% > 来源{:.0}%)", null_rate * 100.0, o * 100.0);
            }
        }
    } else if heal_used {
        let body = format!(
            "# 感知自愈参数 — 程序按白名单落盘\n# 来历: 局{ep_no}因{heal_origin}触发感知自愈后的参数\n# 回退: 使用本参数的一局空击率高于 origin_null_rate 时,程序删除本文件\n\nsettle_ms = {settle_ms}\ndiff_channel = \"{channel}\"\norigin_null_rate = {null_rate:.2}\n"
        );
        if fs::write(&params_path, body).is_ok() {
            println!("      ⚙ 自愈参数固化 → params.toml (settle={settle_ms} 通道={channel})");
        }
    }

    let _ = fs::remove_dir_all(&tmp);
    println!("{}", "=".repeat(56));
    println!("局{ep_no}结束 | 步数{n} 执行{exec_steps} 空击{null_steps} 动态屏{dyn_steps} ban{ban_count} | 调用{}次 | done={done_claim} achieved={achieved}",
        brain.calls);
    println!("记录: {run_dir}/log.jsonl");
    achieved
}
