//! Device 层: adb 封装(截屏/元素列表/手势) + 像素工具。临时文件一律进 tmp 目录。
use quick_xml::events::Event;
use quick_xml::Reader;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Node {
    pub t: String,
    pub b: [i32; 4],
}

/// 全属性节点(账本层): 无文字的容器/纯图标节点也在内,文字不截断。
/// 只喂账本/断言(将来折叠与探针也吃它),不进任何既有消费方——els 契约原样保留。
#[derive(Debug, Clone)]
pub struct FullNode {
    pub t: String,           // text 或 content-desc(text优先),trim后不截断,可为空
    pub b: [i32; 4],         // 设备坐标框
    pub id: Option<String>,  // resource-id 去掉包名前缀,如 "id/close_btn"
    pub class: String,       // 控件类名全称
    pub clickable: bool,
    pub scrollable: bool,
    pub checkable: bool,
    pub checked: bool,
    pub depth: u32,          // XML 嵌套深度(容器折叠的依据)
}

/// 系统级状态(dumpsys): 前台包名 + Activity 全名 + 软键盘是否弹起。
/// 任何一项采不到就如实留空/None,不装。
pub struct SysState {
    pub pkg: String,
    pub activity: String,
    pub ime: Option<bool>,
}

/// 解析 BMP 为灰度像素(sips 输出的 24/32bpp)
fn bmp_gray(path: &str) -> Option<Vec<u8>> {
    let d = std::fs::read(path).ok()?;
    if d.len() < 54 || &d[0..2] != b"BM" { return None; }
    let off = u32::from_le_bytes([d[10], d[11], d[12], d[13]]) as usize;
    let w = i32::from_le_bytes([d[18], d[19], d[20], d[21]]) as usize;
    let h = i32::from_le_bytes([d[22], d[23], d[24], d[25]]).unsigned_abs() as usize;
    let bpp = u16::from_le_bytes([d[28], d[29]]) as usize;
    if bpp != 24 && bpp != 32 { return None; }
    let bytes = bpp / 8;
    let row = (w * bytes + 3) / 4 * 4;
    let mut g = Vec::with_capacity(w * h);
    for r in 0..h {
        for c in 0..w {
            let i = off + r * row + c * bytes;
            if i + 2 < d.len() {
                g.push(((d[i] as u32 + d[i + 1] as u32 + d[i + 2] as u32) / 3) as u8);
            }
        }
    }
    Some(g)
}

/// 众数灰度(按16分桶) = 背景色估计
pub fn mode_gray(px: &[u8]) -> u8 {
    let mut hist = [0u32; 16];
    for &v in px { hist[(v >> 4) as usize] += 1; }
    let mut bi = 0;
    for i in 0..16 { if hist[i] > hist[bi] { bi = i; } }
    (bi as u8) * 16 + 8
}

/// 落点内容检查: tap点周围48x48的灰度 (标准差, 均值)
/// 48 而非 32: 白底列表应用里点稍偏于文字时,较大窗口仍能采到文字像素而放行,
/// 只有真正落在大片空白 void 上才被判空白。
pub fn patch_stats(img: &str, x: i32, y: i32, tmp: &str) -> Option<(f32, f32)> {
    let px = (x - 24).max(0);
    let py = (y - 24).max(0);
    let out = format!("{tmp}/_patch.bmp");
    Command::new("sips")
        .args(["--cropOffset", &py.to_string(), &px.to_string(), "-c", "48", "48",
               "-s", "format", "bmp", img, "--out", &out])
        .stdout(Stdio::null()).stderr(Stdio::null())
        .status().ok()?;
    let g = bmp_gray(&out)?;
    if g.is_empty() { return None; }
    let n = g.len() as f32;
    let mean = g.iter().map(|&v| v as f32).sum::<f32>() / n;
    let var = g.iter().map(|&v| { let d = v as f32 - mean; d * d }).sum::<f32>() / n;
    Some((var.sqrt(), mean))
}

/// 画面变化检测用 64px 缩略灰度图
pub fn thumb_gray(img: &str, tmp: &str, tag: &str) -> Option<Vec<u8>> {
    let out = format!("{tmp}/_thumb{tag}.bmp");
    Command::new("sips")
        .args(["-Z", "64", "-s", "format", "bmp", img, "--out", &out])
        .stdout(Stdio::null()).stderr(Stdio::null())
        .status().ok()?;
    bmp_gray(&out)
}

/// 两帧缩略图差异像素占比(%)。状态栏时钟等微小变化在阈值内。
pub fn frames_diff_pct(a: &[u8], b: &[u8]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return 100.0; }
    let diff = a.iter().zip(b).filter(|(x, y)| (**x as i16 - **y as i16).abs() > 14).count();
    diff as f32 / a.len() as f32 * 100.0
}

pub struct Adb {
    serial: Option<String>,
    tmp: String,
}

impl Adb {
    pub fn new(serial: Option<String>, tmp: String) -> Self {
        Adb { serial, tmp }
    }

    fn run(&self, args: &[&str]) -> Vec<u8> {
        let mut cmd = Command::new("adb");
        if let Some(s) = &self.serial {
            cmd.arg("-s").arg(s);
        }
        cmd.args(args).output().map(|o| o.stdout).unwrap_or_default()
    }

    /// 限时执行(uiautomator 偶发卡死 2~11s, 超时即杀); stdout 走临时文件避免管道阻塞
    fn run_timeout(&self, args: &[&str], ms: u64) -> Vec<u8> {
        let outfile = format!("{}/_cmd.out", self.tmp);
        let Ok(f) = std::fs::File::create(&outfile) else { return vec![] };
        let mut cmd = Command::new("adb");
        if let Some(s) = &self.serial {
            cmd.arg("-s").arg(s);
        }
        let Ok(mut child) = cmd.args(args).stdout(f).stderr(Stdio::null()).spawn() else {
            return vec![];
        };
        let t0 = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return std::fs::read(&outfile).unwrap_or_default(),
                Ok(None) => {
                    if t0.elapsed() > Duration::from_millis(ms) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return vec![];
                    }
                    sleep(Duration::from_millis(50));
                }
                Err(_) => return vec![],
            }
        }
    }

    fn input(&self, args: &[&str]) {
        let mut v = vec!["shell", "input"];
        v.extend_from_slice(args);
        self.run(&v);
    }

    /// 设备心跳(限时,防 adb 卡死拖垮全局): echo ok 能回来即活
    pub fn health_check(&self, ms: u64) -> bool {
        let out = self.run_timeout(&["shell", "echo", "ok"], ms);
        String::from_utf8_lossy(&out).trim() == "ok"
    }

    /// 设备复活: 先重启 adb server;仍无心跳且配置了模拟器命令则重启模拟器等开机。
    /// 返回复活后是否有心跳。emulator_cmd 形如 "<emulator路径> -avd <名字>"。
    pub fn revive(&self, emulator_cmd: &str) -> bool {
        println!("      🔧 设备复活: 重启 adb server…");
        let _ = self.run(&["kill-server"]);
        let _ = self.run(&["start-server"]);
        for _ in 0..10 {
            if self.health_check(5000) { return true; }
            sleep(Duration::from_millis(1000));
        }
        if emulator_cmd.trim().is_empty() { return false; }
        println!("      🔧 adb 复活无效,重启模拟器({emulator_cmd})…");
        let _ = Command::new("pkill").args(["-f", "avd agentphone"]).status();
        sleep(Duration::from_secs(3));
        let parts: Vec<&str> = emulator_cmd.split_whitespace().collect();
        if let Some(bin) = parts.first() {
            // 模拟器作独立后台进程拉起,父进程(本agent)退出与否不影响它
            let _ = Command::new(bin)
                .args(&parts[1..])
                .stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
        let _ = self.run(&["wait-for-device"]);
        for _ in 0..90 {
            let boot = String::from_utf8_lossy(
                &self.run_timeout(&["shell", "getprop", "sys.boot_completed"], 5000)).trim().to_string();
            if boot == "1" && self.health_check(5000) { return true; }
            sleep(Duration::from_secs(2));
        }
        self.health_check(5000)
    }

    fn dump_xml(&self, timeout_ms: u64) -> String {
        let out = self.run_timeout(&["exec-out", "uiautomator", "dump", "--compressed", "/dev/tty"], timeout_ms);
        let s = String::from_utf8_lossy(&out).to_string();
        if let (Some(i), Some(j)) = (s.find("<?xml"), s.rfind("</hierarchy>")) {
            return s[i..j + 12].to_string();
        }
        // 回退: 落到 sdcard 再取。必须先删旧文件: dump 失败(could not get idle state)时
        // 不写新文件,直接 cat 会把上一次留下的陈旧树当成本步观测——这是"假树"的真正主源,
        // 实测该文件能跨应用切换、甚至跨源应用进程死亡存活。删掉后失败即得空表,空表是合法观测。
        self.run(&["shell", "rm", "-f", "/sdcard/ui.xml"]);
        self.run_timeout(&["shell", "uiautomator", "dump", "--compressed", "/sdcard/ui.xml"], timeout_ms);
        let out = self.run(&["exec-out", "cat", "/sdcard/ui.xml"]);
        let s = String::from_utf8_lossy(&out).to_string();
        if let (Some(i), Some(j)) = (s.find("<?xml"), s.rfind("</hierarchy>")) {
            return s[i..j + 12].to_string();
        }
        String::new()
    }

    /// 元素采集(限时): (els文字层, full全量层, 多数包名, 原始XML)。
    /// 采不到返回全空 —— 空表本身是有效观测(canvas 页面即如此)。
    /// els 保持既有契约(有文字、40字截断、上限70条),渲染/吸附/身份证/沙盘零感知;
    /// full 无损(容器与纯图标在内、不截断);xml 原文由调用方落盘——解析器也是投影,地面真值以 XML 为准。
    pub fn els_full(&self, timeout_ms: u64) -> (Vec<Node>, Vec<FullNode>, String, String) {
        let xml = self.dump_xml(timeout_ms);
        if xml.is_empty() {
            return (vec![], vec![], String::new(), String::new());
        }
        let (els, full, pkg) = parse_dump(&xml);
        (els, full, pkg, xml)
    }

    /// 前台应用包名(dumpsys window 焦点窗口)。采不到返回空串。
    pub fn foreground_pkg(&self) -> String {
        let out = self.run_timeout(
            &["shell", "dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'"], 4000);
        let s = String::from_utf8_lossy(&out);
        for key in ["mCurrentFocus", "mFocusedApp"] {
            for line in s.lines().filter(|l| l.contains(key)) {
                let Some(i) = line.find(" u0 ") else { continue };
                let tok: String = line[i + 4..].chars()
                    .take_while(|c| *c != '/' && *c != '}' && *c != ' ')
                    .collect();
                if tok.contains('.')
                    && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
                {
                    return tok;
                }
            }
        }
        String::new()
    }

    /// 系统级状态一次采齐: 焦点窗口(包名+Activity) + 软键盘状态。
    /// 两条 dumpsys 合进一次 adb shell,省一趟往返;失败各自留空/None。
    pub fn sys_state(&self) -> SysState {
        let out = self.run_timeout(&["shell",
            "dumpsys window 2>/dev/null | grep -E 'mCurrentFocus|mFocusedApp'; \
             dumpsys input_method 2>/dev/null | grep -E 'mInputShown|isInputViewShown'"], 4000);
        parse_sys_state(&String::from_utf8_lossy(&out))
    }

    /// 可启动应用清单 (包名, 启动组件)。桌面/抽屉里能看到的应用即在此列。
    pub fn launchable_apps(&self) -> Vec<(String, String)> {
        let out = self.run_timeout(
            &["shell", "cmd", "package", "query-activities",
              "-a", "android.intent.action.MAIN",
              "-c", "android.intent.category.LAUNCHER", "--brief"], 6000);
        let s = String::from_utf8_lossy(&out);
        let mut v: Vec<(String, String)> = Vec::new();
        for line in s.lines() {
            let l = line.trim();
            if !l.contains('/') || l.contains(' ') || l.contains('=') { continue; }
            let pkg = l.split('/').next().unwrap_or("").to_string();
            if pkg.contains('.') && !v.iter().any(|(p, _)| p == &pkg) {
                v.push((pkg, l.to_string()));
            }
        }
        if v.is_empty() {
            // cmd 不可用的旧系统: 退化为三方包列表(无组件,launch 时走 monkey)
            let out = self.run(&["shell", "pm", "list", "packages", "-3"]);
            for line in String::from_utf8_lossy(&out).lines() {
                if let Some(p) = line.trim().strip_prefix("package:") {
                    if p.contains('.') { v.push((p.to_string(), String::new())); }
                }
            }
        }
        v.sort();
        v
    }

    /// 截屏并压成 672 宽 jpg 写到 out_jpg; 返回 (宽, 高)
    pub fn screen(&self, out_jpg: &str) -> Option<(i32, i32)> {
        let raw = format!("{}/_raw.png", self.tmp);
        let png = self.run(&["exec-out", "screencap", "-p"]);
        if png.len() < 1000 {
            return None;
        }
        std::fs::write(&raw, &png).ok()?;
        Command::new("sips")
            .args(["--resampleWidth", "672", "-s", "format", "jpeg",
                   "-s", "formatOptions", "85", &raw, "--out", out_jpg])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status().ok()?;
        let out = Command::new("sips")
            .args(["-g", "pixelWidth", "-g", "pixelHeight", out_jpg])
            .output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout).to_string();
        let w = s.split("pixelWidth:").nth(1)?.split_whitespace().next()?.parse().ok()?;
        let h = s.split("pixelHeight:").nth(1)?.split_whitespace().next()?.parse().ok()?;
        Some((w, h))
    }

    /// 快照小图: 截屏压成64px灰度,用于等画面安静/量背景动静(不入档)
    pub fn quick_thumb(&self, tag: &str) -> Option<Vec<u8>> {
        let png = self.run(&["exec-out", "screencap", "-p"]);
        if png.len() < 1000 {
            return None;
        }
        let raw = format!("{}/_qt{tag}.png", self.tmp);
        std::fs::write(&raw, &png).ok()?;
        let out = format!("{}/_qt{tag}.bmp", self.tmp);
        Command::new("sips")
            .args(["-Z", "64", "-s", "format", "bmp", &raw, "--out", &out])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status().ok()?;
        bmp_gray(&out)
    }

    /// 物理屏幕尺寸
    pub fn size(&self) -> (i32, i32) {
        let out = self.run(&["shell", "wm", "size"]);
        let s = String::from_utf8_lossy(&out).to_string();
        let nums: Vec<i32> = s.split(|c: char| !c.is_ascii_digit())
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse().ok())
            .collect();
        if nums.len() >= 2 { (nums[0], nums[1]) } else { (1080, 2340) }
    }

    pub fn tap(&self, x: i32, y: i32) {
        self.input(&["tap", &x.to_string(), &y.to_string()]);
    }
    pub fn swipe(&self, x: i32, y: i32, x2: i32, y2: i32) {
        self.input(&["swipe", &x.to_string(), &y.to_string(),
                     &x2.to_string(), &y2.to_string(), "350"]);
    }
    pub fn scroll_down(&self) {
        self.input(&["swipe", "540", "1700", "540", "600", "350"]);
    }
    pub fn scroll_up(&self) {
        self.input(&["swipe", "540", "700", "540", "1800", "350"]);
    }
    pub fn type_text(&self, text: &str) {
        self.input(&["text", &text.replace(' ', "%s")]);
    }
    pub fn back(&self) {
        self.input(&["keyevent", "4"]);
    }
    /// 回桌面: 系统级 HOME 键,与设备导航方式(手势/三键)无关,全屏应用也拦不住
    pub fn home(&self) {
        self.input(&["keyevent", "3"]);
    }
    /// 掐死后台应用(假树/无障碍事件风暴的源头)。目标应用与系统包不碰——调用方过 pkg_killable 把关
    pub fn force_stop(&self, pkg: &str) {
        if !pkg.is_empty() {
            self.run_timeout(&["shell", "am", "force-stop", pkg], 6000);
        }
    }
    /// 直接启动应用: 有组件走 am start,否则 monkey 兜底
    pub fn launch(&self, pkg: &str, comp: &str) {
        if !comp.is_empty() {
            self.run_timeout(&["shell", "am", "start", "-n", comp], 8000);
        } else if !pkg.is_empty() {
            self.run_timeout(&["shell", "monkey", "-p", pkg,
                               "-c", "android.intent.category.LAUNCHER", "1"], 8000);
        }
    }
}

fn parse_bounds(b: &str) -> Option<[i32; 4]> {
    // "[x1,y1][x2,y2]"
    let nums: Vec<i32> = b
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() == 4 {
        Some([nums[0], nums[1], nums[2], nums[3]])
    } else {
        None
    }
}

/// 属性值解码: XML转义(&#10;/&quot;等)还原为真实字符,再把换行/制表压成空格——
/// t 是行式投影(一行一元素,不能出现真实换行),字符级原文永远在落盘的XML里。
fn attr_str(a: &quick_xml::events::attributes::Attribute) -> String {
    let s = a.unescape_value()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).to_string());
    if s.contains(['\n', '\r', '\t']) { s.replace(['\n', '\r', '\t'], " ") } else { s }
}

/// dump XML → (els文字层, full全量层, 多数包名)。纯函数,单测钉行为:
/// els 与旧版逐字节等价(有文字、trim、40字截断、前70条、正面积);
/// full 对每个有效框的节点无损收录(含无文字容器),depth 记 XML 嵌套层级。
pub fn parse_dump(xml: &str) -> (Vec<Node>, Vec<FullNode>, String) {
    let mut els: Vec<Node> = Vec::new();
    let mut full: Vec<FullNode> = Vec::new();
    let mut pkg_hist: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut reader = Reader::from_str(xml);
    let mut depth: u32 = 0;
    loop {
        let ev = reader.read_event();
        let (e, d) = match &ev {
            Ok(Event::Start(e)) => { depth += 1; (e, depth) }
            Ok(Event::Empty(e)) => (e, depth + 1),
            Ok(Event::End(_)) => { depth = depth.saturating_sub(1); continue; }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => continue,
        };
        let mut text = String::new();
        let mut desc = String::new();
        let mut bounds = String::new();
        let mut id: Option<String> = None;
        let mut class = String::new();
        let (mut clickable, mut scrollable, mut checkable, mut checked) = (false, false, false, false);
        for a in e.attributes().flatten() {
            let key = String::from_utf8_lossy(a.key.as_ref()).to_string();
            let val = attr_str(&a);
            match key.as_str() {
                "text" => text = val,
                "content-desc" => desc = val,
                "bounds" => bounds = val,
                "package" => { *pkg_hist.entry(val).or_insert(0) += 1; }
                "resource-id" => {
                    if !val.is_empty() {
                        // "com.pkg:id/name" → "id/name"(包名前缀冗余;原始值在 XML 落盘里)
                        id = Some(val.split_once(':').map(|(_, r)| r.to_string()).unwrap_or(val));
                    }
                }
                "class" => class = val,
                "clickable" => clickable = val == "true",
                "scrollable" => scrollable = val == "true",
                "checkable" => checkable = val == "true",
                "checked" => checked = val == "true",
                _ => {}
            }
        }
        let t = if !text.trim().is_empty() { text } else { desc };
        let t = t.trim();
        if let Some(b) = parse_bounds(&bounds) {
            if b[2] > b[0] && b[3] > b[1] {
                if !t.is_empty() && els.len() < 70 {
                    let mut tt = t.to_string();
                    while tt.chars().count() > 40 { tt.pop(); }
                    els.push(Node { t: tt, b });
                }
                full.push(FullNode {
                    t: t.to_string(), b, id, class,
                    clickable, scrollable, checkable, checked, depth: d,
                });
            }
        }
    }
    let pkg = pkg_hist.into_iter().max_by_key(|(_, c)| *c)
        .map(|(p, _)| p).unwrap_or_default();
    (els, full, pkg)
}

/// dumpsys 焦点/键盘输出 → SysState。纯函数供单测。
/// 包名解析规则与 foreground_pkg 同源:mCurrentFocus 优先,mFocusedApp 兜底,
/// " u0 " 后取 token,包名段须形如反域名;Activity 段以 '.' 开头时补全包名。
pub fn parse_sys_state(s: &str) -> SysState {
    let mut pkg = String::new();
    let mut activity = String::new();
    'outer: for key in ["mCurrentFocus", "mFocusedApp"] {
        for line in s.lines().filter(|l| l.contains(key)) {
            let Some(i) = line.find(" u0 ") else { continue };
            let tok: String = line[i + 4..].chars()
                .take_while(|c| *c != '}' && *c != ' ')
                .collect();
            let (p, a) = match tok.split_once('/') {
                Some((p, a)) => (p.to_string(), a.to_string()),
                None => (tok, String::new()),
            };
            if p.contains('.')
                && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            {
                activity = if a.starts_with('.') { format!("{p}{a}") } else { a };
                pkg = p;
                break 'outer;
            }
        }
    }
    let mut ime = None;
    for k in ["mInputShown=", "isInputViewShown="] {
        if let Some(i) = s.find(k) {
            ime = Some(s[i + k.len()..].starts_with("true"));
            break;
        }
    }
    SysState { pkg, activity, ime }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_layer_keeps_containers_and_attrs() {
        // 容器(RecyclerView,无文字)只进 full 不进 els;属性与深度如实解析
        let xml = r#"<?xml version='1.0'?><hierarchy>
<node text="" class="androidx.recyclerview.widget.RecyclerView" package="t.app" bounds="[0,100][1080,2000]" scrollable="true" clickable="false">
<node text="头条新闻" resource-id="t.app:id/title" class="android.widget.TextView" package="t.app" bounds="[10,110][500,160]" clickable="true"/>
<node text="" content-desc="关闭" class="android.widget.ImageView" package="t.app" bounds="[900,110][960,160]" clickable="true" checkable="true" checked="true"/>
</node></hierarchy>"#;
        let (els, full, pkg) = parse_dump(xml);
        assert_eq!(pkg, "t.app");
        assert_eq!(els.len(), 2, "els 只收有文字的(text或desc)");
        assert_eq!(full.len(), 3, "full 连无文字容器一起收");
        let rec = &full[0];
        assert!(rec.scrollable && !rec.clickable && rec.t.is_empty());
        assert!(rec.class.ends_with("RecyclerView"));
        let title = &full[1];
        assert_eq!(title.id.as_deref(), Some("id/title"), "resource-id 去包名前缀");
        assert!(title.clickable);
        assert!(title.depth > rec.depth, "子节点深度大于容器");
        let icon = &full[2];
        assert_eq!(icon.t, "关闭");
        assert!(icon.checkable && icon.checked);
    }

    #[test]
    fn els_truncation_preserved_full_lossless() {
        // els 保留旧契约(70条/40字),full 无损——账本层不再吃截断的亏
        let long = "这是一条超过四十个字的超长文本用来验证四十字截断在文字层仍然生效而全量层原文完整保留丝毫不丢";
        assert!(long.chars().count() > 40);
        let mut xml = String::from("<?xml version='1.0'?><hierarchy>");
        for i in 0..72 {
            xml.push_str(&format!(
                r#"<node text="词{i}" class="c" package="t.app" bounds="[0,{}][100,{}]" />"#,
                i * 10, i * 10 + 9));
        }
        xml.push_str(&format!(
            r#"<node text="{long}" class="c" package="t.app" bounds="[0,900][500,950]"/></hierarchy>"#));
        let (els, full, _) = parse_dump(&xml);
        assert_eq!(els.len(), 70, "els 上限70条不变");
        assert_eq!(full.len(), 73, "full 全量收录");
        let f_long = full.iter().find(|f| f.t.chars().count() > 40);
        assert!(f_long.is_some(), "full 里长文本原文保留");
        assert!(els.iter().all(|n| n.t.chars().count() <= 40), "els 40字截断不变");
    }

    #[test]
    fn sys_state_parses_activity_and_ime() {
        let s = "  mCurrentFocus=Window{5b0f0 u0 com.ss.android.article.news/com.ss.android.article.news.activity.MainActivity}\n  mInputShown=true\n";
        let st = parse_sys_state(s);
        assert_eq!(st.pkg, "com.ss.android.article.news");
        assert_eq!(st.activity, "com.ss.android.article.news.activity.MainActivity");
        assert_eq!(st.ime, Some(true));
    }

    #[test]
    fn sys_state_relative_activity_and_no_ime() {
        // 相对写法 ".MainActivity" 补全包名;无键盘行 → None(不装)
        let s = "  mFocusedApp=ActivityRecord{abc u0 com.demo.app/.MainActivity t12}\n";
        let st = parse_sys_state(s);
        assert_eq!(st.pkg, "com.demo.app");
        assert_eq!(st.activity, "com.demo.app.MainActivity");
        assert_eq!(st.ime, None);
        let s2 = "mCurrentFocus=Window{x u0 StatusBar}\nisInputViewShown=false";
        let st2 = parse_sys_state(s2);
        assert_eq!(st2.pkg, "", "StatusBar 不是包名,拒收");
        assert_eq!(st2.ime, Some(false));
    }
}
