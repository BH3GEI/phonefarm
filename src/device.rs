//! Device 层: adb/hdc 封装(截屏/元素列表/手势) + 像素工具。临时文件一律进 tmp 目录。
//! 后端由 Device enum 统一分发,serial 带 "hdc:" 前缀走 OpenHarmony,其余走 Android。
use quick_xml::events::Event;
use quick_xml::Reader;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::OnceLock;
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

/// sips 读图片像素尺寸(host 侧,与设备后端无关)。
fn sips_wh(path: &str) -> Option<(i32, i32)> {
    let out = Command::new("sips")
        .args(["-g", "pixelWidth", "-g", "pixelHeight", path])
        .output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    let w = s.split("pixelWidth:").nth(1)?.split_whitespace().next()?.parse().ok()?;
    let h = s.split("pixelHeight:").nth(1)?.split_whitespace().next()?.parse().ok()?;
    Some((w, h))
}

/// adb 搜索顺序(Improve Spec 环境自举): 纯函数供单测。
/// ADB_BIN 显式指定最高;仓库自带 platform-tools/ 次之(自己目录自己部署,clone 即跑,
/// 规格细化③的"优先"胜过枚举序);再 PATH;最后常见 SDK 安装点。"PATH" 是哨兵项。
pub fn adb_search_order(adb_bin: Option<&str>, home: Option<&str>) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(b) = adb_bin {
        if !b.trim().is_empty() { v.push(b.to_string()); }
    }
    v.push("platform-tools/adb".into());
    v.push("PATH".into());
    if let Some(h) = home {
        v.push(format!("{h}/Library/Android/sdk/platform-tools/adb"));
        v.push(format!("{h}/Library/Application Support/CindyGlobal/android-platform-tools/darwin-arm64/platform-tools/adb"));
    }
    v.push("/opt/homebrew/share/android-commandlinetools/platform-tools/adb".into());
    v
}

/// adb 定位: 一次定位进程级缓存。找不到返回 None——调用方给可操作警告,不 panic。
pub fn locate_adb() -> Option<String> {
    static ADB: OnceLock<Option<String>> = OnceLock::new();
    ADB.get_or_init(|| {
        let bin = std::env::var("ADB_BIN").ok();
        let home = std::env::var("HOME").ok();
        for c in adb_search_order(bin.as_deref(), home.as_deref()) {
            if c == "PATH" {
                let ok = Command::new("adb").arg("--version")
                    .stdout(Stdio::null()).stderr(Stdio::null())
                    .status().map(|st| st.success()).unwrap_or(false);
                if ok { return Some("adb".into()); }
            } else if std::path::Path::new(&c).is_file() {
                return Some(c);
            }
        }
        None
    }).clone()
}

pub struct Adb {
    bin: String, // 定位到的 adb 路径(构造时解析,见 locate_adb)
    serial: Option<String>,
    tmp: String,
    frozen: AtomicBool, // --freeze-on-done: 置位后不再发"写"动作,只放行观测
}

impl Adb {
    pub fn new(serial: Option<String>, tmp: String) -> Self {
        let Some(bin) = locate_adb() else {
            eprintln!("⚠ 未找到 adb: 请安装 Android platform-tools(或把 platform-tools/ 整个目录放进仓库根),或用 ADB_BIN=/path/to/adb 指定后重跑。");
            std::process::exit(2);
        };
        Adb { bin, serial, tmp, frozen: AtomicBool::new(false) }
    }

    /// --freeze-on-done: 置位后本后端不再对设备发"写"动作(手势/键/force-stop/launch),
    /// 只放行观测(截屏/dumpsys/遥测采集)。done 定局后置位,保住终局画面给 harness 判分。
    pub fn set_frozen(&self) { self.frozen.store(true, Ordering::Relaxed); }
    fn frozen(&self) -> bool { self.frozen.load(Ordering::Relaxed) }

    fn run(&self, args: &[&str]) -> Vec<u8> {
        let mut cmd = Command::new(&self.bin);
        if let Some(s) = &self.serial {
            cmd.arg("-s").arg(s);
        }
        cmd.args(args).output().map(|o| o.stdout).unwrap_or_default()
    }

    /// 限时执行(uiautomator 偶发卡死 2~11s, 超时即杀); stdout 走临时文件避免管道阻塞
    fn run_timeout(&self, args: &[&str], ms: u64) -> Vec<u8> {
        let outfile = format!("{}/_cmd.out", self.tmp);
        let Ok(f) = std::fs::File::create(&outfile) else { return vec![] };
        let mut cmd = Command::new(&self.bin);
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
        if self.frozen() { return; }
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

    /// 剪贴板(best-effort): 新版Android常拒绝后台读,读不到如实返回None,不装
    pub fn clipboard(&self) -> Option<String> {
        let out = self.run_timeout(&["shell", "cmd", "clipboard", "get-primary-clip"], 3000);
        let s = String::from_utf8_lossy(&out).trim().to_string();
        if s.is_empty() || s.contains("Exception") || s.contains("denied") || s.contains("Error") {
            None
        } else {
            Some(s.chars().take(80).collect())
        }
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
        if self.frozen() { return; }
        if !pkg.is_empty() {
            self.run_timeout(&["shell", "am", "force-stop", pkg], 6000);
        }
    }
    /// 直接启动应用: 有组件走 am start -W(阻塞至可见,顺手产出冷启动 WaitTime——遥测层
    /// 唯一"写"动作即启动本身,计时是白拿的),否则 monkey 兜底。返回冷启动毫秒(采不到None)。
    pub fn launch(&self, pkg: &str, comp: &str) -> Option<i64> {
        if self.frozen() { return None; }
        if !comp.is_empty() {
            let out = self.run_timeout(&["shell", "am", "start", "-W", "-n", comp], 8000);
            return crate::telemetry::parse_coldstart_a(&String::from_utf8_lossy(&out)).0;
        } else if !pkg.is_empty() {
            self.run_timeout(&["shell", "monkey", "-p", pkg,
                               "-c", "android.intent.category.LAUNCHER", "1"], 8000);
        }
        None
    }

    /// 遥测开局(Telemetry Spec): root 探测一次 + 采集脚本上载(整批命令一趟 shell 跑完)。
    /// 返回是否 root(root 层字段的开关,非 root 自动跳过)。
    pub fn telemetry_setup(&self, pkg: &str) -> bool {
        let root = String::from_utf8_lossy(&self.run_timeout(&["shell", "id"], 4000)).contains("uid=0");
        let l1 = format!("{}/_pf_t1.sh", self.tmp);
        let l2 = format!("{}/_pf_t2.sh", self.tmp);
        if std::fs::write(&l1, tele_script_a(pkg, root, false)).is_ok() {
            self.run_timeout(&["push", &l1, "/data/local/tmp/pf_t1.sh"], 6000);
        }
        if std::fs::write(&l2, tele_script_a(pkg, root, true)).is_ok() {
            self.run_timeout(&["push", &l2, "/data/local/tmp/pf_t2.sh"], 6000);
        }
        root
    }

    /// 遥测采集: 高频脚本每步,heavy=true 时重量级脚本并跑(仍是一趟 shell 往返)。
    /// 限时,失败返空文本(解析层对空输入产出全 None——采不到留空,不装)。
    pub fn telemetry_collect(&self, heavy: bool) -> String {
        let cmd = if heavy {
            "sh /data/local/tmp/pf_t1.sh 2>/dev/null; sh /data/local/tmp/pf_t2.sh 2>/dev/null"
        } else {
            "sh /data/local/tmp/pf_t1.sh 2>/dev/null"
        };
        String::from_utf8_lossy(&self.run_timeout(&["shell", cmd], 10000)).to_string()
    }

    pub fn telemetry(&self, heavy: bool, pkg: &str) -> crate::telemetry::Telemetry {
        crate::telemetry::from_android(&self.telemetry_collect(heavy), pkg)
    }

    /// 任意设备 shell 命令(CLI probe/exec 用),限时,原样返回输出
    pub fn shell(&self, cmd: &str, ms: u64) -> String {
        String::from_utf8_lossy(&self.run_timeout(&["shell", cmd], ms)).to_string()
    }
}

/// OpenHarmony 后端: hdc 封装,方法面与 Adb 逐位同构,由 Device 统一分发。
/// 命令映射(intel-mac 挂的真机 OH 3.2/arm64 实测):
///   截屏 `snapshot_display -f <路径>`(强制 .jpeg 后缀) + `file recv`
///   UI树 `uitest dumpLayout -p <路径>`(JSON) + `file recv` → parse_layout_json
///   注入 `uitest uiInput click/swipe/keyEvent Back|Home/inputText`
///   启动 `aa start -b <bundle> -a <ability>` — ability 必须用 bm dump -n 的 mainAbility
///   全名(猜 EntryAbility 得 10104001),与 Android 从 query-activities 解析组件对称
///   清单 `bm dump -a`; 心跳 `shell echo`; 复活 `hdc kill/start`
/// 三个实测软点(设备冒烟局重点观察,机制继承但行为未经长跑验证):
///   ①前台包名: OH 3.2 无 mCurrentFocus 等价物,sys_state 走 `aa dump -a` 找 FOREGROUND
///     记录;采不到如实留空——SysState 契约本就容忍空值,开局归位/假树识别相应优雅退化
///   ②`aa force-stop` 实测报错 → pidof+kill 兜底
///   ③dumpLayout 卡死/陈旧的失效形态未知 → 沿用"先删旧文件"的假树卫生纪律(Android 同款教训)
pub struct Hdc {
    key: Option<String>, // hdc -t <connect key>
    tmp: String,
    /// 分辨率缓存: OH 无 wm size,取尺寸要付一趟截屏+回传;物理屏不会中途变,采一次终身用
    size_cache: OnceLock<(i32, i32)>,
    /// uiInput inputText 需要坐标(点哪输哪): 记最近一次 tap 落点。契约里 type 紧跟在
    /// 点输入框之后,落点即输入框(再点一次同框只是重聚焦,无害);没点过则屏幕中心兜底。
    last_tap: (AtomicI32, AtomicI32),
    /// 前台包名缓存(真机对拍实证): dumpLayout 合并窗口按 z 序顶窗在前——首个非空
    /// bundleName 就是用户眼中的前台(弹窗在时正确给出弹窗进程);而 aa dump 的 mission
    /// 序不分层(实测把被弹窗盖住的 app 排在前)。故 OH 前台权威源=最近一次树的顶窗包名,
    /// aa dump 只作树里完全没有包名时的兜底。els_full 每步刷新,与树同源自洽——
    /// Android 的假树病根(陈旧dump文件)在 OH 不存在(每次 rm 后现 dump),树≠前台的
    /// 对质在 OH 因此自然失效,这是设计而非疏漏。
    fg_cache: std::sync::Mutex<Option<String>>,
    frozen: AtomicBool, // --freeze-on-done(与 Adb 对称)
}

impl Hdc {
    pub fn new(key: Option<String>, tmp: String) -> Self {
        Hdc { key, tmp, size_cache: OnceLock::new(),
              last_tap: (AtomicI32::new(-1), AtomicI32::new(-1)),
              fg_cache: std::sync::Mutex::new(None),
              frozen: AtomicBool::new(false) }
    }

    pub fn set_frozen(&self) { self.frozen.store(true, Ordering::Relaxed); }
    fn frozen(&self) -> bool { self.frozen.load(Ordering::Relaxed) }

    fn base(&self) -> Command {
        let mut cmd = Command::new("hdc");
        if let Some(k) = &self.key {
            cmd.arg("-t").arg(k);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Vec<u8> {
        self.base().args(args).output().map(|o| o.stdout).unwrap_or_default()
    }

    /// 限时执行(hdc 同样会卡死): 与 Adb::run_timeout 同构,但输出文件带 tag——
    /// capture 里 els_full 与 quick_thumb 在两个线程并行,共用一个文件会互相截胡
    fn run_timeout(&self, args: &[&str], ms: u64, tag: &str) -> Vec<u8> {
        let outfile = format!("{}/_hdc_{tag}.out", self.tmp);
        let Ok(f) = std::fs::File::create(&outfile) else { return vec![] };
        let Ok(mut child) = self.base().args(args).stdout(f).stderr(Stdio::null()).spawn() else {
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

    fn ui_input(&self, args: &[&str]) {
        if self.frozen() { return; }
        let mut v = vec!["shell", "uitest", "uiInput"];
        v.extend_from_slice(args);
        self.run_timeout(&v, 8000, "input");
    }

    /// 拉回设备文件(file recv 是 hdc 顶层命令,非 shell);先删本地旧文件,防陈旧内容冒充
    fn recv(&self, remote: &str, local: &str, ms: u64, tag: &str) -> bool {
        let _ = std::fs::remove_file(local);
        self.run_timeout(&["file", "recv", remote, local], ms, tag);
        std::fs::metadata(local).map(|m| m.len() > 0).unwrap_or(false)
    }

    /// 设备心跳(限时): echo ok 能回来即活
    pub fn health_check(&self, ms: u64) -> bool {
        let out = self.run_timeout(&["shell", "echo", "ok"], ms, "hc");
        String::from_utf8_lossy(&out).trim() == "ok"
    }

    /// 设备复活: 真机走 hdc server 重启;emulator_cmd 是 Android 模拟器专用,OH 侧不适用
    pub fn revive(&self, _emulator_cmd: &str) -> bool {
        println!("      🔧 设备复活: 重启 hdc server…");
        let _ = self.run(&["kill"]);
        sleep(Duration::from_millis(800));
        let _ = self.run(&["start"]);
        for _ in 0..10 {
            if self.health_check(5000) { return true; }
            sleep(Duration::from_millis(1000));
        }
        false
    }

    /// UI树(JSON): 先删旧文件再 dump——陈旧文件冒充本步观测是 Android 假树的主源,
    /// 同款卫生纪律照搬;dump 失败即 recv 不到,空表是合法观测
    fn dump_layout(&self, timeout_ms: u64) -> String {
        const REMOTE: &str = "/data/local/tmp/pf_layout.json";
        self.run_timeout(&["shell", "rm", "-f", REMOTE], 3000, "els");
        self.run_timeout(&["shell", "uitest", "dumpLayout", "-p", REMOTE], timeout_ms, "els");
        let local = format!("{}/_layout.json", self.tmp);
        if !self.recv(REMOTE, &local, 5000, "els") { return String::new(); }
        let s = std::fs::read_to_string(&local).unwrap_or_default();
        if s.trim_start().starts_with('{') { s } else { String::new() }
    }

    /// 元素采集: 契约与 Adb::els_full 逐位对齐,第4元是原始 JSON(调用方落盘作地面真值)。
    /// 顺手刷新前台缓存(树顶窗包名,见 fg_cache 注)。
    pub fn els_full(&self, timeout_ms: u64) -> (Vec<Node>, Vec<FullNode>, String, String) {
        let json = self.dump_layout(timeout_ms);
        if json.is_empty() {
            return (vec![], vec![], String::new(), String::new());
        }
        let (els, full, pkg) = parse_layout_json(&json);
        if !pkg.is_empty() {
            if let Ok(mut c) = self.fg_cache.lock() { *c = Some(pkg.clone()); }
        }
        (els, full, pkg, json)
    }

    /// 系统级状态: 前台包名走树顶窗缓存(权威源,见 fg_cache 注);缓存空(如开局归位
    /// 早于首次采集)则现场轻采一棵树;树里完全没包名才落到 aa dump 兜底(mission 序
    /// 不分层,弹窗场景会给错)。activity 在 OH 侧无可靠对应(窗口 abilityName 实测常空),
    /// 如实留空;键盘状态无采法,如实 None。
    pub fn sys_state(&self) -> SysState {
        if let Ok(c) = self.fg_cache.lock() {
            if let Some(p) = c.as_ref() {
                return SysState { pkg: p.clone(), activity: String::new(), ime: None };
            }
        }
        let json = self.dump_layout(2500);
        if !json.is_empty() {
            let (_, _, pkg) = parse_layout_json(&json);
            if !pkg.is_empty() {
                if let Ok(mut c) = self.fg_cache.lock() { *c = Some(pkg.clone()); }
                return SysState { pkg, activity: String::new(), ime: None };
            }
        }
        let out = self.run_timeout(&["shell", "aa", "dump", "-a"], 4000, "sys");
        let (pkg, ability) = parse_aa_dump(&String::from_utf8_lossy(&out));
        SysState { pkg, activity: ability, ime: None }
    }

    pub fn foreground_pkg(&self) -> String {
        self.sys_state().pkg
    }

    /// OH 无 shell 剪贴板读法(cmd clipboard 是 Android 的),如实 None
    pub fn clipboard(&self) -> Option<String> {
        None
    }

    /// 应用清单: bm dump -a 全量 bundle 名。OH 拿不到"桌面可见"这层过滤,系统 bundle 会
    /// 混入——launch 门槛只看清单成员资格、目标选择权在模型,污染无害且 com.ohos.* 已进
    /// pkg_killable 保护名单;组件位留空,launch 时按需 bm dump -n 解析(逐个预解析要 N 趟往返)
    pub fn launchable_apps(&self) -> Vec<(String, String)> {
        let out = self.run_timeout(&["shell", "bm", "dump", "-a"], 6000, "apps");
        parse_bm_bundles(&String::from_utf8_lossy(&out))
            .into_iter().map(|b| (b, String::new())).collect()
    }

    /// 截屏到本地(整幅 jpeg): snapshot_display 强制 .jpeg 后缀(实测),再 file recv
    fn shot(&self, local: &str, tag: &str) -> bool {
        const REMOTE: &str = "/data/local/tmp/pf_shot.jpeg";
        self.run_timeout(&["shell", "snapshot_display", "-f", REMOTE], 8000, tag);
        self.recv(REMOTE, local, 8000, tag)
    }

    /// 截屏并压成 672 宽 jpg 写到 out_jpg; 返回 (宽, 高)——与 Adb::screen 同契约
    pub fn screen(&self, out_jpg: &str) -> Option<(i32, i32)> {
        let raw = format!("{}/_raw.jpeg", self.tmp);
        if !self.shot(&raw, "shot") { return None; }
        Command::new("sips")
            .args(["--resampleWidth", "672", "-s", "format", "jpeg",
                   "-s", "formatOptions", "85", &raw, "--out", out_jpg])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status().ok()?;
        sips_wh(out_jpg)
    }

    /// 快照小图: 64px 灰度(等画面安静用)。比 adb 版多付一趟 file recv,settle 循环
    /// 的轮询频率随之自然放缓——cap_ms 是墙钟上限,契约不变
    pub fn quick_thumb(&self, tag: &str) -> Option<Vec<u8>> {
        let raw = format!("{}/_qt{tag}.jpeg", self.tmp);
        if !self.shot(&raw, "thumb") { return None; }
        let out = format!("{}/_qt{tag}.bmp", self.tmp);
        Command::new("sips")
            .args(["-Z", "64", "-s", "format", "bmp", &raw, "--out", &out])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status().ok()?;
        bmp_gray(&out)
    }

    /// 物理屏幕尺寸: OH 无 wm size,从整幅截屏实测并缓存;取不到用 720x1280 兜底(OH 设备常见档)
    pub fn size(&self) -> (i32, i32) {
        *self.size_cache.get_or_init(|| {
            let raw = format!("{}/_size.jpeg", self.tmp);
            if self.shot(&raw, "size") {
                if let Some(wh) = sips_wh(&raw) { return wh; }
            }
            (720, 1280)
        })
    }

    pub fn tap(&self, x: i32, y: i32) {
        self.last_tap.0.store(x, Ordering::Relaxed);
        self.last_tap.1.store(y, Ordering::Relaxed);
        self.ui_input(&["click", &x.to_string(), &y.to_string()]);
    }

    /// uiInput swipe 用速度(px/s)而非时长: 按 Android 350ms 手势的手感换算,夹在合法区间
    pub fn swipe(&self, x: i32, y: i32, x2: i32, y2: i32) {
        let dist = (((x2 - x) as f64).hypot((y2 - y) as f64)).max(1.0);
        let v = ((dist / 0.35) as i32).clamp(200, 40000);
        self.ui_input(&["swipe", &x.to_string(), &y.to_string(),
                        &x2.to_string(), &y2.to_string(), &v.to_string()]);
    }

    /// 滚动: Adb 版坐标按 1080x2340 写死,这里按实测分辨率等比换算(比例同源)
    pub fn scroll_down(&self) {
        let (w, h) = self.size();
        self.swipe(w / 2, h * 1700 / 2340, w / 2, h * 600 / 2340);
    }
    pub fn scroll_up(&self) {
        let (w, h) = self.size();
        self.swipe(w / 2, h * 700 / 2340, w / 2, h * 1800 / 2340);
    }

    /// 文本输入: uiInput inputText 要坐标,用最近 tap 落点(见 last_tap 注),中心兜底。
    /// 真机实测三件事定形: ①inputText 自带落点点击+光标追加(ASCII/中文均直落,无需预聚焦);
    /// ②hdc 参数按空白切分且引号不设防——含空格整体发送必炸(uitest 报参数数量错,一字不落);
    /// ③空格走 keyEvent 2050(OH KEYCODE_SPACE)可落位。故逐词 inputText、逐空格 keyEvent,
    /// 连续空格按个数保真;引号/换行/制表压成空格(t 行式投影同款纪律,引号在 hdc 通道不可表达)。
    pub fn type_text(&self, text: &str) {
        let (mut x, mut y) = (self.last_tap.0.load(Ordering::Relaxed),
                              self.last_tap.1.load(Ordering::Relaxed));
        if x < 0 || y < 0 {
            let (w, h) = self.size();
            x = w / 2;
            y = h / 2;
        }
        let t = text.replace(['"', '\n', '\r', '\t'], " ");
        let mut first = true;
        for seg in t.split(' ') {
            if !first {
                self.ui_input(&["keyEvent", "2050"]);
            }
            first = false;
            if !seg.is_empty() {
                self.ui_input(&["inputText", &x.to_string(), &y.to_string(), seg]);
            }
        }
    }

    pub fn back(&self) {
        self.ui_input(&["keyEvent", "Back"]);
    }
    pub fn home(&self) {
        self.ui_input(&["keyEvent", "Home"]);
    }

    /// 强停(软点②): aa force-stop 在 OH 3.2 实测报错,标准命令先试、pidof+kill 兜底
    /// (成功则 pidof 落空,兜底自然无操作)。系统 bundle 不碰的关卡在调用方 pkg_killable。
    pub fn force_stop(&self, pkg: &str) {
        if self.frozen() { return; }
        if pkg.is_empty() { return; }
        self.run_timeout(&["shell", "aa", "force-stop", pkg], 6000, "stop");
        // 整句交远端 shell 解释($() 在设备侧展开)
        let cmd = format!("p=$(pidof {pkg}); [ -n \"$p\" ] && kill -9 $p");
        self.run_timeout(&["shell", &cmd], 4000, "stop");
    }

    /// 启动: ability 必须是 bm dump -n 的 mainAbility 全名(实测坑)。comp 有值直接用;
    /// 空则现场解析——Android 侧"query-activities 解析组件"的对称物。解析不到就不动(OH 无 monkey)。
    /// 冷启动耗时: OH 无 am start -W 等价物(首帧要另采),如实返 None。
    pub fn launch(&self, pkg: &str, comp: &str) -> Option<i64> {
        if self.frozen() { return None; }
        if pkg.is_empty() { return None; }
        let ability = if comp.is_empty() { self.main_ability_of(pkg) } else { comp.to_string() };
        if ability.is_empty() { return None; }
        self.run_timeout(&["shell", "aa", "start", "-b", pkg, "-a", &ability], 8000, "start");
        None
    }

    fn main_ability_of(&self, bundle: &str) -> String {
        let out = self.run_timeout(&["shell", "bm", "dump", "-n", bundle], 6000, "bm");
        parse_main_ability(&String::from_utf8_lossy(&out))
    }

    /// 送文件上设备(file send 是 hdc 顶层命令)
    fn send(&self, local: &str, remote: &str) {
        self.run_timeout(&["file", "send", local, remote], 6000, "send");
    }

    /// 遥测开局: 脚本上载。hdc shell 本身即 root(真机 id 实测 uid=0),恒 true;
    /// 脚本文件承载整批命令——绕开 hdc 参数按空白切分的通道限制(inputText 实测教训)。
    pub fn telemetry_setup(&self, pkg: &str) -> bool {
        let l1 = format!("{}/_pf_t1.sh", self.tmp);
        let l2 = format!("{}/_pf_t2.sh", self.tmp);
        if std::fs::write(&l1, tele_script_oh(pkg, false)).is_ok() {
            self.send(&l1, "/data/local/tmp/pf_t1.sh");
        }
        if std::fs::write(&l2, tele_script_oh(pkg, true)).is_ok() {
            self.send(&l2, "/data/local/tmp/pf_t2.sh");
        }
        true
    }

    /// 遥测采集: 每脚本一趟 shell;heavy 时两趟。限时失败返已得部分(解析容忍)。
    /// 重量级放宽到 20s: hidumper 服务冷启动首跑实测会超 10s(热跑 4.6s),heavy 每
    /// telemetry_interval 步才付一次,上限不进常规步。
    pub fn telemetry_collect(&self, heavy: bool) -> String {
        let mut out = String::from_utf8_lossy(
            &self.run_timeout(&["shell", "sh", "/data/local/tmp/pf_t1.sh"], 10000, "tele")).to_string();
        if heavy {
            out.push('\n');
            out.push_str(&String::from_utf8_lossy(
                &self.run_timeout(&["shell", "sh", "/data/local/tmp/pf_t2.sh"], 20000, "tele")));
        }
        out
    }

    pub fn telemetry(&self, heavy: bool, _pkg: &str) -> crate::telemetry::Telemetry {
        crate::telemetry::from_oh(&self.telemetry_collect(heavy))
    }

    /// 任意设备 shell 命令(CLI probe/exec 用)。整句一个参数交远端解释。
    pub fn shell(&self, cmd: &str, ms: u64) -> String {
        String::from_utf8_lossy(&self.run_timeout(&["shell", cmd], ms, "cli")).to_string()
    }
}

/// 设备后端统一分发。调用方一律持 Device,方法面与 Adb 逐位同构;选择按 serial 前缀:
/// "hdc:<connect key>" → Hdc(OpenHarmony),其余(含 None)→ Adb。
/// Android 零扰动由构造保证: 不带前缀时行为与旧 Adb 直连一字不差。
pub enum Device {
    Adb(Adb),
    Hdc(Hdc),
}

impl Device {
    pub fn new(serial: Option<String>, tmp: String) -> Self {
        match serial {
            Some(s) if s.starts_with("hdc:") => {
                let key = s["hdc:".len()..].trim().to_string();
                Device::Hdc(Hdc::new(if key.is_empty() { None } else { Some(key) }, tmp))
            }
            other => Device::Adb(Adb::new(other, tmp)),
        }
    }
    /// --freeze-on-done 置位:分发到具体后端。done 定局后调用。
    pub fn set_frozen(&self) {
        match self { Device::Adb(d) => d.set_frozen(), Device::Hdc(d) => d.set_frozen() }
    }
    pub fn health_check(&self, ms: u64) -> bool {
        match self { Device::Adb(d) => d.health_check(ms), Device::Hdc(d) => d.health_check(ms) }
    }
    pub fn revive(&self, emulator_cmd: &str) -> bool {
        match self { Device::Adb(d) => d.revive(emulator_cmd), Device::Hdc(d) => d.revive(emulator_cmd) }
    }
    pub fn els_full(&self, timeout_ms: u64) -> (Vec<Node>, Vec<FullNode>, String, String) {
        match self { Device::Adb(d) => d.els_full(timeout_ms), Device::Hdc(d) => d.els_full(timeout_ms) }
    }
    pub fn foreground_pkg(&self) -> String {
        match self { Device::Adb(d) => d.foreground_pkg(), Device::Hdc(d) => d.foreground_pkg() }
    }
    pub fn sys_state(&self) -> SysState {
        match self { Device::Adb(d) => d.sys_state(), Device::Hdc(d) => d.sys_state() }
    }
    pub fn clipboard(&self) -> Option<String> {
        match self { Device::Adb(d) => d.clipboard(), Device::Hdc(d) => d.clipboard() }
    }
    pub fn launchable_apps(&self) -> Vec<(String, String)> {
        match self { Device::Adb(d) => d.launchable_apps(), Device::Hdc(d) => d.launchable_apps() }
    }
    pub fn screen(&self, out_jpg: &str) -> Option<(i32, i32)> {
        match self { Device::Adb(d) => d.screen(out_jpg), Device::Hdc(d) => d.screen(out_jpg) }
    }
    pub fn quick_thumb(&self, tag: &str) -> Option<Vec<u8>> {
        match self { Device::Adb(d) => d.quick_thumb(tag), Device::Hdc(d) => d.quick_thumb(tag) }
    }
    pub fn size(&self) -> (i32, i32) {
        match self { Device::Adb(d) => d.size(), Device::Hdc(d) => d.size() }
    }
    pub fn tap(&self, x: i32, y: i32) {
        match self { Device::Adb(d) => d.tap(x, y), Device::Hdc(d) => d.tap(x, y) }
    }
    pub fn swipe(&self, x: i32, y: i32, x2: i32, y2: i32) {
        match self { Device::Adb(d) => d.swipe(x, y, x2, y2), Device::Hdc(d) => d.swipe(x, y, x2, y2) }
    }
    pub fn scroll_down(&self) {
        match self { Device::Adb(d) => d.scroll_down(), Device::Hdc(d) => d.scroll_down() }
    }
    pub fn scroll_up(&self) {
        match self { Device::Adb(d) => d.scroll_up(), Device::Hdc(d) => d.scroll_up() }
    }
    pub fn type_text(&self, text: &str) {
        match self { Device::Adb(d) => d.type_text(text), Device::Hdc(d) => d.type_text(text) }
    }
    pub fn back(&self) {
        match self { Device::Adb(d) => d.back(), Device::Hdc(d) => d.back() }
    }
    pub fn home(&self) {
        match self { Device::Adb(d) => d.home(), Device::Hdc(d) => d.home() }
    }
    pub fn force_stop(&self, pkg: &str) {
        match self { Device::Adb(d) => d.force_stop(pkg), Device::Hdc(d) => d.force_stop(pkg) }
    }
    /// 返回冷启动毫秒(Android am start -W 的 WaitTime;OH/monkey 路线采不到为 None)
    pub fn launch(&self, pkg: &str, comp: &str) -> Option<i64> {
        match self { Device::Adb(d) => d.launch(pkg, comp), Device::Hdc(d) => d.launch(pkg, comp) }
    }
    pub fn telemetry_setup(&self, pkg: &str) -> bool {
        match self { Device::Adb(d) => d.telemetry_setup(pkg), Device::Hdc(d) => d.telemetry_setup(pkg) }
    }
    pub fn telemetry(&self, heavy: bool, pkg: &str) -> crate::telemetry::Telemetry {
        match self { Device::Adb(d) => d.telemetry(heavy, pkg), Device::Hdc(d) => d.telemetry(heavy, pkg) }
    }
    pub fn shell(&self, cmd: &str, ms: u64) -> String {
        match self { Device::Adb(d) => d.shell(cmd, ms), Device::Hdc(d) => d.shell(cmd, ms) }
    }
}

/// Android 遥测脚本(设备端 sh 一趟跑完全部数据源,段间哨兵行分隔)。
/// pkg/root 开局烤进脚本;pid 每次现场解析(pidof)。非 root 段自动缺席——采不到留空。
fn tele_script_a(pkg: &str, root: bool, heavy: bool) -> String {
    let mut s = format!(
        "PKG={pkg}\nROOT={}\nset -- $(pidof $PKG 2>/dev/null); PID=$1\n", if root { 1 } else { 0 });
    if !heavy {
        s.push_str(r#"echo "-----PF:pid-----"; echo $PID
echo "-----PF:meminfo-----"; head -48 /proc/meminfo 2>/dev/null
echo "-----PF:psi-----"; cat /proc/pressure/cpu /proc/pressure/memory 2>/dev/null
echo "-----PF:cpuinfo-----"; dumpsys cpuinfo 2>/dev/null | grep -E "^Load:|TOTAL:|$PKG"
echo "-----PF:battery-----"; dumpsys battery 2>/dev/null
echo "-----PF:cpufreq-----"; cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_cur_freq 2>/dev/null
echo "-----PF:thermal-----"; for z in /sys/class/thermal/thermal_zone*; do cat $z/type $z/temp 2>/dev/null; done
echo "-----PF:gfxinfo-----"; if [ -n "$PID" ]; then dumpsys gfxinfo $PKG 2>/dev/null | grep -E "Total frames rendered|Janky frames|th percentile|Number Missed Vsync"; fi
echo "-----PF:topact-----"; dumpsys activity activities 2>/dev/null | grep -m1 topResumedActivity
echo "-----PF:status-----"; if [ -n "$PID" ]; then grep -E "^(VmRSS|VmHWM|Threads):" /proc/$PID/status 2>/dev/null; fi
echo "-----PF:nettcp-----"; cat /proc/net/tcp 2>/dev/null | wc -l; cat /proc/net/tcp6 2>/dev/null | wc -l
echo "-----PF:crashcnt-----"; dumpsys dropbox 2>/dev/null | grep -ci crash
echo "-----PF:anrcnt-----"; dumpsys dropbox 2>/dev/null | grep -ci anr
if [ "$ROOT" = "1" ] && [ -n "$PID" ]; then
echo "-----PF:io-----"; cat /proc/$PID/io 2>/dev/null
echo "-----PF:fd-----"; ls /proc/$PID/fd 2>/dev/null | wc -l; ls -l /proc/$PID/fd 2>/dev/null | grep -c socket
fi
"#);
    } else {
        s.push_str(r#"echo "-----PF:meminfo_app-----"; if [ -n "$PID" ]; then dumpsys meminfo $PKG 2>/dev/null | head -45; fi
echo "-----PF:vsync-----"; dumpsys SurfaceFlinger 2>/dev/null | grep -m2 -E "VSYNC period|refresh-rate"
echo "-----PF:layers-----"; dumpsys SurfaceFlinger --list 2>/dev/null | wc -l
echo "-----PF:wifi-----"; dumpsys wifi 2>/dev/null | grep -m4 -E "mWifiInfo|SSID"
echo "-----PF:diskstats-----"; dumpsys diskstats 2>/dev/null | head -8
echo "-----PF:df-----"; df -k /data 2>/dev/null | tail -1
echo "-----PF:sensors-----"; dumpsys sensorservice 2>/dev/null | grep -m6 -E "active-count|Active sensors|connections"
echo "-----PF:location-----"; dumpsys location 2>/dev/null | head -6
echo "-----PF:gpu-----"; dumpsys gpu 2>/dev/null | head -6
echo "-----PF:batterystats-----"; dumpsys batterystats $PKG 2>/dev/null | head -20
if [ -n "$PID" ]; then
U=$(grep -m1 "^Uid:" /proc/$PID/status 2>/dev/null | cut -f2)
echo "-----PF:netsum-----"; dumpsys netstats detail 2>/dev/null | awk -v u="uid=$U" '$0~u{f=1} f&&match($0,/rb=[0-9]+/){rb+=substr($0,RSTART+3,RLENGTH-3)} f&&match($0,/tb=[0-9]+/){tb+=substr($0,RSTART+3,RLENGTH-3)} END{print "rx:" rb " tx:" tb}'
fi
if [ "$ROOT" = "1" ] && [ -n "$PID" ]; then
echo "-----PF:smaps-----"; head -25 /proc/$PID/smaps_rollup 2>/dev/null
echo "-----PF:procnet-----"; head -6 /proc/$PID/net/dev 2>/dev/null
echo "-----PF:cgroup-----"; head -4 /proc/$PID/cgroup 2>/dev/null; cat /proc/$PID/schedstat 2>/dev/null
echo "-----PF:dmesg-----"; dmesg 2>/dev/null | tail -8
echo "-----PF:tombstones-----"; ls /data/tombstones 2>/dev/null | wc -l
fi
"#);
    }
    s
}

/// OpenHarmony 遥测脚本(shell 即 root,root 层恒采;hidumper 子命令均为真机实测形态)。
/// OH 设备无 awk(实测教训)——聚合一律用 smaps_rollup/hidumper 自带汇总,不依赖文本工具。
fn tele_script_oh(pkg: &str, heavy: bool) -> String {
    let mut s = format!("PKG={pkg}\nset -- $(pidof $PKG 2>/dev/null); PID=$1\n");
    if !heavy {
        s.push_str(r#"echo "-----PF:pid-----"; echo $PID
echo "-----PF:meminfo-----"; head -48 /proc/meminfo 2>/dev/null
echo "-----PF:psi-----"; cat /proc/pressure/cpu /proc/pressure/memory 2>/dev/null
echo "-----PF:cpuusage-----"; hidumper --cpuusage 2>/dev/null | head -10
echo "-----PF:battery-----"; hidumper -s BatteryService -a -i 2>/dev/null | head -30
echo "-----PF:cpufreq-----"; hidumper --cpufreq 2>/dev/null | head -60
echo "-----PF:thermal-----"; for z in /sys/class/thermal/thermal_zone*; do cat $z/type $z/temp 2>/dev/null; done
echo "-----PF:fpscount-----"; hidumper -s RenderService -a fpsCount 2>/dev/null | head -12
echo "-----PF:status-----"; if [ -n "$PID" ]; then grep -E "^(VmRSS|VmHWM|Threads):" /proc/$PID/status 2>/dev/null; fi
echo "-----PF:nettcp-----"; cat /proc/net/tcp 2>/dev/null | wc -l; cat /proc/net/tcp6 2>/dev/null | wc -l
echo "-----PF:faultcnt-----"; hidumper -e --list 2>/dev/null | head -10
if [ -n "$PID" ]; then
echo "-----PF:io-----"; cat /proc/$PID/io 2>/dev/null
echo "-----PF:fd-----"; ls /proc/$PID/fd 2>/dev/null | wc -l; ls -l /proc/$PID/fd 2>/dev/null | grep -c socket
fi
"#);
    } else {
        s.push_str(r#"echo "-----PF:mem_app-----"; if [ -n "$PID" ]; then hidumper --mem $PID 2>/dev/null | head -40; fi
echo "-----PF:gles-----"; hidumper -s RenderService -a gles 2>/dev/null | head -12
echo "-----PF:surface-----"; hidumper -s RenderService -a surface 2>/dev/null | grep -m20 "surface \["
echo "-----PF:storage-----"; hidumper --storage 2>/dev/null | head -15
echo "-----PF:df-----"; df -k /data 2>/dev/null | tail -1
echo "-----PF:net-----"; hidumper --net 2>/dev/null | head -8
echo "-----PF:ipc-----"; hidumper --ipc -a --stat 2>/dev/null | head -40
if [ -n "$PID" ]; then
echo "-----PF:smaps-----"; head -25 /proc/$PID/smaps_rollup 2>/dev/null
echo "-----PF:procnet-----"; head -6 /proc/$PID/net/dev 2>/dev/null
echo "-----PF:cgroup-----"; head -4 /proc/$PID/cgroup 2>/dev/null; cat /proc/$PID/schedstat 2>/dev/null
fi
echo "-----PF:dmesg-----"; dmesg 2>/dev/null | tail -8
"#);
    }
    s
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

/// hdc `uitest dumpLayout` 的 JSON → (els文字层, full全量层, 包名)。纯函数,单测钉行为。
/// 与 parse_dump(Android XML)**同构输出**——下游 fold/探针/断言/吸附/沙盘/里程碑/#20 全部
/// 原样复用,一行不改。OpenHarmony/Android 的差异到此为止,agent 契约层无感。
/// 字段映射(真机 dumpLayout 实测): text|description→t(text优先,description=Android的content-desc),
/// bounds→b(格式 `[x1,y1][x2,y2]` 与Android一字不差,parse_bounds 复用), id→id(OH无包名冒号前缀,原样),
/// type→class(Text/Image/Flex/RelativeContainer…,担当 fold 的"同class兄弟"结构折叠),
/// clickable/scrollable/checkable/checked 同义, JSON嵌套层级→depth(天然文档序,比XML深度栈更直接)。
/// 两个已验缺口: ①description(无文字图标的 t 来源)App常留空——与Android的content-desc同病,
///   fold 的 icon 行(clickable+id+无文字)本就为此兜底; ②bundleName 极稀疏(dumpLayout 默认合并
///   多窗口,常只在窗口根出现、且可能混入桌面进程)——取先序首个非空作代表,前台包名的权威来源
///   是 SysState(hdc 侧 aa/hidumper 单采),树内包名只作 els_pkg 辅助对质。
pub fn parse_layout_json(json: &str) -> (Vec<Node>, Vec<FullNode>, String) {
    let mut els: Vec<Node> = Vec::new();
    let mut full: Vec<FullNode> = Vec::new();
    let mut pkg = String::new();
    let root: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return (els, full, pkg),
    };
    fn walk(x: &serde_json::Value, depth: u32,
            els: &mut Vec<Node>, full: &mut Vec<FullNode>, pkg: &mut String) {
        let a = &x["attributes"];
        let get = |k: &str| a[k].as_str().unwrap_or("");
        if pkg.is_empty() {
            let bn = get("bundleName");
            if !bn.is_empty() { *pkg = bn.to_string(); }
        }
        let text = get("text");
        let t = if !text.trim().is_empty() { text } else { get("description") };
        let t = t.trim();
        if let Some(b) = parse_bounds(get("bounds")) {
            if b[2] > b[0] && b[3] > b[1] {
                if !t.is_empty() && els.len() < 70 {
                    let mut tt = t.to_string();
                    while tt.chars().count() > 40 { tt.pop(); }
                    els.push(Node { t: tt, b });
                }
                let id = { let s = get("id"); if s.is_empty() { None } else { Some(s.to_string()) } };
                full.push(FullNode {
                    t: t.to_string(), b, id,
                    class: get("type").to_string(),
                    clickable: get("clickable") == "true",
                    scrollable: get("scrollable") == "true",
                    checkable: get("checkable") == "true",
                    checked: get("checked") == "true",
                    depth,
                });
            }
        }
        if let Some(ch) = x["children"].as_array() {
            for c in ch { walk(c, depth + 1, els, full, pkg); }
        }
    }
    walk(&root, 0, &mut els, &mut full, &mut pkg);
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

/// OH `aa dump -a` → 前台 (bundle, ability全名)。纯函数供单测。
/// AbilityRecord 块内 "bundle name [x]"/"main name [y]" 先于 "state #FOREGROUND" 行出现,
/// 顺序扫描、见 FOREGROUND 即提交最近一对;解析不到返回空对(SysState 契约容忍空值)。
/// 真机格式成色属软点①,冒烟局验证后如有出入只改这一个函数。
pub fn parse_aa_dump(s: &str) -> (String, String) {
    let (mut bundle, mut ability) = (String::new(), String::new());
    for line in s.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("bundle name [") {
            bundle = v.trim_end_matches(']').to_string();
        } else if let Some(v) = l.strip_prefix("main name [") {
            ability = v.trim_end_matches(']').to_string();
        } else if l.contains("state #FOREGROUND") && !bundle.is_empty() {
            return (bundle, ability);
        }
    }
    (String::new(), String::new())
}

/// OH `bm dump -a` → bundle 名列表。纯函数供单测。
/// 输出按行给 bundle 名,可能混标头("OK!"/"ID: 100"等): 只收形如反域名的整行 token,去重排序。
pub fn parse_bm_bundles(s: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for line in s.lines() {
        let l = line.trim();
        if !l.is_empty() && l.contains('.')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            && !v.iter().any(|p| p == l)
        {
            v.push(l.to_string());
        }
    }
    v.sort();
    v
}

/// OH `bm dump -n <bundle>` → 主入口 ability 全名。纯函数供单测。
/// 老版字段 "mainAbility"/"mainElementName" 优先;新版 bundleInfo(真机 API20 实测)
/// 两个键都没有 → 启发式兜底: 扫 "name" 值,取首个以 MainAbility/EntryAbility/
/// MainActivity 结尾的(真机 settings 的 abilities 首项是 OobeLocalUserTestAbility,
/// 靠"取第一个"会拿错,后缀匹配才对)。输出混非 JSON 标头行,不整体反序列化。
pub fn parse_main_ability(s: &str) -> String {
    for key in ["\"mainAbility\"", "\"mainElementName\""] {
        let mut from = 0;
        while let Some(i) = s[from..].find(key) {
            let rest = &s[from + i + key.len()..];
            if let Some(q1) = rest.find('"') {
                if let Some(q2) = rest[q1 + 1..].find('"') {
                    let val = &rest[q1 + 1..q1 + 1 + q2];
                    if !val.is_empty() { return val.to_string(); }
                }
            }
            from += i + key.len();
        }
    }
    let mut from = 0;
    while let Some(i) = s[from..].find("\"name\"") {
        let rest = &s[from + i + 6..];
        if let Some(q1) = rest.find('"') {
            if let Some(q2) = rest[q1 + 1..].find('"') {
                let val = &rest[q1 + 1..q1 + 1 + q2];
                for suf in ["MainAbility", "EntryAbility", "MainActivity"] {
                    if val.ends_with(suf) {
                        return val.to_string();
                    }
                }
            }
        }
        from += i + 6;
    }
    String::new()
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

    const OH_HOME: &str = include_str!("testdata/oh_home.json");

    #[test]
    fn oh_layout_parses_isomorphic_to_android() {
        // hdc dumpLayout(真机OpenHarmony) 与 parse_dump 同构: 下游零改动的地基
        let (els, full, pkg) = parse_layout_json(OH_HOME);
        assert!(!full.is_empty() && !els.is_empty(), "真机dump应解出节点");
        // 文字节点提取: 桌面图标名进 els
        for name in ["设置", "图库", "电话", "信息", "相机"] {
            assert!(els.iter().any(|n| n.t == name), "桌面文字'{name}'应在els");
        }
        // 无文字图标: clickable 无文字容器只进 full 不进 els(fold 靠 clickable+id 兜出 icon 行)
        let icons: Vec<&FullNode> = full.iter()
            .filter(|f| f.clickable && f.t.is_empty() && f.id.is_some()).collect();
        assert!(!icons.is_empty(), "桌面应有无文字可点图标节点");
        // bounds 格式与 Android 一字不差: 正面积、坐标合理
        assert!(full.iter().all(|f| f.b[2] >= f.b[0] && f.b[3] >= f.b[1]));
        // type→class 担当结构折叠(桌面图标是重复的 RelativeContainer)
        assert!(full.iter().filter(|f| f.class == "RelativeContainer").count() >= 3,
            "桌面重复图标容器应被识别(fold 同class兄弟折叠的输入)");
        // depth 由 JSON 嵌套天然给出,子深于父
        let settings = full.iter().find(|f| f.t == "设置").unwrap();
        assert!(settings.depth > 0);
    }

    #[test]
    fn oh_layout_els_contract_matches_parse_dump() {
        // els 契约与 Android 逐条对齐: 40字截断、70条上限、有文字才收
        let long_desc = "占".repeat(50);
        let json = format!(r#"{{"attributes":{{"bounds":"[0,0][10,10]"}},"children":[
            {{"attributes":{{"text":"{long_desc}","bounds":"[0,0][100,50]","type":"Text"}}}},
            {{"attributes":{{"text":"","description":"关闭","bounds":"[90,0][100,10]","type":"Image","clickable":"true","id":"closeBtn"}}}},
            {{"attributes":{{"text":"","bounds":"[0,0][10,10]","type":"Flex","scrollable":"true"}}}}
        ]}}"#);
        let (els, full, _) = parse_layout_json(&json);
        assert!(els.iter().all(|n| n.t.chars().count() <= 40), "40字截断与Android一致");
        assert_eq!(els.iter().find(|n| n.t.starts_with('占')).map(|n| n.t.chars().count()), Some(40));
        // description 兜 t(无文字图标): text空时取description
        let icon = full.iter().find(|f| f.id.as_deref() == Some("closeBtn")).unwrap();
        assert_eq!(icon.t, "关闭", "text空则description补位(=content-desc语义)");
        assert!(icon.clickable && icon.t.chars().count() > 0);
        // 无文字无desc的可滚容器: 只进full不进els
        assert!(full.iter().any(|f| f.scrollable && f.t.is_empty()));
    }

    #[test]
    fn oh_layout_bad_json_yields_empty() {
        // 坏JSON/空dump → 全空(空表是合法观测,与 dump_xml 失败同待遇)
        let (e, f, p) = parse_layout_json("not json{");
        assert!(e.is_empty() && f.is_empty() && p.is_empty());
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
    fn adb_search_order_spec() {
        // ADB_BIN 最高;仓库自带 platform-tools 胜过 PATH 与系统安装点;PATH 哨兵在系统目录前
        let v = adb_search_order(Some("/x/adb"), Some("/Users/u"));
        assert_eq!(v[0], "/x/adb");
        assert_eq!(v[1], "platform-tools/adb");
        assert_eq!(v[2], "PATH");
        assert!(v[3].starts_with("/Users/u/Library/Android"));
        assert!(v.last().unwrap().contains("homebrew"));
        let v2 = adb_search_order(None, None);
        assert_eq!(v2[0], "platform-tools/adb", "无显式指定时仓库自带最先");
        assert!(adb_search_order(Some("  "), None)[0] == "platform-tools/adb", "空白 ADB_BIN 忽略");
    }

    #[test]
    fn device_dispatch_by_serial_prefix() {
        // "hdc:" 前缀走 Hdc,其余(真机serial/None)走 Adb——Android 零扰动的地基
        assert!(matches!(Device::new(Some("hdc:5ce1227d".into()), "/tmp".into()), Device::Hdc(_)));
        assert!(matches!(Device::new(Some("emulator-5554".into()), "/tmp".into()), Device::Adb(_)));
        assert!(matches!(Device::new(None, "/tmp".into()), Device::Adb(_)));
    }

    #[test]
    fn aa_dump_foreground_record() {
        // 属性行先于 state 行: 见 FOREGROUND 提交最近一对,BACKGROUND 记录跳过
        let s = "User ID #100\n  AbilityRecord ID #4\n    app name [com.ohos.settings]\n    main name [com.ohos.settings.MainAbility]\n    bundle name [com.ohos.settings]\n    state #BACKGROUND  start time [123]\n  AbilityRecord ID #7\n    app name [com.example.hello]\n    main name [EntryAbility]\n    bundle name [com.example.hello]\n    state #FOREGROUND  start time [456]\n";
        let (b, a) = parse_aa_dump(s);
        assert_eq!(b, "com.example.hello");
        assert_eq!(a, "EntryAbility");
        let (b2, a2) = parse_aa_dump("no records here");
        assert!(b2.is_empty() && a2.is_empty(), "解析不到如实留空,不装");
    }

    #[test]
    fn bm_bundle_list_and_main_ability() {
        let v = parse_bm_bundles("OK!\ncom.example.hello\n\tcom.ohos.sceneboard\nID: 100\ncom.example.hello\n");
        assert_eq!(v, vec!["com.example.hello".to_string(), "com.ohos.sceneboard".to_string()],
            "去重+滤掉非bundle行");
        let j = r#"{ "name": "com.x", "mainAbility": "", "hapModuleInfos": [ { "mainAbility": "com.x.MainAbility" } ] }"#;
        assert_eq!(parse_main_ability(j), "com.x.MainAbility", "空值跳过,取第一个非空");
        assert_eq!(parse_main_ability("{}"), "");
        // 新版 bundleInfo(API20 真机): 无 mainAbility 键 → 启发式后缀匹配。
        // settings 实测 abilities 首项是 OobeLocalUserTestAbility,取第一个会拿错
        let j2 = r#"{ "name": "com.ohos.settings", "abilities": [
            { "name": "OobeLocalUserTestAbility" },
            { "name": "com.ohos.settings.ExternalWifiSettingsAbility" },
            { "name": "com.ohos.settings.MainAbility" } ] }"#;
        assert_eq!(parse_main_ability(j2), "com.ohos.settings.MainAbility",
            "无 mainAbility 键时按 MainAbility/EntryAbility/MainActivity 后缀兜底");
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

    // --freeze-on-done 行为单测: 冻结后语义动作(手势/键/force-stop/launch)一律不得下发
    // 任何设备命令(不 spawn 外部进程),只放行观测。对应 AW_RIG_SPEC §1 验收"done 后无 act"。
    #[test]
    fn frozen_blocks_device_writes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pf_freeze_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("touched");
        let fake = dir.join("fakeadb.sh");
        // 假 adb: 无论收到什么参数都 touch 标记文件——被调用即留痕
        std::fs::write(&fake, format!("#!/bin/sh\ntouch \"{}\"\n", marker.display())).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let d = Adb {
            bin: fake.to_string_lossy().into_owned(),
            serial: None,
            tmp: dir.to_string_lossy().into_owned(),
            frozen: AtomicBool::new(false),
        };
        assert!(!d.frozen(), "新建默认不冻结");
        d.set_frozen();
        assert!(d.frozen(), "set_frozen 后冻结");

        // 冻结后所有语义写动作都不得 spawn(marker 永不出现)
        d.tap(1, 2);
        d.swipe(0, 0, 1, 1);
        d.scroll_down();
        d.type_text("x");
        d.back();
        d.home();
        d.force_stop("com.example");
        assert_eq!(d.launch("com.example", "com.example/.Main"), None, "冻结后 launch 直接 None");
        assert!(!marker.exists(), "冻结后任何语义动作都不应下发设备命令");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
