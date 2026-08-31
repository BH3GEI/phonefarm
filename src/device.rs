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

    /// 元素列表(限时)。采不到返回空表 —— 空表本身是有效观测(canvas 页面即如此)。
    /// 同时返回树的多数包名,供假树识别(uiautomator 偶发吐出上一个应用的陈旧树)。
    pub fn els(&self, timeout_ms: u64) -> (Vec<Node>, String) {
        let xml = self.dump_xml(timeout_ms);
        if xml.is_empty() {
            return (vec![], String::new());
        }
        let mut nodes = Vec::new();
        let mut pkg_hist: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut reader = Reader::from_str(&xml);
        loop {
            match reader.read_event() {
                Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                    let mut text = String::new();
                    let mut desc = String::new();
                    let mut bounds = String::new();
                    for a in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(a.key.as_ref()).to_string();
                        let val = String::from_utf8_lossy(&a.value).to_string();
                        match key.as_str() {
                            "text" => text = val,
                            "content-desc" => desc = val,
                            "bounds" => bounds = val,
                            "package" => { *pkg_hist.entry(val).or_insert(0) += 1; }
                            _ => {}
                        }
                    }
                    let t = if !text.trim().is_empty() { text } else { desc };
                    let t = t.trim();
                    if !t.is_empty() {
                        if let Some(b) = parse_bounds(&bounds) {
                            if b[2] > b[0] && b[3] > b[1] {
                                let mut tt = t.to_string();
                                while tt.chars().count() > 40 { tt.pop(); }
                                nodes.push(Node { t: tt, b });
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
        }
        nodes.truncate(70);
        let pkg = pkg_hist.into_iter().max_by_key(|(_, c)| *c)
            .map(|(p, _)| p).unwrap_or_default();
        (nodes, pkg)
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
