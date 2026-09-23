//! fleet: 一次把农场里每台手机的状态读齐, 输出一份**只读**快照。
//!
//! 为什么要有这条命令: `devices` 只给序列号, `keepalive --status` 只看亮屏与不息屏策略,
//! 电量、温度、风扇、前台应用分散在各自的通路里。上层 (game_opt_loop 的面板) 要在
//! 一个页面上摆出"每台手机现在什么样", 需要的是**一次调用拿齐**的一份 JSON。
//!
//! 三条纪律:
//!
//! 1. **只读**。整条通路只有 `cat` / `getprop` / `dumpsys` / `settings get` 这些读命令,
//!    不写任何设备状态 —— 与 `keepalive --status` 同一条纪律。唯一有副作用的是可选截图,
//!    而截图受设备锁与最小间隔双重约束 (见 [`Screens`])。
//! 2. **一台一趟**。所有读操作打包成一段设备端 sh 脚本, 用 `-----PF:<段名>-----` 哨兵分段,
//!    一次 shell 往返带回全部数据源 (与 `telemetry` 的遥测脚本同一套做法)。
//!    正在跑测试的手机上, 多一次 adb 往返就是多一次噪声, 能合并的必须合并。
//! 3. **读不到就留空**。每个字段都是 `Option`, 采不到记 `None` 并在 `unknown` 里写清楚
//!    是哪一项、为什么。不拿 0 或空串冒充读数 —— 面板据此显示「未知」。
//!
//! 设备锁不是 phonefarm 的东西, 是农场上大家共用的一个文件锁 (`devlock`)。
//! 这里**只读它的三个文件**, 不去 exec 那个脚本, 更不会去抢锁。
//!
//! # 别人占着手机的时候读什么
//!
//! 「只读」不等于「零影响」。一次 `dumpsys window` 会在设备上拉起一个进程,
//! 而对面可能正在量 30 秒的帧时 —— 一个 CPU 尖峰就够多出一帧 jank。
//! 所以探测分两档, 按设备锁自动选:
//!
//! - **锁空闲 / 锁是自己的 / 心跳已超期** → 完整档: getprop + dumpsys + settings get。
//! - **锁被别人持有** → 轻量档: 只 `cat` 几个 sysfs 节点 (温度、电量、风扇、uptime)。
//!   读文件不拉进程, 是这台机器正在跑测试时仍然安全的那部分。
//!   dumpsys 才拿得到的字段 (前台应用、亮屏、保活策略) 照常留空并写明原因。
//!
//! `--probe-while-locked` 可以强制走完整档 —— 它会干扰对面的测量, 要用请自己清楚。

use crate::device::Device;
use crate::hwcond;
use crate::telemetry;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime};

/// 设备锁默认所在目录。可用 `--lock-dir` 或 `DEVLOCK_DIR` 覆盖 ——
/// 这个路径是本机农场的约定, 不是 phonefarm 的一部分。
const DEFAULT_LOCK_DIR: &str = "/private/tmp/claude-501";

/// `devlock` 脚本认为心跳多久没动就算废弃 (它的 `STALE` 缺省值)。
const LOCK_STALE_SECS: u64 = 600;

/// 单台设备的探测超时。一趟 shell 里塞了十几个 dumpsys, 给足时间,
/// 但必须有上限: 掉线的设备会把 adb 卡到天荒地老。
const DEFAULT_TIMEOUT_MS: u64 = 12_000;

/// 截图的最小间隔。默认拉得很长 —— 截图是这条只读通路上唯一有副作用的动作。
const DEFAULT_SCREENSHOT_EVERY_S: u64 = 300;

// ══════════════ 输出形状 ══════════════

#[derive(Debug, Serialize)]
pub struct Fleet {
    pub schema: &'static str,
    pub tool: &'static str,
    pub version: &'static str,
    pub generated_at: String,
    pub backends: Backends,
    /// 农场共用的设备锁。`None` = 没配锁目录或读不到。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock: Option<Lock>,
    pub devices: Vec<Card>,
    /// 整体层面的说明 (某一族工具不在 PATH 之类)。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Backends {
    /// adb 可执行文件路径; `None` = 不在 PATH, 这一族整体跳过。
    pub adb: Option<String>,
    pub hdc: bool,
}

#[derive(Debug, Serialize)]
pub struct Lock {
    pub dir: String,
    /// `None` = 锁是空的, 现在没人占着手机。
    pub held_by: Option<String>,
    pub since: Option<String>,
    /// 心跳距今多少秒 (= owner 文件 mtime 距今)。
    pub heartbeat_age_s: Option<u64>,
    /// 心跳超期 → 这把锁可以被别人接管了。
    pub stale: bool,
    pub queue: Vec<String>,
    pub recent: Vec<LockEvent>,
}

#[derive(Debug, Serialize)]
pub struct LockEvent {
    pub at: String,
    pub action: String,
    pub label: String,
}

/// 一台手机一张卡片。
#[derive(Debug, Serialize)]
pub struct Card {
    pub serial: String,
    /// `adb` | `hdc`
    pub transport: &'static str,
    /// adb 自报的状态原文: `device` / `offline` / `unauthorized` / `no permissions` …
    pub state: String,
    pub online: bool,
    pub model: Option<String>,
    pub brand: Option<String>,
    /// 人能读的一行系统版本, 例如 `Android 16 (SDK 36)`。
    pub os: Option<String>,
    /// 能不能拿到 root。`None` = 没探 (设备不在线)。
    pub root: Option<bool>,
    pub battery: Battery,
    pub thermal: Thermal,
    pub fan: Fan,
    pub screen_on: Option<bool>,
    pub foreground: Option<String>,
    pub keepalive: Keepalive,
    /// 开机到现在多少秒。
    pub uptime_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<Screenshot>,
    /// 这一轮走的是哪一档: `full` (getprop+dumpsys) / `light` (只读 sysfs) / `none` (设备不在线)。
    ///
    /// 必须落进输出: 同一个字段在两档下的含义不同 (轻量档没有前台应用),
    /// 面板上"这项是空的"到底是"读不到"还是"这一轮没读", 得分得清。
    pub probe_depth: &'static str,
    /// 这一台探了多久 (毫秒) —— 慢下来通常意味着设备正忙。
    pub probe_ms: u64,
    /// 哪些项没采到、为什么。面板直接把它显示出来。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Battery {
    pub level_pct: Option<i32>,
    /// `Charging` / `Discharging` / `Not charging` / `Full` / `Unknown`
    pub status: Option<String>,
    /// 插着线没有。`None` = 判不出来。
    pub plugged: Option<bool>,
    pub temp_c: Option<f32>,
    pub voltage_mv: Option<i32>,
    /// 整机是不是真由电池供电 —— 只有 true 时功耗才可比 (口径同 `hwcond::BatteryState`)。
    pub on_battery: Option<bool>,
}

#[derive(Debug, Default, Serialize)]
pub struct Thermal {
    /// SoC 结温最高的热区 (只看 cpu-/cpullc/gpuss 前缀, 口径同 `hwcond::soc_max_c`)。
    pub soc_max_c: Option<f64>,
    pub soc_zone: Option<String>,
    pub gpu_c: Option<f64>,
    pub battery_c: Option<f64>,
    /// 一共看见几个热区 —— 0 个通常意味着这台机器不给读。
    pub zones: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct Fan {
    /// 这台机器有没有能读的风扇节点。false = 没有 (不是"风扇关着")。
    pub supported: bool,
    pub enabled: Option<bool>,
    pub speed_level: Option<i64>,
    pub node: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Keepalive {
    pub screen_off_timeout: Option<String>,
    pub stay_on_while_plugged_in: Option<String>,
    /// 不息屏策略到位没有 (口径与 `keepalive --status` 一致)。
    pub policy_ok: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct Screenshot {
    pub path: String,
    pub at: String,
    pub age_s: u64,
    /// 这一轮是新截的, 还是沿用上一张。
    pub fresh: bool,
}

// ══════════════ 纯解析 (单测全打在这一层) ══════════════

/// `adb devices -l` → (serial, state, 附带的 model)。
///
/// 必须带 `state`: `offline` / `unauthorized` 的设备也要出现在卡片里, 否则
/// 面板上它就直接消失了 —— 而"插着但连不上"恰恰是最需要看见的一种状态。
pub fn parse_adb_devices_l(text: &str) -> Vec<AdbEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("List of devices") || line.starts_with('*') {
            continue;
        }
        let mut it = line.split_whitespace();
        let Some(serial) = it.next() else { continue };
        let Some(state) = it.next() else { continue };
        // `no permissions (...)` 这种带空格的状态, 后面的词也是状态的一部分,
        // 但 model:xxx 这类键值对不是。
        let mut state = state.to_string();
        let mut model = None;
        for tok in it {
            if let Some(m) = tok.strip_prefix("model:") {
                model = Some(m.to_string());
            } else if !tok.contains(':') {
                state.push(' ');
                state.push_str(tok);
            }
        }
        out.push((serial.to_string(), state, model));
    }
    out
}

/// `hdc list targets` → connect key 集 (滤掉空行与 `[Empty]` / `[Fail...]` 哨兵)。
pub fn parse_hdc_targets(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "[Empty]" && !l.starts_with("[Fail"))
        .map(str::to_string)
        .collect()
}

/// `dumpsys battery` 的充电状态。
///
/// `status:` 是 `BatteryManager.BATTERY_STATUS_*` 的数字码 ——
/// 2=充电中 3=放电中 4=没在充 5=充满。别把 4 (Not charging) 当成放电:
/// 停充之后内核常常写的就是 4, 而那时整机确实在吃电池。
pub fn parse_battery_status_a(text: &str) -> (Option<String>, Option<bool>) {
    let mut status = None;
    let mut plugged = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("status:") {
            status = match v.trim().parse::<i32>() {
                Ok(2) => Some("Charging"),
                Ok(3) => Some("Discharging"),
                Ok(4) => Some("Not charging"),
                Ok(5) => Some("Full"),
                Ok(1) => Some("Unknown"),
                _ => None,
            }
            .map(str::to_string);
        }
        for key in ["AC powered:", "USB powered:", "Wireless powered:", "Dock powered:"] {
            if let Some(v) = l.strip_prefix(key) {
                let on = v.trim() == "true";
                plugged = Some(plugged.unwrap_or(false) || on);
            }
        }
    }
    (status, plugged)
}

/// 热区里挑 GPU 那条 (高通是 `gpuss-*`, 其它厂商常见 `gpu` 字样)。
pub fn gpu_c(zones: &[(String, i64)]) -> Option<f64> {
    zones
        .iter()
        .filter(|(ty, t)| {
            let ty = ty.to_ascii_lowercase();
            (ty.starts_with("gpuss") || ty.contains("gpu")) && *t > 0 && *t < 100_000
        })
        .max_by_key(|(_, t)| *t)
        .map(|(_, t)| *t as f64 / 1000.0)
}

/// 热区里挑电池那条。
pub fn battery_c(zones: &[(String, i64)]) -> Option<f64> {
    zones
        .iter()
        .find(|(ty, t)| ty.to_ascii_lowercase().contains("battery") && *t > 0 && *t < 100_000)
        .map(|(_, t)| *t as f64 / 1000.0)
}

/// `FAN <节点路径>=<值>` 行 → 风扇状态。
///
/// 节点读不到时值是空的 —— 那说明这台机器没有这个节点 (或没 root),
/// 要报 `supported: false`, **不能**报"风扇关着"。这两件事在读数据的人看来完全不同。
pub fn parse_fan(text: &str) -> Fan {
    let mut fan = Fan::default();
    for line in text.lines() {
        let Some((path, value)) = line.trim().split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if path.ends_with("fan_enable") {
            fan.supported = true;
            fan.node = Some(path.to_string());
            fan.enabled = match value {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            };
        } else if path.ends_with("fan_speed_level") {
            fan.supported = true;
            fan.speed_level = value.parse().ok();
        }
    }
    fan
}

/// `dumpsys power | grep mWakefulness=` → 亮屏没有。
pub fn parse_wakefulness(text: &str) -> Option<bool> {
    let i = text.find("mWakefulness=")?;
    let v: String = text[i + "mWakefulness=".len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    match v.as_str() {
        "Awake" => Some(true),
        "Asleep" | "Dozing" | "Dreaming" => Some(false),
        _ => None,
    }
}

/// `dumpsys window | grep mCurrentFocus` → 前台包名。
///
/// 与 `device::Adb::foreground_pkg` 同一套取法, 只是这里吃的是已经拿回来的文本
/// (省一趟往返)。桌面/锁屏时焦点窗口不是应用, 取不出包名就如实 `None`。
pub fn parse_foreground(text: &str) -> Option<String> {
    for key in ["mCurrentFocus", "mFocusedApp"] {
        for line in text.lines().filter(|l| l.contains(key)) {
            let Some(i) = line.find(" u0 ") else { continue };
            let tok: String = line[i + 4..]
                .chars()
                .take_while(|c| *c != '/' && *c != '}' && *c != ' ')
                .collect();
            if tok.contains('.')
                && tok
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            {
                return Some(tok);
            }
        }
    }
    None
}

/// `/proc/uptime` 的第一个数 → 开机秒数。
pub fn parse_uptime_s(text: &str) -> Option<u64> {
    text.split_whitespace()
        .next()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// 不息屏策略到位没有 —— 口径与 `keepalive --status` 逐字一致。
///
/// `screen_off_timeout` 必须是 int32 上限 (两族都没有真正的"永不"值),
/// `stay_on_while_plugged_in` 非 0 非 null 即可。
pub fn policy_ok_android(timeout: &str, stayon: &str) -> bool {
    let so = stayon.trim();
    timeout.trim() == "2147483647" && !so.is_empty() && so != "0" && so != "null"
}

/// `su -c 'id -u'` 的回包里有没有 uid 0。
///
/// 只认干净的 `0`: 没有 su 的机器回的是 `su: not found` 之类, 里面也可能带数字。
pub fn parse_root(text: &str) -> bool {
    text.lines().any(|l| l.trim() == "0")
}

/// 一行 `getprop` 回包: 空串与 `unknown` 都算没读到。
fn prop(text: &str, n: usize) -> Option<String> {
    let v = text.lines().nth(n)?.trim();
    if v.is_empty() || v == "unknown" {
        None
    } else {
        Some(v.to_string())
    }
}

/// 解析 owner 文件: `"sysknob-loop since 2026-09-23 10:12:31"`。
pub fn parse_lock_owner(text: &str) -> Option<(String, Option<String>)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match text.split_once(" since ") {
        Some((label, since)) => Some((label.trim().to_string(), Some(since.trim().to_string()))),
        None => Some((text.split_whitespace().next()?.to_string(), None)),
    }
}

/// 解析一行锁日志: `"2026-09-23 09:16:41 acquire sysknob-loop"`。
///
/// `steal` 行长这样: `steal by <新主> from [<旧主>] idle 900s` —— 要跳过 "by"。
pub fn parse_lock_event(line: &str) -> Option<LockEvent> {
    let mut parts = line.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    let action = parts.next()?;
    if !matches!(action, "acquire" | "release" | "steal") {
        return None;
    }
    let mut label = parts.next()?;
    if action == "steal" && label == "by" {
        label = parts.next()?;
    }
    Some(LockEvent {
        at: format!("{date} {time}"),
        action: action.to_string(),
        label: label.to_string(),
    })
}

// ══════════════ 设备端脚本 ══════════════

/// Android 一趟读齐的脚本。段名与 `telemetry::split_sections` 的哨兵约定一致。
///
/// 全是读命令。`su -c` 一律带 `2>/dev/null` 兜底: 没 root 的机器上它只是空输出,
/// 不该让整段脚本报错。
fn probe_script_android() -> &'static str {
    r#"echo "-----PF:props-----"; getprop ro.product.model; getprop ro.product.brand; getprop ro.build.version.release; getprop ro.build.version.sdk
echo "-----PF:root-----"; su -c 'id -u' 2>/dev/null
echo "-----PF:uptime-----"; cat /proc/uptime 2>/dev/null
echo "-----PF:battery-----"; dumpsys battery 2>/dev/null
echo "-----PF:thermal-----"; for z in /sys/class/thermal/thermal_zone*; do echo "$(cat $z/type 2>/dev/null) $(cat $z/temp 2>/dev/null)"; done
echo "-----PF:power-----"; dumpsys power 2>/dev/null | grep -m1 mWakefulness=
echo "-----PF:focus-----"; dumpsys window 2>/dev/null | grep -m2 -E 'mCurrentFocus|mFocusedApp'
echo "-----PF:ka-----"; settings get system screen_off_timeout 2>/dev/null; settings get global stay_on_while_plugged_in 2>/dev/null
echo "-----PF:fan-----"; for f in /sys/kernel/fan/fan_enable /sys/kernel/fan/fan_speed_level; do echo "$f=$(cat $f 2>/dev/null || su -c "cat $f" 2>/dev/null)"; done
echo "-----PF:end-----""#
}

/// Android 轻量档: 只读 sysfs, 不拉任何 dumpsys 进程。
///
/// 别人正占着这台手机时走这一档。`cat` 一个 sysfs 节点是一次 VFS 读,
/// 对正在跑的帧时测量不构成可测量的干扰; `dumpsys` 不是。
fn probe_script_android_light() -> &'static str {
    r#"echo "-----PF:props-----"; getprop ro.product.model; getprop ro.product.brand; getprop ro.build.version.release; getprop ro.build.version.sdk
echo "-----PF:uptime-----"; cat /proc/uptime 2>/dev/null
echo "-----PF:sysbat-----"; for f in capacity status temp current_now voltage_now; do echo "$f=$(cat /sys/class/power_supply/battery/$f 2>/dev/null)"; done
echo "-----PF:thermal-----"; for z in /sys/class/thermal/thermal_zone*; do echo "$(cat $z/type 2>/dev/null) $(cat $z/temp 2>/dev/null)"; done
echo "-----PF:fan-----"; for f in /sys/kernel/fan/fan_enable /sys/kernel/fan/fan_speed_level; do echo "$f=$(cat $f 2>/dev/null || su -c "cat $f" 2>/dev/null)"; done
echo "-----PF:end-----""#
}

/// `/sys/class/power_supply/battery/*` 的 `键=值` 行 → 电池。
///
/// 这里的 `temp` 是**分摄氏度** (400 = 40.0 ℃), 与 `dumpsys battery` 的
/// `temperature` 同一个量纲; `current_now` 是微安, 正=在充。
pub fn parse_sysfs_battery(text: &str) -> Battery {
    let mut b = Battery::default();
    let mut current_ua: Option<i64> = None;
    for line in text.lines() {
        let Some((k, v)) = line.trim().split_once('=') else { continue };
        let v = v.trim();
        if v.is_empty() { continue; }
        match k {
            "capacity" => b.level_pct = v.parse().ok(),
            "status" => b.status = Some(v.to_string()),
            "temp" => b.temp_c = v.parse::<f32>().ok().map(|t| t / 10.0),
            "current_now" => current_ua = v.parse().ok(),
            "voltage_now" => b.voltage_mv = v.parse::<i64>().ok().map(|uv| (uv / 1000) as i32),
            _ => {}
        }
    }
    // 口径同 hwcond::BatteryState::on_battery: 两个判据都要看 ——
    // status 在停充后有的内核仍写 "Not charging", 而 current_now 在充放平衡时会过零。
    b.on_battery = match (&b.status, current_ua) {
        (Some(s), _) if s.eq_ignore_ascii_case("Charging") => Some(false),
        (_, Some(c)) if c < 0 => Some(true),
        (_, Some(_)) => Some(false),
        _ => None,
    };
    b
}

/// OpenHarmony 一趟读齐的脚本。
///
/// OH 侧能读的比安卓少, 而且本机没有 OH 真机可验 —— 这里只放公开可读的几项,
/// 采不到的字段照常留空并进 `unknown`。**不要**按安卓的字段名硬套 OH 的输出。
fn probe_script_oh() -> &'static str {
    r#"echo "-----PF:props-----"; param get const.product.model 2>/dev/null; param get const.product.brand 2>/dev/null; param get const.ohos.fullname 2>/dev/null; echo ""
echo "-----PF:root-----"; id -u 2>/dev/null
echo "-----PF:uptime-----"; cat /proc/uptime 2>/dev/null
echo "-----PF:battery-----"; hidumper -s BatteryService -a -i 2>/dev/null
echo "-----PF:thermal-----"; for z in /sys/class/thermal/thermal_zone*; do echo "$(cat $z/type 2>/dev/null) $(cat $z/temp 2>/dev/null)"; done
echo "-----PF:screen-----"; hidumper -s RenderService -a screen 2>/dev/null
echo "-----PF:end-----""#
}

/// OH 的 `hidumper -s RenderService -a screen` → 亮屏没有。
pub fn parse_hdc_power_on(text: &str) -> Option<bool> {
    let i = text.find("powerStatus=POWER_STATUS_")?;
    let v: String = text[i + "powerStatus=POWER_STATUS_".len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    match v.as_str() {
        "ON" => Some(true),
        "OFF" => Some(false),
        _ => None,
    }
}

// ══════════════ 组装 ══════════════

fn sec<'a>(ss: &'a [(String, String)], k: &str) -> Option<&'a str> {
    ss.iter()
        .find(|(key, v)| key == k && !v.trim().is_empty())
        .map(|(_, v)| v.as_str())
}

/// 把一台设备的一趟回包拼成卡片。纯函数 —— 真机回包存成夹具就能单测。
pub fn card_from_android(serial: &str, state: &str, raw: &str, probe_ms: u64, depth: &'static str) -> Card {
    let ss = telemetry::split_sections(raw);
    let mut unknown = Vec::new();

    let props = sec(&ss, "props").unwrap_or_default();
    let model = prop(props, 0);
    let brand = prop(props, 1);
    let release = prop(props, 2);
    let sdk = prop(props, 3);
    let os = release.map(|r| match &sdk {
        Some(s) => format!("Android {r} (SDK {s})"),
        None => format!("Android {r}"),
    });
    if model.is_none() {
        unknown.push("型号: getprop ro.product.model 没回值".into());
    }

    // 轻量档没探 root —— 那要 `su -c`, 会在别人正跑测试的机器上多拉一个进程。
    // 没探就是 None, 不能默认成 false: "没 root" 与 "没查" 是两回事。
    let root = sec(&ss, "root").map(parse_root);

    let mut battery = Battery::default();
    if let Some(b) = sec(&ss, "sysbat") {
        // 轻量档: 电池来自 sysfs, 没有 dumpsys 的 "AC/USB powered" 那几行
        battery = parse_sysfs_battery(b);
    } else if let Some(b) = sec(&ss, "battery") {
        let (level, mv, temp) = telemetry::parse_battery_a(b);
        let (status, plugged) = parse_battery_status_a(b);
        battery.level_pct = level;
        battery.voltage_mv = mv;
        battery.temp_c = temp;
        battery.on_battery = match (&status, plugged) {
            // 口径同 hwcond::BatteryState::on_battery: 充电中一定不是放电态;
            // 没插线且没在充, 才算整机靠电池。"Not charging" 且插着线判不出来。
            (Some(s), _) if s == "Charging" || s == "Full" => Some(false),
            (_, Some(false)) => Some(true),
            _ => None,
        };
        battery.status = status;
        battery.plugged = plugged;
    } else {
        unknown.push("电池: dumpsys battery 没回值".into());
    }
    if depth == "light" {
        unknown.push(
            "前台应用 / 亮屏 / 保活策略: 设备锁被别人持有, 这一轮只读了 sysfs, \
没跑 dumpsys (跑了会干扰对面正在做的测量)"
                .into(),
        );
    }

    let mut thermal = Thermal::default();
    if let Some(t) = sec(&ss, "thermal") {
        let zones = hwcond::parse_thermal(t);
        thermal.zones = zones.len();
        if let Some((zone, c)) = hwcond::soc_max_c(&zones) {
            thermal.soc_zone = Some(zone);
            thermal.soc_max_c = Some(c);
        }
        thermal.gpu_c = gpu_c(&zones);
        thermal.battery_c = battery_c(&zones);
        if thermal.gpu_c.is_none() {
            unknown.push("GPU 温度: 热区里没有 gpu 字样的区".into());
        }
    } else {
        unknown.push("温度: /sys/class/thermal 读不到".into());
    }

    let fan = sec(&ss, "fan").map(parse_fan).unwrap_or_default();
    if !fan.supported {
        unknown.push(
            "风扇: /sys/kernel/fan 下的节点读不到 (这台机器没有风扇, 或者需要 root)".into(),
        );
    }

    let screen_on = sec(&ss, "power").and_then(parse_wakefulness);
    // 轻量档根本没跑 dumpsys, 那句"里面没有 mWakefulness"会把人带偏 ——
    // "这一轮没读" 与 "读了但没有" 必须分开说。
    if screen_on.is_none() && depth == "full" {
        unknown.push("亮屏: dumpsys power 里没有 mWakefulness".into());
    }
    let foreground = sec(&ss, "focus").and_then(parse_foreground);

    let mut keepalive = Keepalive::default();
    if let Some(k) = sec(&ss, "ka") {
        let mut lines = k.lines();
        keepalive.screen_off_timeout = lines.next().map(|v| v.trim().to_string());
        keepalive.stay_on_while_plugged_in = lines.next().map(|v| v.trim().to_string());
        if let (Some(t), Some(s)) = (
            &keepalive.screen_off_timeout,
            &keepalive.stay_on_while_plugged_in,
        ) {
            keepalive.policy_ok = Some(policy_ok_android(t, s));
        }
    }

    Card {
        serial: serial.to_string(),
        transport: "adb",
        state: state.to_string(),
        online: state == "device",
        model,
        brand,
        os,
        root,
        battery,
        thermal,
        fan,
        screen_on,
        foreground,
        keepalive,
        uptime_s: sec(&ss, "uptime").and_then(parse_uptime_s),
        screenshot: None,
        probe_depth: depth,
        probe_ms,
        unknown,
    }
}

/// OH 回包 → 卡片。字段比安卓少, 少的那些如实进 `unknown`。
pub fn card_from_oh(serial: &str, raw: &str, probe_ms: u64) -> Card {
    let ss = telemetry::split_sections(raw);
    let mut unknown = Vec::new();

    let props = sec(&ss, "props").unwrap_or_default();
    let model = prop(props, 0);
    let brand = prop(props, 1);
    let os = prop(props, 2);

    let mut battery = Battery::default();
    if let Some(b) = sec(&ss, "battery") {
        let (level, mv, temp, _cur) = telemetry::parse_battery_oh(b);
        battery.level_pct = level;
        battery.voltage_mv = mv;
        battery.temp_c = temp;
    } else {
        unknown.push("电池: hidumper BatteryService 没回值".into());
    }

    let mut thermal = Thermal::default();
    if let Some(t) = sec(&ss, "thermal") {
        let zones = hwcond::parse_thermal(t);
        thermal.zones = zones.len();
        if let Some((zone, c)) = hwcond::soc_max_c(&zones) {
            thermal.soc_zone = Some(zone);
            thermal.soc_max_c = Some(c);
        }
        thermal.gpu_c = gpu_c(&zones);
        thermal.battery_c = battery_c(&zones);
    }

    unknown.push("风扇 / 不息屏策略: 鸿蒙侧这两项尚未上真机验证, 一律不猜".into());

    Card {
        serial: serial.to_string(),
        transport: "hdc",
        state: "device".to_string(),
        online: true,
        model,
        brand,
        os,
        root: sec(&ss, "root").map(|r| r.trim() == "0"),
        battery,
        thermal,
        fan: Fan::default(),
        screen_on: sec(&ss, "screen").and_then(parse_hdc_power_on),
        foreground: None,
        keepalive: Keepalive::default(),
        uptime_s: sec(&ss, "uptime").and_then(parse_uptime_s),
        screenshot: None,
        probe_depth: "full",
        probe_ms,
        unknown,
    }
}

// ══════════════ 设备锁 (只读那三个文件) ══════════════

fn file_age_s(path: &Path) -> Option<u64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(m).ok().map(|d| d.as_secs())
}

pub fn read_lock(dir: &Path) -> Option<Lock> {
    if !dir.is_dir() {
        return None;
    }
    let owner_path = dir.join("devlock.d/owner");
    let owner = std::fs::read_to_string(&owner_path)
        .ok()
        .and_then(|t| parse_lock_owner(&t));
    let heartbeat_age_s = owner.as_ref().and_then(|_| file_age_s(&owner_path));
    let queue = std::fs::read_to_string(dir.join("devlock.queue"))
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut recent: Vec<LockEvent> = std::fs::read_to_string(dir.join("devlock.log"))
        .map(|t| t.lines().filter_map(parse_lock_event).collect())
        .unwrap_or_default();
    recent.reverse();
    recent.truncate(10);

    Some(Lock {
        dir: dir.display().to_string(),
        held_by: owner.as_ref().map(|(l, _)| l.clone()),
        since: owner.as_ref().and_then(|(_, s)| s.clone()),
        stale: heartbeat_age_s.is_some_and(|a| a > LOCK_STALE_SECS),
        heartbeat_age_s,
        queue,
        recent,
    })
}

// ══════════════ 截图 (这条只读通路上唯一有副作用的动作) ══════════════

/// 截图策略。
///
/// 两道闸, 缺一不可:
/// - **设备锁**。别人占着手机时一律不截 —— 截图会拉起一次 screencap,
///   正在采帧时数据的那一轮会多出一个尖峰。
/// - **最小间隔**。默认 5 分钟。面板 30 秒刷一次, 不设间隔就是每 30 秒骚扰一次设备。
pub struct Screens {
    pub dir: PathBuf,
    pub every_s: u64,
    /// 自己持有的锁标签。锁正被这个标签持有时也允许截图。
    pub own_label: Option<String>,
}

impl Screens {
    /// 现在能不能截。返回 `Err(原因)` 时把原因写进卡片的 `unknown`。
    pub fn may_shoot(&self, lock: Option<&Lock>, existing_age_s: Option<u64>) -> Result<(), String> {
        if let Some(l) = lock {
            if let Some(holder) = &l.held_by {
                let mine = self.own_label.as_deref() == Some(holder.as_str());
                if !mine && !l.stale {
                    return Err(format!("设备锁被 {holder} 持有, 这一轮不截图"));
                }
            }
        }
        match existing_age_s {
            Some(age) if age < self.every_s => Err(format!(
                "上一张才 {age}s, 没到最小间隔 {}s",
                self.every_s
            )),
            _ => Ok(()),
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// 序列号里可能有 `:` `/` 之类, 不能直接当文件名。
fn safe_name(serial: &str) -> String {
    serial
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn shoot(phone: &Device, serial: &str, screens: &Screens, lock: Option<&Lock>) -> (Option<Screenshot>, Option<String>) {
    let out = screens.dir.join(format!("{}.jpg", safe_name(serial)));
    let existing_age = file_age_s(&out);
    if let Err(why) = screens.may_shoot(lock, existing_age) {
        // 沿用上一张 (如果有) 并说明为什么没有新的 —— 比直接没有图有用。
        let shot = existing_age.map(|age| Screenshot {
            path: out.display().to_string(),
            at: now_rfc3339(),
            age_s: age,
            fresh: false,
        });
        return (shot, Some(format!("截图: {why}")));
    }
    if std::fs::create_dir_all(&screens.dir).is_err() {
        return (None, Some(format!("截图: 建不了目录 {}", screens.dir.display())));
    }
    match phone.screen(&out.display().to_string()) {
        Some(_) => (
            Some(Screenshot {
                path: out.display().to_string(),
                at: now_rfc3339(),
                age_s: 0,
                fresh: true,
            }),
            None,
        ),
        None => (None, Some("截图: screencap 没回图".into())),
    }
}

// ══════════════ 采集 ══════════════

/// `adb devices -l` 里一台机器: (serial, 状态原文, adb 顺带报的 model)。
type AdbEntry = (String, String, Option<String>);

/// 枚举两族设备。某一族的工具不在 PATH 就整族跳过 —— 与 `devices` 子命令同纪律。
fn enumerate(adb: Option<&str>) -> (Vec<AdbEntry>, Vec<String>) {
    let mut android = Vec::new();
    if let Some(adb) = adb {
        if let Ok(out) = Command::new(adb).args(["devices", "-l"]).output() {
            android = parse_adb_devices_l(&String::from_utf8_lossy(&out.stdout));
        }
    }
    let mut oh = Vec::new();
    if let Ok(out) = Command::new("hdc").args(["list", "targets"]).output() {
        oh = parse_hdc_targets(&String::from_utf8_lossy(&out.stdout));
    }
    (android, oh)
}

pub struct Opts {
    pub serial: Option<String>,
    pub timeout_ms: u64,
    pub lock_dir: PathBuf,
    pub screens: Option<Screens>,
    /// 自己持有的设备锁标签。锁正是这个标签时按"锁是自己的"处理。
    pub own_label: Option<String>,
    /// 别人持锁时也强行走完整档。会干扰对面的测量, 默认关。
    pub probe_while_locked: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            serial: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            lock_dir: PathBuf::from(
                std::env::var("DEVLOCK_DIR").unwrap_or_else(|_| DEFAULT_LOCK_DIR.to_string()),
            ),
            screens: None,
            own_label: None,
            probe_while_locked: false,
        }
    }
}

/// 这一轮该走哪一档。
///
/// 判据只有一条: **这台手机现在是不是别人的**。锁空着、锁是自己的、
/// 或者持锁方心跳已经超期 (那把锁本来就可以被接管了) —— 这三种情况才走完整档。
pub fn depth_for(lock: Option<&Lock>, own_label: Option<&str>, force: bool) -> &'static str {
    if force {
        return "full";
    }
    match lock.and_then(|l| l.held_by.as_deref().map(|h| (h, l.stale))) {
        Some((holder, false)) if Some(holder) != own_label => "light",
        _ => "full",
    }
}

pub fn collect(opts: &Opts) -> Fleet {
    let adb = crate::device::locate_adb();
    let mut notes = Vec::new();
    if adb.is_none() {
        notes.push("没找到 adb, 安卓这一族整族跳过 (ADB_BIN=/path/to/adb 可指定)".to_string());
    }
    let (android, oh) = enumerate(adb.as_deref());
    let hdc_ok = !oh.is_empty()
        || Command::new("hdc")
            .arg("-v")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    if !hdc_ok {
        notes.push("没找到 hdc, 鸿蒙这一族整族跳过".to_string());
    }
    let lock = read_lock(&opts.lock_dir);

    let tmp = std::env::temp_dir()
        .join(format!("phonefarm-fleet-{}", std::process::id()))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::create_dir_all(&tmp);

    let want = |s: &str| opts.serial.as_deref().is_none_or(|w| w == s);
    let depth = depth_for(lock.as_ref(), opts.own_label.as_deref(), opts.probe_while_locked);
    if depth == "light" {
        notes.push(
            "设备锁被别人持有: 这一轮只读了 sysfs (温度/电量/风扇/uptime), \
没跑 dumpsys —— 跑了会给对面正在做的测量添噪声。--probe-while-locked 可强制完整档。"
                .to_string(),
        );
    }
    let mut devices = Vec::new();

    for (serial, state, model_hint) in android.into_iter().filter(|(s, _, _)| want(s)) {
        let t0 = Instant::now();
        if state != "device" {
            // 掉线/未授权的机器也要出现在面板上 —— 它恰恰是最该被看见的那一台。
            devices.push(offline_card(&serial, "adb", &state, model_hint));
            continue;
        }
        let phone = Device::new(Some(serial.clone()), tmp.clone());
        let script = if depth == "light" {
            probe_script_android_light()
        } else {
            probe_script_android()
        };
        let raw = phone.shell(script, opts.timeout_ms);
        let mut card =
            card_from_android(&serial, &state, &raw, t0.elapsed().as_millis() as u64, depth);
        if card.model.is_none() {
            card.model = model_hint;
        }
        if let Some(screens) = &opts.screens {
            let (shot, why) = shoot(&phone, &serial, screens, lock.as_ref());
            card.screenshot = shot;
            if let Some(w) = why {
                card.unknown.push(w);
            }
        }
        devices.push(card);
    }

    for key in oh.into_iter().filter(|k| want(&format!("hdc:{k}"))) {
        let serial = format!("hdc:{key}");
        let t0 = Instant::now();
        let phone = Device::new(Some(serial.clone()), tmp.clone());
        let raw = phone.shell(probe_script_oh(), opts.timeout_ms);
        devices.push(card_from_oh(&serial, &raw, t0.elapsed().as_millis() as u64));
    }

    Fleet {
        schema: "phonefarm/fleet@1",
        tool: "phonefarm",
        version: env!("CARGO_PKG_VERSION"),
        generated_at: now_rfc3339(),
        backends: Backends {
            adb: adb.clone(),
            hdc: hdc_ok,
        },
        lock,
        devices,
        notes,
    }
}

fn offline_card(serial: &str, transport: &'static str, state: &str, model: Option<String>) -> Card {
    Card {
        serial: serial.to_string(),
        transport,
        state: state.to_string(),
        online: false,
        model,
        brand: None,
        os: None,
        root: None,
        battery: Battery::default(),
        thermal: Thermal::default(),
        fan: Fan::default(),
        screen_on: None,
        foreground: None,
        keepalive: Keepalive::default(),
        uptime_s: None,
        screenshot: None,
        probe_depth: "none",
        probe_ms: 0,
        unknown: vec![format!("这台机器 adb 状态是 {state}, 什么都读不到")],
    }
}

// ══════════════ CLI ══════════════

fn usage() -> &'static str {
    "用法: phonefarm fleet [--json] [--serial S] [--timeout-ms N]\n\
     \x20            [--lock-dir 目录] [--screenshot-dir 目录] [--screenshot-every 秒] [--lock-label 标签]\n\
     \x20            [--probe-while-locked]\n\
     \x20  只读: 一趟 shell 把每台手机的在线/型号/电量/温度/风扇/亮屏/前台应用读齐。\n\
     \x20  别人占着手机时自动降成轻量档 (只 cat sysfs, 不跑 dumpsys), 免得给对面的测量添噪声。\n\
     \x20  截图默认关闭; 开了也受设备锁与最小间隔约束 (别人占着手机时不截)。\n\
     \x20  退出码 0=至少读到一台 / 1=一台都没有 / 2=参数错。"
}

pub fn run_fleet(args: &[String]) -> i32 {
    let mut opts = Opts::default();
    let mut json = false;
    let mut screenshot_dir: Option<PathBuf> = None;
    let mut every_s = DEFAULT_SCREENSHOT_EVERY_S;
    let mut own_label: Option<String> = None;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--serial" => opts.serial = it.next().cloned(),
            "--timeout-ms" => {
                opts.timeout_ms = match it.next().and_then(|v| v.parse().ok()) {
                    Some(v) => v,
                    None => {
                        eprintln!("--timeout-ms 需要一个毫秒数\n{}", usage());
                        return 2;
                    }
                }
            }
            "--lock-dir" => {
                let Some(v) = it.next() else {
                    eprintln!("--lock-dir 需要一个目录\n{}", usage());
                    return 2;
                };
                opts.lock_dir = PathBuf::from(v);
            }
            "--screenshot-dir" => screenshot_dir = it.next().map(PathBuf::from),
            "--screenshot-every" => {
                every_s = it.next().and_then(|v| v.parse().ok()).unwrap_or(every_s);
            }
            "--lock-label" => own_label = it.next().cloned(),
            "--probe-while-locked" => opts.probe_while_locked = true,
            other => {
                eprintln!("无法识别的参数 '{other}'\n{}", usage());
                return 2;
            }
        }
    }
    opts.own_label = own_label.clone();
    if let Some(dir) = screenshot_dir {
        opts.screens = Some(Screens {
            dir,
            every_s,
            own_label,
        });
    }

    let fleet = collect(&opts);
    if json {
        match serde_json::to_string(&fleet) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("序列化失败: {e}");
                return 2;
            }
        }
    } else {
        print!("{}", render_text(&fleet));
    }
    if fleet.devices.is_empty() {
        1
    } else {
        0
    }
}

/// 人读的一屏。`--json` 才是给上层消费的。
pub fn render_text(f: &Fleet) -> String {
    let mut s = String::new();
    if let Some(l) = &f.lock {
        match &l.held_by {
            Some(h) => s.push_str(&format!(
                "设备锁: {h} 持有 (心跳 {}s 前{})\n",
                l.heartbeat_age_s.map(|a| a.to_string()).unwrap_or_else(|| "?".into()),
                if l.stale { ", 已超期可接管" } else { "" }
            )),
            None => s.push_str("设备锁: 空闲\n"),
        }
        if !l.queue.is_empty() {
            s.push_str(&format!("排队: {}\n", l.queue.join(" → ")));
        }
    }
    if f.devices.is_empty() {
        s.push_str("没有设备 (adb devices / hdc list targets 均为空)\n");
    }
    for d in &f.devices {
        let q = |v: &Option<String>| v.clone().unwrap_or_else(|| "?".into());
        s.push_str(&format!(
            "{} {:<24} {} {} | 电量 {} {} | SoC {} | GPU {} | 风扇 {} | 亮屏 {} | 前台 {}\n",
            if d.online { "●" } else { "○" },
            d.serial,
            q(&d.model),
            d.os.clone().unwrap_or_else(|| d.state.clone()),
            d.battery.level_pct.map(|v| format!("{v}%")).unwrap_or_else(|| "?".into()),
            q(&d.battery.status),
            d.thermal.soc_max_c.map(|v| format!("{v:.1}℃")).unwrap_or_else(|| "?".into()),
            d.thermal.gpu_c.map(|v| format!("{v:.1}℃")).unwrap_or_else(|| "?".into()),
            match (d.fan.supported, d.fan.enabled) {
                (false, _) => "无".into(),
                (true, Some(true)) => format!("开 L{}", d.fan.speed_level.unwrap_or(-1)),
                (true, Some(false)) => "关".into(),
                (true, None) => "?".into(),
            },
            match d.screen_on { Some(true) => "是", Some(false) => "否", None => "?" },
            d.foreground.clone().unwrap_or_else(|| "-".into()),
        ));
        for u in &d.unknown {
            s.push_str(&format!("    · {u}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const BATTERY: &str = include_str!("testdata/tele_a_battery.txt");

    #[test]
    fn adb_devices_l_keeps_offline_and_unauthorized() {
        let text = "List of devices attached\n\
                    91253241019A           device usb:1-1 product:NX809J model:NX809J device:NX809J\n\
                    emulator-5554          offline\n\
                    ABCDEF                 unauthorized\n\
                    * daemon started successfully *\n";
        let got = parse_adb_devices_l(text);
        assert_eq!(got.len(), 3, "掉线与未授权的机器也要出现在卡片里");
        assert_eq!(got[0].0, "91253241019A");
        assert_eq!(got[0].1, "device");
        assert_eq!(got[0].2.as_deref(), Some("NX809J"));
        assert_eq!(got[1].1, "offline");
        assert_eq!(got[2].1, "unauthorized");
    }

    #[test]
    fn adb_devices_l_keeps_multiword_states() {
        let got = parse_adb_devices_l("XYZ  no permissions user in plugdev group\n");
        assert_eq!(got.len(), 1);
        assert!(got[0].1.starts_with("no permissions"), "状态原文要留住: {}", got[0].1);
    }

    #[test]
    fn hdc_targets_filter_sentinels() {
        assert_eq!(parse_hdc_targets("abc\ndef\n"), vec!["abc", "def"]);
        assert!(parse_hdc_targets("[Empty]\n").is_empty());
        assert!(parse_hdc_targets("[Fail]Connect failed\n").is_empty());
    }

    /// `status: 4` 是 Not charging, **不是**放电 —— 停充之后内核常常就写这个。
    #[test]
    fn battery_status_code_is_not_guessed() {
        let (status, plugged) = parse_battery_status_a(BATTERY);
        assert_eq!(status.as_deref(), Some("Not charging"));
        assert_eq!(plugged, Some(false), "四条 powered 全 false = 没插线");

        let (s2, p2) = parse_battery_status_a("  status: 2\n  USB powered: true\n  AC powered: false\n");
        assert_eq!(s2.as_deref(), Some("Charging"));
        assert_eq!(p2, Some(true));

        let (s3, _) = parse_battery_status_a("  status: 99\n");
        assert_eq!(s3, None, "不认识的码不要硬翻译成某个状态");
    }

    #[test]
    fn android_card_reads_the_real_battery_dump() {
        let raw = format!("-----PF:battery-----\n{BATTERY}\n-----PF:end-----\n");
        let card = card_from_android("SER", "device", &raw, 12, "full");
        assert_eq!(card.battery.level_pct, Some(100));
        assert_eq!(card.battery.temp_c, Some(25.0), "temperature 是分摄氏度");
        assert_eq!(card.battery.status.as_deref(), Some("Not charging"));
        assert_eq!(card.battery.on_battery, Some(true), "没插线且没在充 = 靠电池");
        assert!(card.online);
        assert_eq!(card.probe_ms, 12);
    }

    #[test]
    fn charging_is_never_reported_as_on_battery() {
        let raw = "-----PF:battery-----\n  status: 2\n  USB powered: true\n  level: 40\n";
        let card = card_from_android("SER", "device", raw, 0, "full");
        assert_eq!(card.battery.on_battery, Some(false));
    }

    /// 采不到的项必须留空并说明, 不能填 0。
    #[test]
    fn an_empty_reply_yields_unknowns_not_zeros() {
        let card = card_from_android("SER", "device", "", 0, "full");
        assert_eq!(card.battery.level_pct, None);
        assert_eq!(card.thermal.soc_max_c, None);
        assert_eq!(card.screen_on, None);
        assert!(!card.fan.supported);
        assert!(card.unknown.len() >= 4, "每一项采不到都要有一句说明: {:?}", card.unknown);
    }

    #[test]
    fn thermal_picks_gpu_and_battery_zones() {
        let zones = vec![
            ("cpu-1-0".to_string(), 45_000),
            ("gpuss-0".to_string(), 52_300),
            ("battery".to_string(), 40_000),
            ("dead-sensor".to_string(), 0),
        ];
        assert_eq!(gpu_c(&zones), Some(52.3));
        assert_eq!(battery_c(&zones), Some(40.0));
        assert_eq!(gpu_c(&[("cpu-0".to_string(), 40_000)]), None);
        // 0 是"这个传感器没读数", 不是 0 摄氏度
        assert_eq!(battery_c(&[("battery".to_string(), 0)]), None);
    }

    /// 节点读不到 = 这台机器没风扇, 跟"风扇关着"是两回事。
    #[test]
    fn fan_absent_is_not_fan_off() {
        let off = parse_fan("/sys/kernel/fan/fan_enable=0\n/sys/kernel/fan/fan_speed_level=0\n");
        assert!(off.supported);
        assert_eq!(off.enabled, Some(false));

        let on = parse_fan("/sys/kernel/fan/fan_enable=1\n/sys/kernel/fan/fan_speed_level=5\n");
        assert_eq!(on.enabled, Some(true));
        assert_eq!(on.speed_level, Some(5));

        let none = parse_fan("/sys/kernel/fan/fan_enable=\n/sys/kernel/fan/fan_speed_level=\n");
        assert!(!none.supported, "读不到节点应报「没有风扇」而不是「风扇关着」");
        assert_eq!(none.enabled, None);
    }

    #[test]
    fn wakefulness_and_foreground() {
        assert_eq!(parse_wakefulness("  mWakefulness=Awake\n"), Some(true));
        assert_eq!(parse_wakefulness("  mWakefulness=Asleep\n"), Some(false));
        assert_eq!(parse_wakefulness("(什么都没有)"), None);

        let focus = "  mCurrentFocus=Window{a1b2 u0 com.miHoYo.Yuanshen/com.miHoYo.GetMobileInfo.MainActivity}\n";
        assert_eq!(parse_foreground(focus).as_deref(), Some("com.miHoYo.Yuanshen"));
        assert_eq!(parse_foreground("  mCurrentFocus=null\n"), None);
    }

    #[test]
    fn uptime_and_root_and_policy() {
        assert_eq!(parse_uptime_s("123456.78 987654.32\n"), Some(123456));
        assert_eq!(parse_uptime_s(""), None);

        assert!(parse_root("0\n"));
        assert!(!parse_root("su: not found\n"), "报错文本里的数字不算 root");
        assert!(!parse_root(""));

        assert!(policy_ok_android("2147483647", "3"));
        assert!(!policy_ok_android("30000", "3"), "会息屏就不算到位");
        assert!(!policy_ok_android("2147483647", "0"));
        assert!(!policy_ok_android("2147483647", "null"));
    }

    #[test]
    fn os_line_combines_release_and_sdk() {
        let raw = "-----PF:props-----\nNX809J\nnubia\n16\n36\n";
        let card = card_from_android("SER", "device", raw, 0, "full");
        assert_eq!(card.model.as_deref(), Some("NX809J"));
        assert_eq!(card.brand.as_deref(), Some("nubia"));
        assert_eq!(card.os.as_deref(), Some("Android 16 (SDK 36)"));
    }

    #[test]
    fn lock_files_are_parsed() {
        let (label, since) = parse_lock_owner("sysknob-loop since 2026-09-23 10:12:31\n").unwrap();
        assert_eq!(label, "sysknob-loop");
        assert_eq!(since.as_deref(), Some("2026-09-23 10:12:31"));
        assert!(parse_lock_owner("  \n").is_none());

        let ev = parse_lock_event("2026-09-23 09:16:41 acquire sysknob-loop").unwrap();
        assert_eq!(ev.action, "acquire");
        assert_eq!(ev.label, "sysknob-loop");
        let steal = parse_lock_event("2026-09-23 09:16:41 steal by wb-megacity from [x] idle 900s").unwrap();
        assert_eq!(steal.label, "wb-megacity", "steal 行要跳过 by 取到新主人");
        assert!(parse_lock_event("2026-09-23 09:16:41 renew x").is_none());
        assert!(parse_lock_event("这不是锁日志").is_none());
    }

    /// 别人占着手机时一律不截图 —— 截图会给正在采的那一轮加一个尖峰。
    #[test]
    fn screenshots_yield_to_the_device_lock() {
        let screens = Screens {
            dir: PathBuf::from("/tmp/x"),
            every_s: 300,
            own_label: Some("gol-panel".into()),
        };
        let held = |by: &str, stale: bool| Lock {
            dir: "/x".into(),
            held_by: Some(by.into()),
            since: None,
            heartbeat_age_s: Some(1),
            stale,
            queue: vec![],
            recent: vec![],
        };
        let other = held("wb-vksamples", false);
        assert!(screens.may_shoot(Some(&other), None).is_err());
        let mine = held("gol-panel", false);
        assert!(screens.may_shoot(Some(&mine), None).is_ok(), "自己持锁时可以截");
        let dead = held("someone", true);
        assert!(screens.may_shoot(Some(&dead), None).is_ok(), "心跳超期的锁不算数");

        let free = Lock { dir: "/x".into(), held_by: None, since: None,
                          heartbeat_age_s: None, stale: false, queue: vec![], recent: vec![] };
        assert!(screens.may_shoot(Some(&free), None).is_ok());
        // 最小间隔: 面板 30 秒刷一次, 不设间隔就是每 30 秒骚扰一次设备
        assert!(screens.may_shoot(Some(&free), Some(30)).is_err());
        assert!(screens.may_shoot(Some(&free), Some(9999)).is_ok());
    }

    /// 别人占着手机时不许跑 dumpsys —— 一次 `dumpsys window` 就够给
    /// 对面那轮 30 秒帧时测量多出一帧 jank。
    #[test]
    fn a_lock_held_by_someone_else_downgrades_the_probe() {
        let held = |by: &str, stale: bool| Lock {
            dir: "/x".into(), held_by: Some(by.into()), since: None,
            heartbeat_age_s: Some(1), stale, queue: vec![], recent: vec![],
        };
        let other = held("wb-vksamples", false);
        assert_eq!(depth_for(Some(&other), None, false), "light");
        assert_eq!(depth_for(Some(&other), Some("gol-panel"), false), "light");
        assert_eq!(
            depth_for(Some(&other), None, true),
            "full",
            "--probe-while-locked 要能强制, 但那是明知会干扰"
        );

        // 自己持锁、锁空着、心跳超期(本来就可接管) —— 这三种才是完整档
        assert_eq!(depth_for(Some(&held("gol-panel", false)), Some("gol-panel"), false), "full");
        assert_eq!(depth_for(Some(&held("someone", true)), None, false), "full");
        let free = Lock { dir: "/x".into(), held_by: None, since: None,
                          heartbeat_age_s: None, stale: false, queue: vec![], recent: vec![] };
        assert_eq!(depth_for(Some(&free), None, false), "full");
        assert_eq!(depth_for(None, None, false), "full", "没配锁目录就照常读");
    }

    #[test]
    fn the_light_probe_still_yields_temperature_battery_and_fan() {
        let raw = "-----PF:props-----\nNX809J\nnubia\n16\n36\n\
-----PF:uptime-----\n864000.5 1.0\n\
-----PF:sysbat-----\ncapacity=87\nstatus=Discharging\ntemp=402\ncurrent_now=-325000\nvoltage_now=4102000\n\
-----PF:thermal-----\ngpuss-0 52300\ncpu-1-0 45000\n\
-----PF:fan-----\n/sys/kernel/fan/fan_enable=1\n/sys/kernel/fan/fan_speed_level=5\n";
        let card = card_from_android("SER", "device", raw, 30, "light");
        assert_eq!(card.probe_depth, "light");
        assert_eq!(card.battery.level_pct, Some(87));
        assert_eq!(card.battery.temp_c, Some(40.2), "sysfs 的 temp 是分摄氏度");
        assert_eq!(card.battery.on_battery, Some(true), "current_now 是负的 = 在放电");
        assert_eq!(card.thermal.gpu_c, Some(52.3));
        assert!(card.fan.enabled == Some(true) && card.fan.speed_level == Some(5));
        assert_eq!(card.uptime_s, Some(864000));
        // 轻量档拿不到的那几项要说清楚是"这一轮没读", 不是"读不到"
        assert_eq!(card.foreground, None);
        assert_eq!(card.screen_on, None);
        assert!(
            card.unknown.iter().any(|u| u.contains("设备锁被别人持有")),
            "缺席的原因必须写出来: {:?}", card.unknown
        );
    }

    /// 没探过的项必须是 None。`root: false` 会让人以为"这机器没 root",
    /// 而真相是这一轮压根没查。
    #[test]
    fn root_is_null_when_it_was_never_probed() {
        let light = card_from_android("SER", "device", "-----PF:props-----\nX\n", 0, "light");
        assert_eq!(light.root, None);
        let full = card_from_android("SER", "device", "-----PF:root-----\n0\n", 0, "full");
        assert_eq!(full.root, Some(true));
        let no_su = card_from_android("SER", "device", "-----PF:root-----\nsu: not found\n", 0, "full");
        assert_eq!(no_su.root, Some(false));
    }

    #[test]
    fn sysfs_battery_reads_charging_as_not_on_battery() {
        let b = parse_sysfs_battery("capacity=100\nstatus=Charging\ntemp=250\ncurrent_now=1072000\n");
        assert_eq!(b.on_battery, Some(false));
        assert_eq!(b.temp_c, Some(25.0));
        // 停充后内核常写 "Not charging" 而电流才是真判据
        let b2 = parse_sysfs_battery("status=Not charging\ncurrent_now=-325000\n");
        assert_eq!(b2.on_battery, Some(true));
        let b3 = parse_sysfs_battery("status=Not charging\n");
        assert_eq!(b3.on_battery, None, "只有状态没有电流时判不出来, 就别判");
    }

    #[test]
    fn serial_with_colon_becomes_a_safe_filename() {
        assert_eq!(safe_name("hdc:5ce122"), "hdc_5ce122");
        assert_eq!(safe_name("91253241019A"), "91253241019A");
    }

    #[test]
    fn offline_devices_still_get_a_card() {
        let card = offline_card("emulator-5554", "adb", "offline", None);
        assert!(!card.online);
        assert_eq!(card.root, None, "没探过就不是 false");
        assert!(card.unknown[0].contains("offline"));
    }

    #[test]
    fn text_render_marks_unknowns_with_question_marks() {
        let fleet = Fleet {
            schema: "phonefarm/fleet@1",
            tool: "phonefarm",
            version: "0.2.0",
            generated_at: "now".into(),
            backends: Backends { adb: None, hdc: false },
            lock: None,
            devices: vec![offline_card("X", "adb", "offline", None)],
            notes: vec![],
        };
        let s = render_text(&fleet);
        assert!(s.contains('○'));
        assert!(s.contains('?'), "采不到的位置要看得出来是没读到: {s}");
    }

    #[test]
    fn oh_card_does_not_invent_android_only_fields() {
        let card = card_from_oh("hdc:abc", "-----PF:props-----\nMate\nHUAWEI\nOpenHarmony 5.0\n", 5);
        assert_eq!(card.transport, "hdc");
        assert_eq!(card.os.as_deref(), Some("OpenHarmony 5.0"));
        assert!(!card.fan.supported);
        assert_eq!(card.keepalive.policy_ok, None);
        assert!(card.unknown.iter().any(|u| u.contains("尚未上真机验证")));
    }
}
