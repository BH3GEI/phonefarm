//! 六步循环 + 记录流(契约v1) + 确定性检查 + 时间点表。
//! v0.3: 多步计划(一次调用1~plan_max个动作,逐步验证,遇挫弃约) + 双采集并行。
//! v0.4: 定格从"固定等待"改为"等画面安静"(连续两张小图一致即稳,上限settle_ms);
//!       不安静的屏记为动态(noise=背景动静幅度),像素差异需压过背景才算有效,
//!       边界差异(刚过背景一点)经仲裁裁定,每局限5次。
//! 程序只写 tasks/<任务>/ 之下: runs/<局>/log.jsonl、runs/<局>/step*.jpg、lessons.jsonl、params.toml。
use crate::brain::Brain;
use crate::device::{frames_diff_pct, mode_gray, patch_stats, thumb_gray, Adb, FullNode, Node};
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
    full: Vec<FullNode>, // 全量属性层(无损,含无文字容器;OCR帧为空)
    folded: Vec<crate::fold::Line>, // 折叠投影(纯函数f(full),采集时算一次;渲染/沙盘共用)
    img: String,    // jpg 绝对路径
    img_rel: String,
    xml_rel: String, // 原始XML落盘文件名(stepN.xml[.gz]);没dump到为空
    w: i32,
    h: i32,
    thumb: Option<Vec<u8>>,
    noise: f32,       // 采集时量到的背景动静幅度(%);0=画面安静
    pkg: String,      // 前台应用包名(采不到为空)
    activity: String, // 前台Activity全类名(采不到为空)
    ime: Option<bool>, // 软键盘是否弹起(读不到为None)
    els_pkg: String,  // 元素树的多数包名
    suspect: bool,    // 假树: 树包名≠前台包名且重抓无效 → 本步文字通道不可信
    ocr: bool,        // 本帧清单来自截图文字识别(UI树为空时的备胎)
    webview: bool,    // 本页主体为WebView(框架类语义+占屏过半): 网页黑盒,原生树只见外框
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

/// 历史里程碑折叠(v0.6 Step 2): 本局按页面分段,旧页压成单行里程碑,当前页全量展开。
/// 废除 window=5——上下文从 O(步数) 变 O(换页数),且当前页细节分毫不丢。
struct PageRun {
    key: (String, i64), // (activity, 身份证页id);未知分量为 ""/-1
    label: String,      // 展示名: P{id}[{页名}] 或 activity 短名
    lines: Vec<(String, String)>, // (act行, diff) —— 与旧window同一行格式
}

/// 换页判定: (activity, 页面身份证) 合取——任一【已知】分量变化即翻页。
/// 未知分量(dumpsys偶发失败=空activity;不在网中=-1页)不触发,防假里程碑。
/// activity 管跨Activity跳转(设置链实测区分度极好),身份证管单Activity内的页面切换(feed标签)。
fn page_boundary(prev: &(String, i64), cur: &(String, i64)) -> bool {
    let act = !prev.0.is_empty() && !cur.0.is_empty() && prev.0 != cur.0;
    let page = prev.1 >= 0 && cur.1 >= 0 && prev.1 != cur.1;
    act || page
}

fn run_label(activity: &str, pid: i64, pname: &str) -> String {
    if pid >= 0 {
        format!("P{}[{}]", pid, tcut(pname, 8))
    } else if !activity.is_empty() {
        activity.rsplit('.').next().unwrap_or(activity).to_string()
    } else {
        "?".into()
    }
}

/// 行入账: 永远append到当前页段(驳回/盲移/goto断点都算当前页的经历)
fn run_push(runs: &mut Vec<PageRun>, aline: String, d: String) {
    if runs.is_empty() {
        runs.push(PageRun { key: (String::new(), -1), label: "?".into(), lines: vec![] });
    }
    runs.last_mut().unwrap().lines.push((aline, d));
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

/// 单局结构化成绩单(主程序退出码 / benchmark 报表共用)
pub struct EpisodeResult {
    pub achieved: bool,
    pub run_id: String, // runs/ 下的时间戳目录名
    pub stop: String,   // done | budget | watchdog | max_steps | model_fail | capture_fail | orient…
    pub steps: u32,
    pub calls: u32,
    pub tokens: u64,
    pub wall_ms: u64,
}

/// 打摆警报词(给实例帮助模型举一反三:标题≠菜单名是最常见的"到了不认账")
const OSCILL_WARN: &str = "【系统警告:打摆干预】你已连续2次进入同一页面又立即返回!注意:页面实际标题往往与菜单名不一致(如点'广告设置'进去后标题是'为什么我会看到此广告')。严禁继续back!请仔细阅读当前页正文寻找目标内容,确认已到达就直接done。";

/// 动作签名(打摆检测): tap认"点了什么"不认裸坐标——坐标每步都在漂,按钮身份才稳。
/// 未点名的tap按50px粗桶近似(有what时坐标只是线索,吸附后由what定身份)。
fn act_sig(a: &ActN) -> String {
    match a.a.as_str() {
        "tap" => match a.what.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
            Some(w) => format!("tap[{}]", tcut(w, 12)),
            None => format!("tap({},{})", a.x.unwrap_or(-1) / 50, a.y.unwrap_or(-1) / 50),
        },
        // 探针认"问了什么": 同一问题反复问与反复点同一按钮同罪,进打摆账
        "inspect" | "find" | "history" => match a.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => format!("{}[{}]", a.a, tcut(t, 12)),
            None => format!("{}({},{})", a.a, a.x.unwrap_or(-1) / 50, a.y.unwrap_or(-1) / 50),
        },
        other => other.to_string(),
    }
}

/// 打摆判定: 两拍 X,Y,X,Y 或三拍 X,Y,Z,X,Y,Z,且拍内含 back/home(进出循环)。
/// 三拍是实测出来的变体: 警报后模型不秒退了,改成"读一会儿(wait/scroll)再走",照样进出循环。
/// 上滑下滑交替翻列表是合法探索(无back/home),不报警。
fn is_oscillating(sigs: &VecDeque<String>) -> bool {
    let n = sigs.len();
    if n >= 4 {
        let (a, b, c, d) = (&sigs[n - 4], &sigs[n - 3], &sigs[n - 2], &sigs[n - 1]);
        if a == c && b == d && a != b
            && (a.as_str() == "back" || b.as_str() == "back"
                || a.as_str() == "home" || b.as_str() == "home")
        { return true; }
    }
    if n >= 6 {
        let t: Vec<&String> = sigs.iter().skip(n - 6).collect();
        if t[0] == t[3] && t[1] == t[4] && t[2] == t[5]
            && !(t[0] == t[1] && t[1] == t[2])
            && ["back", "home"].iter().any(|k| {
                t[0].as_str() == *k || t[1].as_str() == *k || t[2].as_str() == *k
            })
        { return true; }
    }
    false
}

/// 打摆记账: 推入已执行动作签名(窗口8条);命中进出循环返回 true(fire_at 控制第3轮才再报)
fn osc_note(sigs: &mut VecDeque<String>, sig: String, fire_at: &mut usize) -> bool {
    sigs.push_back(sig);
    while sigs.len() > 8 { sigs.pop_front(); }
    if sigs.len() >= *fire_at && is_oscillating(sigs) {
        *fire_at = sigs.len() + 2;
        return true;
    }
    false
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
        let h_els = s.spawn(|| phone.els_full(cfg.els_timeout_ms));
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
    let (mut els, mut full, mut els_pkg, mut xml) = elsres;
    let st = phone.sys_state();
    let (mut fg, mut activity, mut ime) = (st.pkg, st.activity, st.ime);
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
            let (e2, f2n, p2, x2) = phone.els_full(cfg.els_timeout_ms);
            if !e2.is_empty() { els = e2; full = f2n; els_pkg = p2; xml = x2; }
            let s2 = phone.sys_state();
            if !s2.pkg.is_empty() { fg = s2.pkg; activity = s2.activity; ime = s2.ime; }
            if els.is_empty() || els_pkg.is_empty() || els_pkg == fg { break; }
        }
        suspect = !els.is_empty() && !els_pkg.is_empty() && els_pkg != fg;
        if suspect {
            println!("      👻 重抓后仍不符,本步文字通道降级(只信截图)");
        }
    }
    // 原始XML落盘(账本地面真值): 解析器也是投影,parser 漏抓任何属性都不再是数据损失。
    // gzip 压缩(shell自带,零依赖);压不动就留明文。与截图同为不进 git 的本地重资产。
    let mut xml_rel = String::new();
    if !xml.is_empty() {
        let xp = format!("{run_dir}/step{seq}.xml");
        if fs::write(&xp, &xml).is_ok() {
            xml_rel = format!("step{seq}.xml");
            let gz = std::process::Command::new("gzip").args(["-f", &xp]).output()
                .map(|o| o.status.success()).unwrap_or(false);
            if gz && fs::metadata(format!("{xp}.gz")).is_ok() { xml_rel.push_str(".gz"); }
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
    // 折叠投影: 假树帧不折(全量层来自陈旧树,折出来的骨架是上个应用的)
    let folded = if suspect { vec![] } else { crate::fold::fold(&full) };
    // WebView主导页判定: 框架类名(含X5等子类)+占屏过半——网页内按钮对原生树是黑盒,
    // H5活动页常拿JS拦back。类名是Android框架语义,非App特判。
    let webview = full.iter().any(|f| f.class.contains("WebView")
        && (f.b[2] - f.b[0]).max(0) as i64 * (f.b[3] - f.b[1]).max(0) as i64
            >= realw as i64 * realh as i64 / 2);
    Some(Cap { seq, els, full, folded, img, img_rel, xml_rel, w, h, thumb, noise,
               pkg: fg, activity, ime, els_pkg, suspect, ocr, webview })
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

/// 坐标框: 设备像素 → 图片像素
fn b_img_space(b: [i32; 4], cap: &Cap, realw: i32, realh: i32) -> [i32; 4] {
    let sx = |v: i32| (v as i64 * cap.w as i64 / realw.max(1) as i64) as i32;
    let sy = |v: i32| (v as i64 * cap.h as i64 / realh.max(1) as i64) as i32;
    [sx(b[0]), sy(b[1]), sx(b[2]), sy(b[3])]
}

/// 元素框: 设备像素 → 图片像素
fn el_img_space(n: &Node, cap: &Cap, realw: i32, realh: i32) -> [i32; 4] {
    b_img_space(n.b, cap, realw, realh)
}

/// 图片像素 → 0~999
fn to_norm(v: i32, span: i32) -> i64 {
    (v as i64 * NORM / span.max(1) as i64).clamp(0, NORM)
}

/// 坐标框(设备px) → 0~999 空间
fn b_norm(b: [i32; 4], cap: &Cap, realw: i32, realh: i32) -> [i64; 4] {
    let bi = b_img_space(b, cap, realw, realh);
    [to_norm(bi[0], cap.w), to_norm(bi[1], cap.h), to_norm(bi[2], cap.w), to_norm(bi[3], cap.h)]
}

/// 吸附/点名/落点判定的锚点源: 全量层的文字节点(无70条/40字截断——折叠渲染出的
/// 头行文字必须能吸附回其元素,否则模型看得见却点不着);OCR帧无全量层,用els(识别清单)。
fn anchors(cap: &Cap) -> Vec<(&str, [i32; 4])> {
    if cap.full.is_empty() {
        cap.els.iter().map(|n| (n.t.as_str(), n.b)).collect()
    } else {
        cap.full.iter().filter(|f| !f.t.is_empty()).map(|f| (f.t.as_str(), f.b)).collect()
    }
}

/// 沙盘候选源: 折叠后骨架的普通文字行——卡片内部噪音(点赞数/作者名)已收进折叠头,
/// 天然不进沙盘(#18 根治);无折叠层(OCR帧)退回els清单,即v0.5现状。
fn sandbox_texts(cap: &Cap) -> Vec<&str> {
    if cap.folded.is_empty() {
        cap.els.iter().map(|n| n.t.as_str()).collect()
    } else {
        cap.folded.iter()
            .filter(|l| l.kind == crate::fold::Kind::Plain)
            .map(|l| l.t.as_str())
            .collect()
    }
}

/// 按文字找元素: 精确等值优先,退而含串(≥2字)。
/// 多个命中取离 (hx,hy)@0~999 最近者 —— 模型给的坐标是"哪一个同名元素"的线索。
/// 返回 (元素文字, 图片px框)。
fn find_el(cap: &Cap, w: &str, hx: i64, hy: i64, realw: i32, realh: i32)
    -> Option<(String, [i32; 4])>
{
    let anc = anchors(cap);
    let wt = w.trim();
    let mut pool: Vec<&(&str, [i32; 4])> = anc.iter().filter(|(t, _)| t.trim() == wt).collect();
    if pool.is_empty() && wt.chars().count() >= 2 {
        pool = anc.iter().filter(|(t, _)| t.contains(wt)).collect();
    }
    pool.into_iter()
        .min_by_key(|(_, b)| {
            let bn = b_norm(*b, cap, realw, realh);
            ((bn[0] + bn[2]) / 2 - hx).abs() + ((bn[1] + bn[3]) / 2 - hy).abs()
        })
        .map(|(t, b)| (t.trim().to_string(), b_img_space(*b, cap, realw, realh)))
}

/// (x,y)@0~999 落在哪个文字元素框内 → 该框(图片px);取最小框(最内层元素)。
/// 只认文字锚点(与旧els语义一致): 无字容器不参与,防止空白落点被吸去容器中心。
fn box_at(cap: &Cap, x: i64, y: i64, realw: i32, realh: i32) -> Option<[i32; 4]> {
    anchors(cap).into_iter()
        .filter(|(_, b)| {
            let bn = b_norm(*b, cap, realw, realh);
            x >= bn[0] && x <= bn[2] && y >= bn[1] && y <= bn[3]
        })
        .map(|(_, b)| (b_norm(b, cap, realw, realh), b_img_space(b, cap, realw, realh)))
        .min_by_key(|(bn, _)| (bn[2] - bn[0]) * (bn[3] - bn[1]))
        .map(|(_, bi)| bi)
}

/// 附近元素候选(驳回时给模型指路): 距 (x,y)@0~999 最近的3个 "文字(x,y)"
fn nearby_els(cap: &Cap, x: i64, y: i64, realw: i32, realh: i32) -> String {
    let mut v: Vec<(i64, String)> = anchors(cap)
        .into_iter()
        .map(|(t, b)| {
            let bn = b_norm(b, cap, realw, realh);
            let d = ((bn[0] + bn[2]) / 2 - x).abs() + ((bn[1] + bn[3]) / 2 - y).abs();
            (d, format!("{}({},{})", tcut(t.trim(), 10), (bn[0] + bn[2]) / 2, (bn[1] + bn[3]) / 2))
        })
        .collect();
    v.sort_by_key(|(d, _)| *d);
    v.into_iter().take(3).map(|(_, s)| s).collect::<Vec<_>>().join("、")
}

/// 控件类名缩短: android 官方包前缀冗余,去掉;三方自绘类保留全名。原文在 XML 落盘里。
fn class_short(c: &str) -> &str {
    c.strip_prefix("android.widget.")
        .or_else(|| c.strip_prefix("android.view."))
        .unwrap_or(c)
}

/// 本帧画面入记录流(seq 去重)。主循环顶部、goto跳段、终局三处共用同一节奏。
/// els 保持旧形态(build_tree.py 契约);nodes 为全量属性层(仅记账,暂无运行时消费方);
/// activity/ime_shown/xml 一并入账。
fn log_screen(log: &mut Log, cap: &Cap, n: u32, realw: i32, realh: i32, logged_seq: &mut u32) {
    if cap.seq == *logged_seq { return; }
    let els_img: Vec<Value> = cap
        .els
        .iter()
        .map(|e| json!({"t": e.t, "b": el_img_space(e, cap, realw, realh)}))
        .collect();
    let mut rec = json!({"r":"screen","n":n,"els":els_img,"img":cap.img_rel,"pkg":cap.pkg});
    if !cap.full.is_empty() {
        let nodes_img: Vec<Value> = cap.full.iter().map(|f| {
            let mut m = serde_json::Map::new();
            if !f.t.is_empty() { m.insert("t".into(), json!(f.t)); }
            m.insert("b".into(), json!(b_img_space(f.b, cap, realw, realh)));
            if let Some(id) = &f.id { m.insert("id".into(), json!(id)); }
            if !f.class.is_empty() { m.insert("c".into(), json!(class_short(&f.class))); }
            if f.clickable { m.insert("clickable".into(), json!(true)); }
            if f.scrollable { m.insert("scrollable".into(), json!(true)); }
            if f.checkable { m.insert("checked".into(), json!(f.checked)); }
            m.insert("d".into(), json!(f.depth));
            Value::Object(m)
        }).collect();
        rec["nodes"] = json!(nodes_img);
    }
    if !cap.activity.is_empty() { rec["activity"] = json!(cap.activity); }
    if let Some(v) = cap.ime { rec["ime_shown"] = json!(v); }
    if !cap.xml_rel.is_empty() { rec["xml"] = json!(cap.xml_rel); }
    if cap.suspect { rec["suspect"] = json!(true); }
    if cap.ocr { rec["ocr"] = json!(true); }
    log.put(rec);
    *logged_seq = cap.seq;
}

/// 模型原文落账: 每次真实调用的完整回包原文(含JSON外的说明文字),按调用点分类。
/// in_ctx: step 的输入侧已全量在 ctx.log,不重复;verdict/reflect 的输入仅此一份,一并入账。
fn log_raw(log: &mut Log, hook: &str, n: u32, by: &str, ms: u64, text: &str, in_ctx: Option<&str>) {
    let mut m = serde_json::Map::new();
    m.insert("r".into(), json!("raw"));
    m.insert("hook".into(), json!(hook));
    m.insert("n".into(), json!(n));
    m.insert("by".into(), json!(by));
    m.insert("ms".into(), json!(ms));
    if let Some(u) = in_ctx { m.insert("in".into(), json!(u)); }
    m.insert("t".into(), json!(text));
    log.put(Value::Object(m));
}

/// 采集,失败则尝试复活设备后重采(每局一次): 模拟器/adb 中途卡死不再直接弃局。
/// 复活链: 重启adb server → 仍无心跳按 cfg.emulator_cmd 重启模拟器等开机。
fn capture_or_revive(phone: &Adb, run_dir: &str, seq: u32, cfg: &Config, tmp: &str, cap_ms: u64,
                     realw: i32, realh: i32, target: Option<&str>, revived: &mut bool) -> Option<Cap> {
    if let Some(c) = capture(phone, run_dir, seq, cfg, tmp, cap_ms, realw, realh, target) {
        return Some(c);
    }
    if *revived { return None; }
    *revived = true;
    println!("      🔧 采集失败,尝试设备复活(重启adb/模拟器)");
    if !phone.revive(&cfg.emulator_cmd) { return None; }
    capture(phone, run_dir, seq, cfg, tmp, cap_ms, realw, realh, target)
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
            let (txt, bi) = find_el(cap, t, 500, 500, realw, realh)?;
            Some(ActN {
                a: "tap".into(),
                x: Some(((bi[0] + bi[2]) / 2) as i64),
                y: Some(((bi[1] + bi[3]) / 2) as i64),
                what: Some(txt),
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
        "inspect" | "find" | "history" => format!(
            "act#{n} {}({})", a.a,
            a.text.as_deref().map(|t| tcut(t, 12))
                .unwrap_or_else(|| format!("{},{}", a.x.unwrap_or(-1), a.y.unwrap_or(-1)))
        ),
        o => format!("act#{n} {o}"),
    }
}

/// 契约式到达断言(--assert): 逐元素子串匹配,不跨元素拼接——"广告"+"设置"分居两处凑不成"广告设置"。
/// 断言权在测试契约手里: 命中即注入"实测事实",不靠模型对标题的语义猜认
/// (局31: 到了广告设置页却因页内标题是"为什么我会看到此广告"死不认账,烧光22次调用)
fn assert_hits_texts<'a>(texts: impl Iterator<Item = &'a str> + Clone, asserts: &[String])
    -> Vec<String>
{
    asserts.iter().filter_map(|w| {
        let w = w.trim();
        if !w.is_empty() && texts.clone().any(|t| t.trim().contains(w)) {
            Some(w.to_string())
        } else { None }
    }).collect()
}

/// 断言在全量层上测: 无70条/40字截断盲区(验收词落在第71个元素或长文40字后不再假阴性)。
/// OCR帧没有全量层,退回文字清单。
fn assert_hits(cap: &Cap, asserts: &[String]) -> Vec<String> {
    if cap.full.is_empty() {
        assert_hits_texts(cap.els.iter().map(|n| n.t.as_str()), asserts)
    } else {
        assert_hits_texts(cap.full.iter().map(|f| f.t.as_str()), asserts)
    }
}

/// 探针应答(v0.6 Step 3): 四个只读查询,不动屏幕不重采,纯函数供单测。
/// inspect=展开子树(命中叶子上溯至有内容容器) find=全量层全屏搜索 get_state=系统状态 history=里程碑展开。
/// 假树/OCR帧没有可信全量层,如实拒答——探针不说谎。
fn probe_answer(a: &ActN, cap: &Cap, runs: &[PageRun], clip: Option<String>, realw: i32, realh: i32) -> String {
    if a.a == "get_state" {
        let ime = match cap.ime { Some(true) => "弹起", Some(false) => "收起", None => "未知" };
        return format!("activity={} 键盘={} 剪贴板={} 前台={}",
            if cap.activity.is_empty() { "未知" } else { &cap.activity }, ime,
            clip.map(|c| format!("\"{}\"", tcut(&c, 60))).unwrap_or_else(|| "空/不可读".into()),
            if cap.pkg.is_empty() { "未知" } else { &cap.pkg });
    }
    if a.a == "history" {
        let q = a.text.as_deref().unwrap_or("").trim();
        let pnum = q.trim_start_matches('P').parse::<i64>().ok();
        // 旧页段里找(当前页本就全量展开不重复);同页多段取最近一段
        let hit = runs.iter().enumerate().rev().skip(1).find(|(_, r)| match pnum {
            Some(id) => r.key.1 == id,
            None => !q.is_empty() && r.label.contains(q),
        });
        return match hit {
            Some((i, r)) => {
                let mut s = format!("[里程碑{}] {} 逐步细节:\n", i + 1, r.label);
                for (al, d) in r.lines.iter().take(15) { s.push_str(&format!("{al} → {d}\n")); }
                if r.lines.len() > 15 { s.push_str(&format!("(另有{}步)", r.lines.len() - 15)); }
                s
            }
            None => {
                let labels: Vec<&str> = runs.iter().rev().skip(1).take(8).map(|r| r.label.as_str()).collect();
                format!("无匹配里程碑'{}';可查: {}", tcut(q, 10),
                    if labels.is_empty() { "无(本局尚未换过页)".into() } else { labels.join(",") })
            }
        };
    }
    // inspect / find 需要可信全量层
    if cap.suspect { return "元素树本步不可信(假树),探针无数据;只按截图判断".into(); }
    if cap.full.is_empty() { return "本屏无UI树(OCR/空树帧),探针无数据".into(); }
    if a.a == "find" {
        let q = a.text.as_deref().unwrap_or("").trim();
        let all: Vec<&FullNode> = cap.full.iter().filter(|f| !f.t.is_empty() && f.t.contains(q)).collect();
        if all.is_empty() {
            return format!("当前屏未找到'{}';可scroll后再找,或目标在别页", tcut(q, 12));
        }
        let hits: Vec<String> = all.iter().take(6).map(|f| {
            let bn = b_norm(f.b, cap, realw, realh);
            format!("\"{}\" [{},{},{},{}]{}", tcut(&f.t, 30), bn[0], bn[1], bn[2], bn[3],
                if f.clickable { "(可点)" } else { "" })
        }).collect();
        return format!("'{}'命中{}处: {}", tcut(q, 12), all.len(), hits.join(" ; "));
    }
    // inspect: 定位目标(id/文字/坐标) → 子树文字与图标行全展开(含折叠内)
    let (kids, _) = crate::fold::tree_of(&cap.full);
    let mut parent = vec![usize::MAX; cap.full.len()];
    for (p, ks) in kids.iter().enumerate() { for &k in ks { parent[k] = p; } }
    let found = if let Some(q) = a.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        cap.full.iter().position(|f| f.id.as_deref().is_some_and(|i| i == q || i.ends_with(q)))
            .or_else(|| cap.full.iter().position(|f| !f.t.is_empty() && f.t.contains(q)))
    } else {
        let dx = (a.x.unwrap_or(0) * realw as i64 / NORM) as i32;
        let dy = (a.y.unwrap_or(0) * realh as i64 / NORM) as i32;
        cap.full.iter().enumerate()
            .filter(|(_, f)| f.b[0] <= dx && dx <= f.b[2] && f.b[1] <= dy && dy <= f.b[3])
            .min_by_key(|(_, f)| (f.b[2] - f.b[0]) as i64 * (f.b[3] - f.b[1]) as i64)
            .map(|(i, _)| i)
    };
    let Some(mut ti) = found else {
        return format!("inspect目标'{}'不在当前屏", tcut(a.text.as_deref().unwrap_or("(该坐标)"), 12));
    };
    // 子树=文档序连续段(i..j while depth>d);命中叶子(折叠头行文字)时上溯至有内容的容器
    let sub_end = |i: usize| {
        let d = cap.full[i].depth;
        let mut j = i + 1;
        while j < cap.full.len() && cap.full[j].depth > d { j += 1; }
        j
    };
    for _ in 0..2 {
        let texts = cap.full[ti..sub_end(ti)].iter().filter(|f| !f.t.is_empty()).count();
        if texts >= 3 || parent[ti] == usize::MAX { break; }
        ti = parent[ti];
    }
    let mut lines: Vec<String> = Vec::new();
    for f in &cap.full[ti..sub_end(ti)] {
        if f.t.is_empty() && !(f.clickable && f.id.is_some()) { continue; }
        let bn = b_norm(f.b, cap, realw, realh);
        let mut marks = String::new();
        if f.clickable { marks.push_str("可点"); }
        if f.checkable { marks.push_str(if f.checked { " 已开" } else { " 已关" }); }
        let name = if f.t.is_empty() { format!("[icon {}]", f.id.as_deref().unwrap_or("?")) }
                   else { tcut(&f.t, 40) };
        lines.push(format!("{name} [{},{},{},{}]{}", bn[0], bn[1], bn[2], bn[3],
            if marks.is_empty() { String::new() } else { format!("({marks})") }));
    }
    let total = lines.len();
    lines.truncate(22);
    let mut s = format!("inspect展开({}条):\n{}", total, lines.join("\n"));
    if total > 22 { s.push_str(&format!("\n(另有{}条)", total - 22)); }
    s
}

/// ② 上下文组装: goal + 通用/任务经验 + 安装清单 + 里程碑+当前页act/diff + ban + note + 小地图 + 前台应用 + 屏幕
/// 静态段(goal/经验/apps)在前,动态段在后,保住能保的提示词缓存前缀。
fn render_ctx(goal: &str, glessons: &[Value], lessons: &[Value], apps_line: &str, budget_line: &str,
              map_line: &str, assert_line: &str, alert: &str, probe_line: &str, runs: &[PageRun], bans: &[Ban],
              note: &str, cap: &Cap, realw: i32, realh: i32) -> String {
    let mut s = format!("goal: {goal}\n");
    if !assert_line.is_empty() {
        s.push_str(&format!("✔ {assert_line}\n"));
    }
    if !alert.is_empty() {
        s.push_str(&format!("⚠ {alert}\n"));
    }
    // 探针应答: 一次性注入(仅本轮可见),要留存的事实模型自己写note
    if !probe_line.is_empty() {
        s.push_str(&format!("probe应答(你上一步的探针查询,仅本轮可见):\n{probe_line}\n"));
    }
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
    // 预算事实(局38教训: 模型被要求管理预算却看不见预算,烧穿也不知道收尾)
    if !budget_line.is_empty() {
        s.push_str(&format!("{budget_line}\n"));
    }
    // 历史: 旧页折成单行里程碑,当前页全量展开(含驳回行)
    for (i, r) in runs.iter().enumerate() {
        if i + 1 == runs.len() {
            if runs.len() > 1 && !r.lines.is_empty() {
                s.push_str(&format!("当前页{}:\n", r.label));
            }
            for (a, d) in &r.lines {
                s.push_str(&format!("{a} → diff: {d}\n"));
            }
        } else if let Some((last, _)) = r.lines.last() {
            s.push_str(&format!(
                "[里程碑{}] {} ×{}步 → {}\n",
                i + 1, r.label, r.lines.len(), tcut(last, 30)
            ));
        }
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
    if cap.webview && !cap.suspect {
        s.push_str("(本页主体为WebView网页: 网页内按钮不在原生清单里,点图标/关闭用what:\"icon:描述\"按截图坐标直点;back可能被网页JS拦截,连续back无效时程序会自动升级为快速连按两次)\n");
    }
    if cap.suspect {
        s.push_str(&format!(
            "(元素列表不可信:树来自{}而前台是{},本步只看截图判断)\n",
            cap.els_pkg, cap.pkg
        ));
    } else if !cap.folded.is_empty() {
        // 折叠投影: 骨架/简单项全展开,复合重复项收成头行,折缝有账(折N条M字)
        for l in cap.folded.iter().take(60) {
            let bi = b_img_space(l.b, cap, realw, realh);
            s.push_str(&format!(
                "{} [{},{},{},{}]\n",
                l.t,
                to_norm(bi[0], cap.w), to_norm(bi[1], cap.h),
                to_norm(bi[2], cap.w), to_norm(bi[3], cap.h)
            ));
        }
        if cap.folded.len() > 60 {
            s.push_str(&format!("(另有{}行未显示)\n", cap.folded.len() - 60));
        }
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

/// 模型回复原文 → (计划acts, note)。纯函数,单测钉契约。
/// note 两种来法都收: 正常的 r=note,以及把便签误当动作发的 r=act,a=note(契约口子,不判死)。
fn parse_plan(text: &str, plan_max: usize) -> (Vec<ActN>, Option<String>) {
    let objs = extract_objs(text);
    let mut acts: Vec<ActN> = Vec::new();
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
        let mut act = ActN {
            a: name,
            x: num(&o["x"]),
            y: num(&o["y"]),
            x2: num(&o["x2"]),
            y2: num(&o["y2"]),
            text: o["text"].as_str().map(String::from),
            what: o["what"].as_str().map(String::from),
        };
        // 探针别名参数: inspect 的 id / history 的 page 都归到 text
        if act.text.is_none() {
            act.text = o["id"].as_str().or_else(|| o["page"].as_str()).map(String::from);
        }
        acts.push(act);
        if is_done || acts.len() >= plan_max { break; }
    }
    // 契约容错(#19, 局35原文实录): 模型判断对了但违约格式——裸"done"与 {"r":"done",...}
    // 两种实测形态收编为合法done(与"便签误当动作"同一先例,白烧过2次调用+40秒);
    // 其余违约仍不收,换人重问的防线不动。
    if acts.is_empty() {
        if let Some(o) = objs.iter().find(|o| o["r"] == "done") {
            acts.push(ActN {
                a: "done".into(),
                text: o["text"].as_str().or_else(|| o["t"].as_str()).map(String::from),
                ..Default::default()
            });
        } else if objs.is_empty() && text.trim().eq_ignore_ascii_case("done") {
            acts.push(ActN { a: "done".into(), text: Some("(裸done,契约容错)".into()), ..Default::default() });
        }
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
        // 探针同理只能收尾: 结果到下一轮决策才可见,排在其后的动作全是盲动
        if let Some(pi) = acts.iter().position(|a| PROBES.contains(&a.a.as_str())) {
            acts.truncate(pi + 1);
        }
    }
    (acts, note)
}

/// 探针动作集(v0.6 Step 3): 只读查询,不动屏幕,不重采画面
const PROBES: [&str; 4] = ["inspect", "find", "get_state", "history"];

/// ③ 决策调用: 返回按序计划(1~plan_max个act,done截断) + note。换人重问最多3家。
/// 每次真实回包原文先落账再解析——解析不动的违约样本恰是最该留档的。
fn plan_call(brain: &mut Brain, cfg: &Config, user: &str, img: &str, log: &mut Log, n: u32)
    -> Result<(Vec<ActN>, Option<String>, String, u64), String>
{
    let sys = cfg.prompts.get("step").map(|s| s.as_str()).unwrap_or("");
    for attempt in 0..3 {
        let out = brain.call(sys, user, &[img], 500, attempt)?;
        log_raw(log, "step", n, &out.by, out.ms, &out.text, None);
        let (acts, note) = parse_plan(&out.text, cfg.plan_max);
        if !acts.is_empty() {
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
fn gate(a: &mut ActN, cap: &Cap, bans: &[Ban], taps: &VecDeque<(i32, i32, String)>, cfg: &Config, tmp: &str,
        apps: &[(String, String)], realw: i32, realh: i32) -> Option<String> {
    let known = ["tap", "swipe", "scroll_up", "scroll_down", "type", "back", "home", "launch", "goto", "wait", "done",
                 "inspect", "find", "get_state", "history"];
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
            } else if a.what.as_deref().map(str::trim).is_some_and(|w| w.starts_with("icon:")) {
                // 纯图标按钮(×/齿轮/返回箭头等无文字控件): 清单和OCR都没有它的文字,
                // 跳过吸附按模型视觉坐标直点;坐标合法性/前科/连点/空白死角检查照常
            } else if let Some(w) = a.what.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
                match find_el(cap, w, x, y, realw, realh) {
                    Some((_, bi)) => {
                        xi = (bi[0] + bi[2]) / 2;
                        yi = (bi[1] + bi[3]) / 2;
                    }
                    None => {
                        return Some(format!(
                            "点名'{}'不在screen清单;附近元素: {}",
                            tcut(w, 12),
                            nearby_els(cap, x, y, realw, realh)
                        ));
                    }
                }
            } else if let Some(bi) = box_at(cap, x, y, realw, realh) {
                xi = (bi[0] + bi[2]) / 2;
                yi = (bi[1] + bi[3]) / 2;
            }
            if let Some(b) = bans.iter().find(|b| (b.x - xi).abs() < b.rad && (b.y - yi).abs() < b.rad) {
                return Some(format!("前科:距ban({},{})不足{}px", to_norm(b.x, cap.w), to_norm(b.y, cap.h), b.rad));
            }
            // 同点连点认身份不认裸坐标(局40教训: 频道栏自动居中,连点"财经→军事→国际"
            // 三个不同按钮物理落点相同且每次真实切页——同点不是罪,同一目标同点三连才是)。
            // 2048抖动/登录振荡/直播页连点仍被拦: 它们要么同名,要么无名同坐标桶。
            let sig_now = match a.what.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
                Some(w) => format!("tap[{}]", tcut(w, 12)),
                None => format!("tap({},{})", xi as i64 / 50, yi as i64 / 50),
            };
            let near = |p: &(i32, i32, String)| (p.0 - xi).abs() < cfg.ban_radius && (p.1 - yi).abs() < cfg.ban_radius;
            let mut rev = taps.iter().rev();
            if let (Some(p1), Some(p2)) = (rev.next(), rev.next()) {
                if near(p1) && near(p2) && p1.2 == sig_now && p2.2 == sig_now {
                    return Some("同一目标同点连点3次:换个目标,或用back/scroll_down离开当前位置".into());
                }
            }
            if let (Some((sd, mean)), Some(bg)) =
                (patch_stats(&cap.img, xi, yi, tmp), cap.thumb.as_ref().map(|t| mode_gray(t) as f32))
            {
                if sd < 6.0 && (mean - bg).abs() < 22.0 {
                    return Some(format!(
                        "空白背景:σ{sd:.1}亮度{mean:.0}≈背景{bg:.0};清单附近元素: {}",
                        nearby_els(cap, x, y, realw, realh)
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
        "inspect" => {
            // 寻址二选一: id/文字(text) 或 坐标。坐标保持0~999屏幕空间,探针不转像素
            if a.text.as_deref().map(str::trim).filter(|t| !t.is_empty()).is_none() {
                match (chk(a.x, "x"), chk(a.y, "y")) {
                    (Ok(x), Ok(y)) => { a.x = Some(x); a.y = Some(y); }
                    (Err(e), _) | (_, Err(e)) => return Some(format!("inspect需text(卡片头行文字或id)或坐标: {e}")),
                }
            }
        }
        "find" | "history" => {
            if a.text.as_deref().unwrap_or("").trim().is_empty() {
                return Some(format!("{}缺text", a.a));
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
        "inspect" => { a.x2 = None; a.y2 = None; } // 坐标寻址时保留(0~999)
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
fn arbit_changed(brain: &mut Brain, cfg: &Config, img_a: &str, img_b: &str, log: &mut Log, n: u32)
    -> Option<(bool, String, String)>
{
    let p = cfg.hook_for("pre_ban").and_then(|h| cfg.prompt_of(h))?;
    let u = "第一张为点击前截图,第二张为点击后截图。".to_string();
    for att in 0..2 {
        if let Ok(out) = brain.call(p, &u, &[img_a, img_b], 150, att) {
            log_raw(log, "arbit", n, &out.by, out.ms, &out.text, None);
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
/// 返回 (差异串, 是否边界区, 变化元素坐标框)。差异串契约不变(喂模型/窗口);
/// 坐标框只进账本(add/del 各至多8条,图片像素空间),像素通道无框(Null)。
fn diff_of(before: &Cap, after: &Cap, channel: &str, realw: i32, realh: i32)
    -> (String, bool, Value)
{
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
        let mut added_keys: Vec<&String> = Vec::new();
        let mut removed_keys: Vec<&String> = Vec::new();
        for (t, ca) in &ma {
            let cb = mb.get(t).unwrap_or(&0);
            if ca > cb { added_keys.push(t); }
        }
        for (t, cb) in &mb {
            let ca = ma.get(t).unwrap_or(&0);
            if cb > ca { removed_keys.push(t); }
        }
        if added_keys.is_empty() && removed_keys.is_empty() {
            // 矛盾检测: 树说"没变"但像素大变(压过背景) → 同应用内树未刷新,改采像素结论,
            // 防止把真实生效的动作记成空击、误落 ban
            if let (Some(a), Some(b)) = (&before.thumb, &after.thumb) {
                let pct = frames_diff_pct(a, b);
                let bg = before.noise.max(after.noise);
                if pct > bg + 13.0 && pct > 10.0 {
                    return (format!("pixel({}%,元素树未更新)", pct.round() as i32), false, Value::Null);
                }
            }
            return ("none".into(), false, Value::Null);
        }
        let mut added: Vec<String> = added_keys.iter().map(|t| tcut(t, 18)).collect();
        let mut removed: Vec<String> = removed_keys.iter().map(|t| tcut(t, 18)).collect();
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
        // 变化框: 每个变化词在对应端清单里的首个同文元素框
        let boxes_of = |keys: &mut Vec<&String>, side: &Cap| -> Vec<Value> {
            keys.sort();
            keys.iter().take(8).filter_map(|t| {
                side.els.iter().find(|n| &n.t == *t)
                    .map(|n| json!([tcut(t, 18), el_img_space(n, side, realw, realh)]))
            }).collect()
        };
        let ab = boxes_of(&mut added_keys, after);
        let db = boxes_of(&mut removed_keys, before);
        let mut bm = serde_json::Map::new();
        if !ab.is_empty() { bm.insert("add".into(), json!(ab)); }
        if !db.is_empty() { bm.insert("del".into(), json!(db)); }
        let boxes = if bm.is_empty() { Value::Null } else { Value::Object(bm) };
        (tcut(&s, 160), false, boxes)
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
                    (s, false, Value::Null)
                } else if pct <= bg + 5.0 {
                    ("none".into(), false, Value::Null) // 未压过背景: 记没变化(空击/ban防线照常工作)
                } else {
                    let s = format!("pixel({}%)>bg({:.0}%)", pct.round() as i32, bg);
                    (s, pct <= bg + 13.0, Value::Null) // 刚过背景一点 → 边界区,交给仲裁
                }
            }
            _ => ("none".into(), false, Value::Null),
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
               endless: bool, budget: u32, app: Option<String>, asserts: Vec<String>) -> EpisodeResult {
    let t0 = std::time::Instant::now();
    let fail = |stop: &str, run_id: String| EpisodeResult {
        achieved: false, run_id, stop: stop.into(), steps: 0, calls: 0, tokens: 0,
        wall_ms: t0.elapsed().as_millis() as u64,
    };
    // ── 目录与文件 ──
    let task_dir = format!("{}/tasks/{}", cfg.data_dir.trim_end_matches('/'), task);
    let run_id = now_tag();
    let run_dir = format!("{}/runs/{run_id}", task_dir);
    if fs::create_dir_all(&run_dir).is_err() {
        eprintln!("✗ 建不了运行目录 {run_dir}");
        return fail("mkdir_fail", String::new());
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

    // 模型视角存档: 本局所用的完整提示词与规则 + 每次决策看到的全部材料,精准落 run_dir/ctx.log
    // (此前写tmp、局末即删,无法复盘"模型当时到底看到了什么")
    let ctx_path = format!("{run_dir}/ctx.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&ctx_path) {
        let _ = writeln!(f, "== 本局提示词与规则([prompts] 全文,顺序无关) ==");
        let mut keys: Vec<&String> = cfg.prompts.keys().collect();
        keys.sort();
        for k in keys {
            let _ = writeln!(f, "\n--[{k}]--\n{}", cfg.prompts[k]);
        }
        let _ = writeln!(f, "\n== 阈值 ==\nsettle_ms={} plan_max={} els_timeout_ms={} ban_radius={} ban_strikes={} stall_limit={}",
            cfg.settle_ms, cfg.plan_max, cfg.els_timeout_ms, cfg.ban_radius, cfg.ban_strikes, cfg.stall_limit);
    }

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
            let act = if kill { "force-stop+home+launch" } else { "home+launch" };
            println!("(开局归位: 前台{fg} ≠ 目标{target},{act})");
            if kill { phone.force_stop(&fg); }
            phone.home();
            std::thread::sleep(std::time::Duration::from_millis(900));
            // 冷启动目标应用(整合round.sh轮间清理语义: 统一从主Activity起)
            if let Some((_, comp)) = apps.iter().find(|(p, _)| p == target) {
                phone.launch(target, comp);
                std::thread::sleep(std::time::Duration::from_millis(900));
            }
            log.put(json!({"r":"hook","kind":"orient","from":fg,"target":target,"act":act}));
        }
    }

    // ── 局内状态 ──
    let mut runs: Vec<PageRun> = Vec::new(); // 里程碑折叠: 按页分段的(act行, diff)全量历史
    let mut ctx_bytes: u64 = 0; // 决策上下文体积账(投影层验收指标: 字节/步不随步数涨)
    let mut ctx_calls: u32 = 0;
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
    let mut revived = false; // 设备复活每局一次(采集失败→重启adb/模拟器)
    let mut visited: std::collections::HashSet<i64> = Default::default(); // 沙盘: 本局到访页
    let mut act_sigs: VecDeque<String> = VecDeque::new(); // 打摆: 已执行动作签名
    let mut osc_fire_at = 4usize;   // 打摆: 签名窗达此长度才(再)触发
    let mut alert = String::new();  // 打摆警报(注入下一次决策,用过即清)
    let mut assert_hit_prev = false; // 验收词命中上升沿(只在新命中时记账,避免每步刷屏)
    let mut probe_streak = 0u32;    // 探针连击计数(上限3防空转,任一物理动作清零)
    let mut probe_ans = String::new(); // 探针应答(注入下一次决策,用过即清)

    // ── 计划队列 ──
    let mut queue: VecDeque<ActN> = VecDeque::new();
    let mut taps_hist: VecDeque<(i32, i32, String)> = VecDeque::new(); // 已执行的连续tap(落点图片像素+动作签名)
    let mut back_eaten = false; // WebView页back无效(JS拦截)标记 → 下次back升级连按两次
    let mut plan_by = String::new();
    let mut plan_ms: Option<u64> = None; // 只随计划首动作入日志
    let mut pending_note: Option<String> = None;

    let Some(mut cap) = capture_or_revive(&phone, &run_dir, capseq, cfg, &tmp, settle_ms, realw, realh, app.as_deref(), &mut revived) else {
        eprintln!("✗ 首屏采集失败(设备复活无效)");
        return fail("capture_fail", run_id.clone());
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
        // 沙盘: 到访页登记(每轮顶部必经,覆盖goto跳段/驳回重采/盲移等一切路径)
        if let Some(t) = tree.as_ref() {
            if let Some(p) = t.page_of(&cap.els) { visited.insert(p.id); }
        }
        // 里程碑: 换页判定与页段维护((activity,身份证)合取,未知分量不触发)
        {
            let (pid, pname) = tree.as_ref().and_then(|t| t.page_of(&cap.els))
                .map(|p| (p.id, p.name.clone())).unwrap_or((-1, String::new()));
            let cur_key = (cap.activity.clone(), pid);
            match runs.last_mut() {
                Some(r) if !page_boundary(&r.key, &cur_key) => {
                    // 同段内补全先前未知的分量(首帧activity采失败等),顺带修正展示名
                    if (r.key.0.is_empty() && !cur_key.0.is_empty()) || (r.key.1 < 0 && pid >= 0) {
                        if r.key.0.is_empty() { r.key.0 = cur_key.0.clone(); }
                        if r.key.1 < 0 { r.key.1 = pid; }
                        r.label = run_label(&r.key.0, r.key.1, &pname);
                    }
                }
                _ => runs.push(PageRun {
                    label: run_label(&cap.activity, pid, &pname),
                    key: cur_key, lines: vec![],
                }),
            }
        }

        // 契约式到达断言: 验收词全部在当前屏实测到 → 注入事实行(假树不测,防止拿旧屏谎报命中)
        let hits = if cap.suspect { Vec::new() } else { assert_hits(&cap, &asserts) };
        let assert_line = if asserts.is_empty() || hits.len() < asserts.len() {
            assert_hit_prev = false;
            String::new()
        } else {
            if !assert_hit_prev {
                assert_hit_prev = true;
                log.put(json!({"r":"hook","kind":"assert","hit":hits}));
                println!("      ✔ 验收词实测命中: [{}]", hits.join(","));
            }
            format!("验收条件已满足:程序在当前屏实测到任务验收词 [{}] —— 目标画面已到达,核对无遗漏后即可done,不必寻找与任务字面一致的标题",
                hits.join(","))
        };

        // ②③ 队列空则组装上下文并要一份计划
        if queue.is_empty() {
            // 小地图/探索沙盘: 遍历类任务给沙盘看板(进度+本页未探索+建议目标),普通任务单行小地图
            let map_line = match tree.as_ref().and_then(|t| t.page_of(&cap.els)) {
                Some(p) => {
                    let t = tree.as_ref().unwrap();
                    let traversal = ["遍历", "全部", "覆盖", "逛"].iter().any(|k| goal.contains(k));
                    if traversal {
                        // WebView活动页的H5文字不作探索分支: 位置漂、每秒变、原生树锚不住
                        let un = if cap.webview {
                            vec!["(WebView活动页,网页内容不作探索分支,看毕即撤)".to_string()]
                        } else {
                            t.unexplored(p.id, &sandbox_texts(&cap))
                        };
                        let nxt = t.nearest_unvisited(p.id, &visited);
                        let mut s = format!("[探索沙盘] 已覆盖 {}/{} 页 | 当前P{}[{}]",
                            visited.len(), t.pages.len(), p.id, tcut(&p.name, 12));
                        s.push_str(&format!(" | 本页未探索: [{}]",
                            if un.is_empty() { "无".to_string() } else { un.join(",") }));
                        if let Some((nid, h)) = nxt {
                            let nname = t.page(nid).map(|q| tcut(&q.name, 10)).unwrap_or_default();
                            s.push_str(&format!(" | 建议最近未访: goto P{nid}[{nname}]({h}跳)"));
                        }
                        if let Some(m) = t.map_line(p.id) {
                            if let Some(i) = m.find(" 熟路: ") {
                                s.push_str(&m[i..]); // 熟路明细保留,模型可自行导航
                            }
                        }
                        println!("      🗺 {s}");
                        s
                    } else {
                        t.map_line(p.id).unwrap_or_default()
                    }
                }
                None => String::new(),
            };
            let budget_line = if endless {
                format!("预算: 已用{}/{budget}次模型调用", brain.calls)
            } else {
                format!("进度: 第{n}/{}步", cfg.max_steps)
            };
            let user = render_ctx(goal, &glessons, &lessons, &apps_line, &budget_line, &map_line, &assert_line, &alert, &probe_ans, &runs, &bans, &note, &cap, realw, realh);
            ctx_bytes += user.len() as u64;
            ctx_calls += 1;
            // 模型视角存档: 这次决策发给模型的完整上下文(含警报/沙盘/清单/便签)
            let _ = fs::OpenOptions::new().create(true).append(true)
                .open(&ctx_path)
                .map(|mut f| writeln!(f, "\n══ 步#{n} 决策上下文 ══\n{user}\n"));
            match plan_call(&mut brain, cfg, &user, &cap.img, &mut log, n) {
                Ok((acts, note_new, by, ms)) => {
                    if acts.len() > 1 { println!("      📋 {}给出{}步计划", by, acts.len()); }
                    queue = acts.into();
                    plan_by = by;
                    plan_ms = Some(ms);
                    pending_note = note_new;
                    alert.clear(); // 警报已送达本次决策,清掉避免重复轰炸
                    probe_ans.clear(); // 探针应答同为一次性投递
                }
                Err(e) if e == "内容过滤" && escapes_left > 0 => {
                    // 画面被安全审核拒绝(确定性): 不等模型,盲滑一步离开,重新采集再决策
                    escapes_left -= 1;
                    println!("      🚧 画面被内容过滤拒绝,盲移离开(余{escapes_left}次)");
                    let esc = ActN { a: "scroll_down".into(), ..Default::default() };
                    exec(&phone, &esc, &cap, realw, realh, &apps);
                    if osc_note(&mut act_sigs, act_sig(&esc), &mut osc_fire_at) {
                        alert = OSCILL_WARN.to_string();
                        log.put(json!({"r":"hook","kind":"oscill","at":"escape"}));
                        println!("      ⚠ 打摆检测命中,警报已注入");
                    }
                    log_act(&mut log, n, &esc, "(盲移)", None);
                    let d = "escape(内容过滤)".to_string();
                    log.put(json!({"r":"diff","n":n,"d":d}));
                    println!("[{n}] ·盲移 | scroll_down → {d}");
                    run_push(&mut runs, format!("act#{n} scroll_down(盲移)"), d.clone());
                    diffs_all.push(d.clone());
                    stream.push(format!("act#{n} scroll_down → escape(内容过滤)"));
                    stall += 1;
                    if stall >= cfg.stall_limit { stop_reason = Some("watchdog"); break; }
                    capseq += 1;
                    match capture_or_revive(&phone, &run_dir, capseq, cfg, &tmp, settle_ms, realw, realh, app.as_deref(), &mut revived) {
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
            run_push(&mut runs, aline.clone(), d.clone());
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
            match capture_or_revive(&phone, &run_dir, capseq, cfg, &tmp, settle_ms, realw, realh, app.as_deref(), &mut revived) {
                Some(c) => cap = c,
                None => { stop_reason = Some("capture_fail"); break; }
            }
            continue; // 回到第①步
        }
        reject_streak = 0; // 过门即清零

        // 🔍 探针(v0.6 Step 3): 只读查询,不动屏幕不重采,答案注入下一轮决策。
        // 连续上限3次防空转——探针替代的是乱撞,不是行动;探针签名照进打摆账。
        if PROBES.contains(&act.a.as_str()) {
            probe_streak += 1;
            if probe_streak > 3 {
                log_act(&mut log, n, &act, &plan_by, ms_field);
                let d = "rejected(连续探针达3次上限:先基于已获信息行动)".to_string();
                log.put(json!({"r":"diff","n":n,"d":d}));
                println!("[{n}] {ms_disp} {plan_by} | {aline} ⛔ {d}");
                run_push(&mut runs, aline.clone(), d.clone());
                diffs_all.push(d.clone());
                stream.push(format!("{aline} → {d}"));
                stall += 1;
                if stall >= cfg.stall_limit { stop_reason = Some("watchdog"); break; }
                continue;
            }
            let clip = if act.a == "get_state" { phone.clipboard() } else { None };
            let ans = tcut(&probe_answer(&act, &cap, &runs, clip, realw, realh), 1200);
            log_act(&mut log, n, &act, &plan_by, ms_field);
            log.put(json!({"r":"probe","n":n,"a":act.a,"q":act.text,"ans":ans}));
            println!("[{n}] {ms_disp} {plan_by} | {aline} → probe✓({}字)", ans.chars().count());
            run_push(&mut runs, aline.clone(), format!("probe✓({})", tcut(&ans.replace('\n', " "), 24)));
            diffs_all.push(format!("probe({})", act.a));
            stream.push(format!("{aline} → probe: {}", tcut(&ans.replace('\n', " "), 80)));
            if osc_note(&mut act_sigs, act_sig(&act), &mut osc_fire_at) {
                alert = OSCILL_WARN.to_string();
                log.put(json!({"r":"hook","kind":"oscill","at":"probe"}));
                println!("      ⚠ 打摆检测命中(探针),警报已注入");
            }
            probe_ans = format!("{aline} → {ans}");
            if let Some(t) = pending_note.take() {
                let t = tcut(&t, cfg.note_max_chars);
                log.put(json!({"r":"note","t":t}));
                note = t;
            }
            continue; // 画面没动: 同一cap直接进下一轮,不重采
        }

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
                run_push(&mut runs, aline.clone(), d.clone());
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
            probe_streak = 0; // 物理导航,探针连击清零
            let mut ms_left = ms_field; // goto那次模型调用的耗时记在首段上
            let mut walked_all = true;
            for hop in &hops {
                log_screen(&mut log, &cap, n, realw, realh, &mut logged_seq);
                let Some(hact) = act_from_edge(hop, &cap, realw, realh) else {
                    println!("      🧭 熟路断了: 当前清单里找不到 {}", hop.label);
                    let d = format!("树✗(找不到按钮{})", hop.label);
                    log.put(json!({"r":"diff","n":n,"d":d}));
                    println!("[{n}] ·熟路 | goto断 → {d}");
                    run_push(&mut runs, format!("act#{n} goto·断"), d.clone());
                    diffs_all.push(d.clone());
                    stream.push(format!("act#{n} goto断({}) → {d}", hop.label));
                    walked_all = false;
                    break;
                };
                let aline_h = act_line(n, &hact);
                let ms_h = ms_left.take();
                exec(&phone, &hact, &cap, realw, realh, &apps);
                if osc_note(&mut act_sigs, act_sig(&hact), &mut osc_fire_at) {
                    alert = OSCILL_WARN.to_string();
                    log.put(json!({"r":"hook","kind":"oscill","at":"goto"}));
                    println!("      ⚠ 打摆检测命中(goto段),警报已注入");
                }
                if hact.a == "tap" {
                    taps_hist.push_back((hact.x.unwrap_or(0) as i32, hact.y.unwrap_or(0) as i32, act_sig(&hact)));
                    while taps_hist.len() > 4 { taps_hist.pop_front(); }
                } else {
                    taps_hist.clear();
                }
                exec_steps += 1;
                capseq += 1;
                // 跳段用短定格: 到达判定靠页面身份证,不必等画面完全安静
                let Some(nc) = capture_or_revive(&phone, &run_dir, capseq, cfg, &tmp, 1200, realw, realh, app.as_deref(), &mut revived) else {
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
                run_push(&mut runs, aline_h.clone(), d.clone());
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
        // WebView返回死锁破解: 上一记back在网页页无效(前端JS拦截单次back),本次back升级为快速连按两次
        if act.a == "back" && back_eaten {
            println!("      🕸 WebView页back曾被吞,升级为快速连按两次");
            log.put(json!({"r":"hook","kind":"webview_back2","n":n}));
            phone.back();
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        exec(&phone, &act, &cap, realw, realh, &apps);
        probe_streak = 0; // 物理动作落地,探针连击清零
        if act.a == "tap" {
            taps_hist.push_back((act.x.unwrap_or(0) as i32, act.y.unwrap_or(0) as i32, act_sig(&act)));
            while taps_hist.len() > 4 { taps_hist.pop_front(); }
        } else {
            taps_hist.clear(); // 连点只算"连续tap",夹其他动作即重新数
        }
        exec_steps += 1;
        // 打摆记账: 已执行动作签名;进出循环(X,Y,X,Y且含back/home)→注入警报
        if osc_note(&mut act_sigs, act_sig(&act), &mut osc_fire_at) {
            alert = OSCILL_WARN.to_string();
            log.put(json!({"r":"hook","kind":"oscill",
                "pair": format!("{},{}", act_sigs[act_sigs.len() - 2], act_sigs[act_sigs.len() - 1])}));
            println!("      ⚠ 打摆检测: {}↔{} 进出循环,警报已注入(严禁back,读正文或done)",
                act_sigs[act_sigs.len() - 2], act_sigs[act_sigs.len() - 1]);
        }

        // ⑥ 等画面安静后定帧 + 实测差异(与拉元素列表并行)
        capseq += 1;
        let Some(newcap) = capture_or_revive(&phone, &run_dir, capseq, cfg, &tmp, settle_ms, realw, realh, app.as_deref(), &mut revived) else {
            stop_reason = Some("capture_fail");
            break;
        };
        if newcap.noise > 0.0 { dyn_steps += 1; }
        let (d_raw, border, dboxes) = diff_of(&cap, &newcap, &channel, realw, realh);
        let d = if border {
            // 边界区: 差异刚压过背景一点,问一次仲裁(限额内)
            if arbit_left > 0 {
                arbit_left -= 1;
                match arbit_changed(&mut brain, cfg, &cap.img, &newcap.img, &mut log, n) {
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
        let mut drec = json!({"r":"diff","n":n,"d":d});
        if !dboxes.is_null() { drec["boxes"] = dboxes; } // 变化元素坐标框只进账本,不进上下文
        log.put(drec);
        println!("[{n}] {ms_disp} {plan_by} | {aline} → {d}");
        if let Some(t) = pending_note.take() {
            let t = tcut(&t, cfg.note_max_chars);
            log.put(json!({"r":"note","t":t}));
            note = t;
        }
        run_push(&mut runs, aline.clone(), d.clone());
        diffs_all.push(d.clone());
        stream.push(format!("{aline} → {d}"));

        let is_null = d == "none";
        // WebView back被吞检测: 网页页上back无效 → 立旗,下一记back自动连按两次
        back_eaten = act.a == "back" && is_null && cap.webview;
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
                    match arbit_changed(&mut brain, cfg, &cap.img, &newcap.img, &mut log, n) {
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
                                log_raw(&mut log, "arbit", n, &out.by, out.ms, &out.text, None);
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
            // 契约式到达断言也交给复核: 验收词+程序对终局的实测结果,契约语义=全部命中即可判达成
            if !asserts.is_empty() {
                u.push_str(&format!("任务契约验收词: [{}]", asserts.join("、")));
                if cap.suspect {
                    u.push_str("(元素列表不可信,请按最终截图自行核对验收词)\n");
                } else {
                    let final_hits = assert_hits(&cap, &asserts);
                    u.push_str(&format!(" | 程序在最终画面实测: {}\n", if final_hits.len() == asserts.len() {
                        format!("全部命中 —— 契约语义:命中即可判达成")
                    } else {
                        format!("未全部命中(仅[{}])", final_hits.join(","))
                    }));
                }
            }
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
                    // 复核的输入材料别处没有存,连同原文一并落账
                    log_raw(&mut log, "verdict", n, &out.by, out.ms, &out.text, Some(&u));
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
                    log_raw(&mut log, "reflect", n, &out.by, out.ms, &out.text, Some(&u));
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
    // 上下文体积账(投影层验收指标): 决策上下文的均值字节入账,campaign 间可比
    if ctx_calls > 0 {
        log.put(json!({"r":"hook","kind":"ctx_stat","bytes":ctx_bytes,"calls":ctx_calls,
                       "avg":ctx_bytes / ctx_calls as u64}));
    }
    println!("{}", "=".repeat(56));
    println!("局{ep_no}结束 | 步数{n} 执行{exec_steps} 空击{null_steps} 动态屏{dyn_steps} ban{ban_count} | 调用{}次 tokens{} ctx均{}B | done={done_claim} achieved={achieved}",
        brain.calls, brain.tokens, if ctx_calls > 0 { ctx_bytes / ctx_calls as u64 } else { 0 });
    println!("记录: {run_dir}/log.jsonl");
    EpisodeResult {
        achieved,
        run_id,
        stop: stop_reason.unwrap_or("done").to_string(),
        steps: n,
        calls: brain.calls,
        tokens: brain.tokens,
        wall_ms: t0.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        toml::from_str(&std::fs::read_to_string("../phonefarm.toml").unwrap()).unwrap()
    }

    /// 空清单、图片路径不存在的最小Cap: patch_stats拿不到图会跳过空白检查,正好单测门控分流
    fn cap() -> Cap {
        Cap {
            seq: 1, els: vec![], full: vec![], folded: vec![], img: "/nonexistent.jpg".into(),
            img_rel: String::new(), xml_rel: String::new(),
            w: 672, h: 1456, thumb: None, noise: 0.0,
            pkg: "t.app".into(), activity: String::new(), ime: None,
            els_pkg: "t.app".into(), suspect: false, ocr: false, webview: false,
        }
    }

    #[test]
    fn icon_what_passes_gate_without_list() {
        // 纯图标: 空清单也能放行,不被"点名不在清单"驳回
        let mut a = ActN {
            a: "tap".into(), x: Some(950), y: Some(40),
            what: Some("icon:close".into()), ..Default::default()
        };
        let r = gate(&mut a, &cap(), &[], &VecDeque::new(), &cfg(), "/tmp/pf-test", &[], 1080, 2340);
        assert!(r.is_none(), "icon: 应放行, 却被驳回: {r:?}");
        assert!(a.x.is_some() && a.y.is_some(), "坐标应已换算成图片像素");
    }

    #[test]
    fn text_what_off_list_rejected() {
        // 有文字点名但清单为空 → 驳回(与icon分流相反的方向)
        let mut a = ActN {
            a: "tap".into(), x: Some(500), y: Some(500),
            what: Some("设置".into()), ..Default::default()
        };
        let r = gate(&mut a, &cap(), &[], &VecDeque::new(), &cfg(), "/tmp/pf-test", &[], 1080, 2340);
        assert!(r.is_some(), "文字点名不在清单应驳回");
    }

    #[test]
    fn oscillation_detected_on_tap_back_loop() {
        let mut sigs: VecDeque<String> = VecDeque::new();
        let mut fire_at = 4usize;
        // 广告设置打摆: tap[广告设置]→back→tap[广告设置]→back,第2轮回合应触发
        assert!(!osc_note(&mut sigs, "tap[广告设置]".into(), &mut fire_at));
        assert!(!osc_note(&mut sigs, "back".into(), &mut fire_at));
        assert!(!osc_note(&mut sigs, "tap[广告设置]".into(), &mut fire_at));
        assert!(osc_note(&mut sigs, "back".into(), &mut fire_at), "第2轮进出应触发打摆警报");
        // 触发后立即再判不重复报(fire_at抬到+2)
        assert!(!osc_note(&mut sigs, "tap[广告设置]".into(), &mut fire_at));
    }

    #[test]
    fn scroll_alternation_not_flagged() {
        // 上滑下滑交替是合法探索,不报警
        let sigs: VecDeque<String> = ["scroll_up", "scroll_down", "scroll_up", "scroll_down"]
            .iter().map(|s| s.to_string()).collect();
        assert!(!is_oscillating(&sigs));
    }

    #[test]
    fn three_beat_loop_flagged() {
        // 警报后的变体: tap→scroll(读一会儿)→back 三拍×2,照样是进出循环
        let seq = ["tap[广告设置]", "scroll_down", "back", "tap[广告设置]", "scroll_down", "back"];
        let sigs: VecDeque<String> = seq.iter().map(|s| s.to_string()).collect();
        assert!(is_oscillating(&sigs));
        // 三拍里换过按钮(读别的页)不算
        let seq2 = ["tap[广告设置]", "scroll_down", "back", "tap[个性化推荐设置]", "scroll_down", "back"];
        let sigs2: VecDeque<String> = seq2.iter().map(|s| s.to_string()).collect();
        assert!(!is_oscillating(&sigs2));
    }

    #[test]
    fn tap_sig_ignores_coord_jitter() {
        // 坐标漂移(150→160)不改变签名: tap认按钮身份
        let a = ActN { a: "tap".into(), x: Some(150), y: Some(920), what: Some("广告设置".into()), ..Default::default() };
        let b = ActN { a: "tap".into(), x: Some(160), y: Some(928), what: Some("广告设置".into()), ..Default::default() };
        assert_eq!(act_sig(&a), act_sig(&b));
    }

    fn nd(t: &str) -> Node { Node { t: t.into(), b: [0, 0, 0, 0] } }

    fn fnode(t: &str) -> FullNode {
        FullNode {
            t: t.into(), b: [0, 0, 1, 1], id: None, class: String::new(),
            clickable: false, scrollable: false, checkable: false, checked: false, depth: 1,
        }
    }

    #[test]
    fn assert_word_matched_by_substring_within_one_element() {
        // 局31场景: 页内元素是"个性化广告推荐"这类长文,验收词"个性化广告"子串命中即算
        let ws = vec!["个性化广告".to_string()];
        assert_eq!(assert_hits_texts(["个性化广告推荐", "清理缓存"].iter().copied(), &ws),
                   vec!["个性化广告".to_string()]);
    }

    #[test]
    fn assert_requires_all_words_present() {
        // 多个验收词必须全中,缺一不注入(合取=更严,防单词撞车假阳性)
        let ws = vec!["个性化广告".to_string(), "程序化广告".to_string()];
        assert_eq!(assert_hits_texts(["个性化广告推荐"].iter().copied(), &ws).len(), 1);
        assert_eq!(assert_hits_texts(["个性化广告", "程序化广告设置"].iter().copied(), &ws).len(), 2);
    }

    #[test]
    fn assert_words_not_stitched_across_elements() {
        // "广告"和"设置"分居两处,不得拼接成"广告设置"算命中
        assert!(assert_hits_texts(["广告", "设置"].iter().copied(),
                                  &vec!["广告设置".to_string()]).is_empty());
    }

    #[test]
    fn plan_tolerates_contract_violating_done() {
        // #19(局35原文实录): 裸"done"与 {"r":"done"} 是模型判断正确但违约格式的两种真实形态
        let (a1, _) = parse_plan("done", 4);
        assert_eq!((a1.len(), a1[0].a.as_str()), (1, "done"), "裸done收编");
        let (a2, _) = parse_plan(r#"{"r":"done","text":"验收词已实测命中"}"#, 4);
        assert_eq!((a2.len(), a2[0].a.as_str()), (1, "done"));
        assert_eq!(a2[0].text.as_deref(), Some("验收词已实测命中"), "理由字段一并收");
        // 防线不松: 其他胡言仍不收;正常契约不受影响
        assert!(parse_plan("我觉得任务完成了", 4).0.is_empty());
        let (a3, _) = parse_plan(r#"{"r":"act","a":"tap","x":1,"y":2,"what":"设置"}"#, 4);
        assert_eq!(a3[0].a, "tap");
    }

    #[test]
    fn plan_done_single_and_goto_tail_preserved() {
        // 既有截断契约在纯函数里原样保留: done单发、goto断尾
        let (a, _) = parse_plan("{\"r\":\"act\",\"a\":\"back\"}\n{\"r\":\"act\",\"a\":\"done\"}", 4);
        assert_eq!((a.len(), a[0].a.as_str()), (1, "back"), "done被截到重看画面之后");
        let (g, _) = parse_plan("{\"r\":\"act\",\"a\":\"goto\",\"text\":\"P5\"}\n{\"r\":\"act\",\"a\":\"tap\",\"x\":1,\"y\":2}", 4);
        assert_eq!((g.len(), g[0].a.as_str()), (1, "goto"), "goto后的计划作废");
    }

    #[test]
    fn same_point_different_targets_not_rejected() {
        // 局40: 频道栏自动居中,不同按钮先后占据同一物理落点且每次真实切页——同点不同身份放行
        let mut hist: VecDeque<(i32, i32, String)> = VecDeque::new();
        hist.push_back((370, 211, "tap[财经]".into()));
        hist.push_back((371, 211, "tap[军事]".into()));
        let mut a = ActN { a: "tap".into(), x: Some(550), y: Some(145),
                           what: Some("icon:国际".into()), ..Default::default() };
        let r = gate(&mut a, &cap(), &[], &hist, &cfg(), "/tmp/pf-test", &[], 1080, 2340);
        assert!(r.is_none(), "同点但不同身份应放行: {r:?}");
    }

    #[test]
    fn same_point_same_target_third_tap_rejected() {
        // 同一身份三连同点仍拦(2048抖动/登录振荡的原保护不丢)
        let mut hist: VecDeque<(i32, i32, String)> = VecDeque::new();
        hist.push_back((369, 210, "tap[icon:刷新]".into()));
        hist.push_back((370, 212, "tap[icon:刷新]".into()));
        let mut a = ActN { a: "tap".into(), x: Some(550), y: Some(145),
                           what: Some("icon:刷新".into()), ..Default::default() };
        let r = gate(&mut a, &cap(), &[], &hist, &cfg(), "/tmp/pf-test", &[], 1080, 2340);
        assert!(r.as_deref().is_some_and(|m| m.contains("同一目标")), "同身份三连同点应驳回: {r:?}");
    }

    #[test]
    fn page_boundary_conjunction_of_known_components() {
        let k = |a: &str, p: i64| (a.to_string(), p);
        // 已知分量变化即翻页: activity 变 / 身份证变
        assert!(page_boundary(&k("com.a.Main", 0), &k("com.a.Settings", 0)));
        assert!(page_boundary(&k("com.a.Main", 0), &k("com.a.Main", 3)), "单Activity内标签切换靠身份证");
        // 未知分量不触发(dumpsys偶发失败/页不在网中 → 不造假里程碑)
        assert!(!page_boundary(&k("com.a.Main", 0), &k("", 0)));
        assert!(!page_boundary(&k("", -1), &k("com.a.Main", 2)));
        assert!(!page_boundary(&k("com.a.Main", -1), &k("com.a.Main", 5)));
        assert!(!page_boundary(&k("com.a.Main", 5), &k("com.a.Main", 5)));
    }

    #[test]
    fn find_el_reaches_beyond_els_truncation() {
        // 折叠渲染出的文字必须能吸附: 全量层锚点无70条/40字盲区
        let mut c = cap();
        let long = format!("{}广告设置入口", "占".repeat(45));
        c.full = vec![fnode(&long)];
        c.els = vec![]; // els 层看不见它
        let hit = find_el(&c, "广告设置入口", 500, 500, 1080, 2340);
        assert!(hit.is_some(), "40字截断之外的文字应可点名吸附");
        assert_eq!(hit.unwrap().0, long.trim());
    }

    #[test]
    fn plan_probe_alias_and_tail_truncation() {
        // 探针别名参数(id/page→text);探针后的计划作废(答案下一轮才可见)
        let (a, _) = parse_plan(r#"{"r":"act","a":"inspect","id":"news_list"}"#, 4);
        assert_eq!((a.len(), a[0].a.as_str(), a[0].text.as_deref()), (1, "inspect", Some("news_list")));
        let (b, _) = parse_plan(
            "{\"r\":\"act\",\"a\":\"find\",\"text\":\"注销\"}\n{\"r\":\"act\",\"a\":\"tap\",\"x\":1,\"y\":2}", 4);
        assert_eq!((b.len(), b[0].a.as_str()), (1, "find"), "探针后计划作废");
        let (c, _) = parse_plan(r#"{"r":"act","a":"history","page":"P2"}"#, 4);
        assert_eq!(c[0].text.as_deref(), Some("P2"));
    }

    #[test]
    fn probe_gate_validation() {
        // get_state 无参放行;inspect 无寻址驳回;探针坐标保持0~999不转像素
        let mut ok = ActN { a: "get_state".into(), ..Default::default() };
        assert!(gate(&mut ok, &cap(), &[], &VecDeque::new(), &cfg(), "/tmp/pf-test", &[], 1080, 2340).is_none());
        let mut bad = ActN { a: "inspect".into(), ..Default::default() };
        assert!(gate(&mut bad, &cap(), &[], &VecDeque::new(), &cfg(), "/tmp/pf-test", &[], 1080, 2340).is_some());
        let mut co = ActN { a: "inspect".into(), x: Some(500), y: Some(300), ..Default::default() };
        assert!(gate(&mut co, &cap(), &[], &VecDeque::new(), &cfg(), "/tmp/pf-test", &[], 1080, 2340).is_none());
        assert_eq!((co.x, co.y), (Some(500), Some(300)));
    }

    #[test]
    fn probe_inspect_expands_folded_card() {
        // 折叠头行文字寻址 → 上溯容器 → 折缝内文字与图标全展开;id寻址同样可用
        let xml = r#"<?xml version='1.0'?><hierarchy>
<node text="" class="c.Wrap" package="t.app" bounds="[0,100][1080,600]">
<node text="标题大文字" class="c.T" package="t.app" bounds="[10,110][1000,200]"/>
<node text="小雅" class="c.T" package="t.app" bounds="[10,210][200,260]"/>
<node text="328赞" class="c.T" package="t.app" bounds="[210,210][400,260]"/>
<node text="" resource-id="t.app:id/more" class="c.I" package="t.app" bounds="[900,210][1000,260]" clickable="true"/>
</node></hierarchy>"#;
        let (els, full, _) = crate::device::parse_dump(xml);
        let mut c = cap();
        c.els = els; c.full = full;
        let a = ActN { a: "inspect".into(), text: Some("标题大文字".into()), ..Default::default() };
        let ans = probe_answer(&a, &c, &[], None, 1080, 2340);
        for w in ["小雅", "328赞", "icon id/more"] {
            assert!(ans.contains(w), "inspect应展开'{w}': {ans}");
        }
        let b = ActN { a: "inspect".into(), text: Some("id/more".into()), ..Default::default() };
        assert!(probe_answer(&b, &c, &[], None, 1080, 2340).contains("小雅"), "id寻址(叶子)也应上溯展开");
    }

    #[test]
    fn probe_find_beyond_els_and_history() {
        // find 在全量层搜索: 40字截断之外照样命中;history 按页号展开旧里程碑
        let long = format!("{}账号注销入口", "占".repeat(45));
        let mut c = cap();
        c.full = vec![fnode(&long)];
        let a = ActN { a: "find".into(), text: Some("注销".into()), ..Default::default() };
        assert!(probe_answer(&a, &c, &[], None, 1080, 2340).contains("命中1处"));
        let miss = ActN { a: "find".into(), text: Some("乌有词".into()), ..Default::default() };
        assert!(probe_answer(&miss, &c, &[], None, 1080, 2340).contains("未找到"));
        let runs = vec![
            PageRun { key: ("com.a.Main".into(), 2), label: "P2[设置]".into(),
                      lines: vec![("act#3 tap(1,2)[设置]".into(), "+[通用]".into())] },
            PageRun { key: ("com.a.Main".into(), 5), label: "P5[通用]".into(), lines: vec![] },
        ];
        let h = ActN { a: "history".into(), text: Some("P2".into()), ..Default::default() };
        let ans = probe_answer(&h, &cap(), &runs, None, 1080, 2340);
        assert!(ans.contains("act#3") && ans.contains("+[通用]"), "{ans}");
        let h9 = ActN { a: "history".into(), text: Some("P9".into()), ..Default::default() };
        assert!(probe_answer(&h9, &cap(), &runs, None, 1080, 2340).contains("无匹配"));
    }

    #[test]
    fn assert_reads_full_layer_beyond_els_truncation() {
        // 验收词落在长文第40字之后: 文字层被截断看不见,全量层实测命中(#17盲区根治)
        let long = format!("{}广告设置", "占".repeat(45));
        let mut c = cap();
        c.els = vec![nd(&long.chars().take(40).collect::<String>())];
        c.full = vec![fnode(&long)];
        let ws = vec!["广告设置".to_string()];
        assert_eq!(assert_hits(&c, &ws), vec!["广告设置".to_string()], "全量层无截断盲区");
        c.full.clear();
        assert!(assert_hits(&c, &ws).is_empty(), "无全量层(OCR帧)退回文字清单,如实测不到就不注入");
    }
}
