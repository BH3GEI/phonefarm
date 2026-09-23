//! 系统参数闭环的两块纯逻辑: 「哪些参数准改、准改成什么」与「这组参数保留还是淘汰」。
//!
//! 由 `loop_v1/auto/whitelist.py` 与 `loop_v1/auto/verdict.py` 逐行搬过来, 输出字节一致。
//! 全程不读设备、不读时钟、无随机数 —— 输入 probe 文本与对比统计, 输出白名单与判定。
//!
//! 白名单不是写死的常量表
//! ----------------------
//! 它是 `probe_sysparam.sh` 真机探测结果的函数: 每个候选参数要同时满足
//! 「节点存在 + 可写 + 写了真生效」才进白名单, 三关缺一不可。探不到的、写了被内核
//! 退回的, 一律不进 —— 宁可少改, 不可假改。
//!
//! 另有一条不看探测结果的硬规矩: **温控保护相关的任何节点永远不进白名单**。
//! 关热保护、抬温控阈值能立刻换来漂亮数字, 但那是拿硬件安全换指标, 不是优化。
//! [`DENY_KEYWORDS`] 在这里拦一道, `knob_sysparam.sh` 在设备端再拦一道。
//!
//! 判定看三样, 不是只看省电
//! ------------------------
//! | 指标         | 方向     | 来源                       |
//! |--------------|----------|----------------------------|
//! | frame_p95    | 越小越好 | ftrace 帧时序 (帧时稳定性) |
//! | fps_mean     | 越大越好 | ftrace 帧时序 (帧率)       |
//! | power_w_mean | 越小越好 | power_supply 轨 (整机功耗) |
//!
//! 系统参数不动画面, 所以画质天然不变, 不必量。温度不作为「更好」的加分项,
//! 只作为**上限**: 超过 temp_cap 这一轮直接作废并还原, 不参与比较。
//!
//! 多重比较: 一次看 3 个指标, 按 0.05 判「显著改善」会把假阳性抬到约 14%。
//! 所以「改善」一侧用 Bonferroni 收紧到 alpha/K, 「变差」一侧仍用 0.05 不收紧
//! —— 两侧故意不对称: 宁可漏掉一个真改善, 也不要把一个真退化放过去。

use crate::pyjson::{py_round, py_sum, PyVal};
use crate::pyobj;
use regex::Regex;

/// 温控保护关键词。命中即拒, 不看探测结果, 不看大模型怎么说。
pub const DENY_KEYWORDS: [&str; 7] = [
    "thermal",
    "trip_point",
    "cooling",
    "fan",
    "tsens",
    "bcl",
    "throttl",
];

/// 生效判定: 只有这一种结论算「写了真生效」
const EFFECT_OK: [&str; 1] = ["live"];
/// 部分节点当前值已经是探测值, 无法做差异写测试 —— 可写 + 有合法取值表即可进,
/// 每轮还有 snap_at_run.txt 复核实际生效, 所以不会放过假生效。
const EFFECT_SOFT: [&str; 1] = ["skipped_same_value"];

const ALPHA: f64 = 0.05;
/// 手机整机功耗的合理区间 (瓦)。本机 battery/power_now 读出过 777W —— 单位有误,
/// 这种数不进任何均值, 只如实记录并标 plausible=False。
const PLAUSIBLE_W: (f64, f64) = (0.05, 30.0);
/// (指标, 方向) —— 方向 "lower" = 越小越好
const PRIMARY_METRICS: [(&str, &str); 3] = [
    ("frame_p95", "lower"),
    ("fps_mean", "higher"),
    ("power_w_mean", "lower"),
];
const RULE_VERSION: &str = "sysparam_v1";

// ══════════════ probe 文本 ══════════════

/// 保序的 key=value 表 (Python dict 语义: 重复键就地覆盖, 不改位置)。
/// 顺序要紧 —— 厂商刷新率那一段要按 probe 里出现的顺序遍历。
#[derive(Default, Clone)]
pub struct ProbeMap(Vec<(String, String)>);

impl ProbeMap {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.as_str())
    }
    /// Python `d.get(k, "")` / `d.get(k) or ""`
    fn s(&self, k: &str) -> &str {
        self.get(k).unwrap_or("")
    }
    fn set(&mut self, k: &str, v: &str) {
        match self.0.iter_mut().find(|(ek, _)| ek == k) {
            Some(slot) => slot.1 = v.to_string(),
            None => self.0.push((k.to_string(), v.to_string())),
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// `probe_sysparam.sh` 的 key=value 文本 → 表。`#` 开头是注释。
pub fn parse_probe(text: &str) -> ProbeMap {
    let mut out = ProbeMap::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        out.set(k.trim(), v.trim());
    }
    out
}

// ══════════════ 白名单 ══════════════

/// 一个参数准写哪些值。
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    /// "sysfs" | "setting"
    pub kind: String,
    /// 设备端路径 (sysfs 绝对路径, 或 "<ns>:<key>")
    pub path: String,
    /// 允许的取值列表, 大模型只能从里面挑
    pub values: Vec<String>,
    /// 探测时的原值
    pub current: String,
    /// 用于跨项约束检查 (同 group 的 min/max 要成对合法)
    pub group: Option<String>,
    /// 这一项是凭什么进来的 —— 证据里直接看得到, 不必回头翻 probe.txt
    pub effect: String,
}

/// 按插入顺序排列的白名单。
pub type Whitelist = Vec<(String, Spec)>;

pub fn wl_get<'a>(wl: &'a Whitelist, pid: &str) -> Option<&'a Spec> {
    wl.iter().find(|(k, _)| k == pid).map(|(_, v)| v)
}

fn ints(s: &str) -> Vec<i64> {
    let mut v: Vec<i64> = s
        .replace(',', " ")
        .split_whitespace()
        .filter_map(|t| t.parse::<i64>().ok())
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 从 dumpsys 抓来的刷新率文本里取出档位。
///
/// 不同 Android 版本的措辞不一样 ("120.00001 fps" / "fps=120" / "60.000004"),
/// 所以只认数字本身, 四舍五入到整数再去重 —— 120.00001 与 120 是同一档。
pub fn fps_list(s: &str) -> Vec<i64> {
    let re = Regex::new(r"\d+(?:\.\d+)?").expect("fps re");
    let mut v: Vec<i64> = re
        .find_iter(s)
        .filter_map(|m| m.as_str().parse::<f64>().ok())
        // Python 的 round() 是 banker's rounding, py_round(x, 0) 走同一条规则
        .map(|x| py_round(x, 0) as i64)
        .filter(|x| *x > 0)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

pub fn denied(path: &str) -> bool {
    let low = path.to_lowercase();
    DENY_KEYWORDS.iter().any(|k| low.contains(k))
}

/// 可写 + (生效测试通过 或 这台机器上没有可供差异测试的第二个值)。
///
/// `probe_sysparam.sh` 对每个 sysfs 候选都会做差异写测试, 但有些节点在当前设备状态下
/// 找不到合法的第二个值可试 (例如当前值已经是唯一能写的那个), 这时 `.effect` 缺失。
/// 这种情况放行, 但把 `effect` 原样记进白名单 spec, 证据里看得到哪几项是
/// 「未经差异测试」进来的 —— 不含糊过去。
/// settings 类 (刷新率) 没有差异测试, 靠每轮的 snap_at_run.txt 复核实际生效。
fn usable(probe: &ProbeMap, key: &str) -> bool {
    if probe.get(&format!("{key}.writable")) != Some("yes") {
        return false;
    }
    match probe.get(&format!("{key}.effect")) {
        None => true,
        Some(e) => EFFECT_OK.contains(&e) || EFFECT_SOFT.contains(&e),
    }
}

fn effect_of(probe: &ProbeMap, key: &str) -> String {
    probe
        .get(&format!("{key}.effect"))
        .unwrap_or("untested")
        .to_string()
}

struct Builder<'a> {
    probe: &'a ProbeMap,
    wl: Whitelist,
}

impl Builder<'_> {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        pid: &str,
        kind: &str,
        path: &str,
        values: Vec<String>,
        current: &str,
        group: Option<&str>,
        probe_key: Option<&str>,
    ) {
        if denied(path) || denied(pid) {
            return;
        }
        let values: Vec<String> = values.into_iter().filter(|v| !v.is_empty()).collect();
        if values.len() < 2 {
            return;
        }
        let spec = Spec {
            kind: kind.to_string(),
            path: path.to_string(),
            values,
            current: current.to_string(),
            group: group.map(String::from),
            effect: match probe_key {
                Some(k) => effect_of(self.probe, k),
                None => "n/a(setting)".to_string(),
            },
        };
        match self.wl.iter_mut().find(|(k, _)| k == pid) {
            Some(slot) => slot.1 = spec,
            None => self.wl.push((pid.to_string(), spec)),
        }
    }
}

fn strs(xs: &[i64]) -> Vec<String> {
    xs.iter().map(|x| x.to_string()).collect()
}

/// probe → 白名单。
pub fn build_whitelist(probe: &ProbeMap) -> Whitelist {
    let mut b = Builder {
        probe,
        wl: Vec::new(),
    };

    // ── CPU 各簇 ──
    for pol in probe.s("cpu.policies").split(',') {
        let pol = pol.trim();
        if pol.is_empty() {
            continue;
        }
        let k = format!("cpu.policy{pol}");
        let base = format!("/sys/devices/system/cpu/cpufreq/policy{pol}");
        let freqs = ints(probe.s(&format!("{k}.avail_freqs")));
        if !freqs.is_empty() {
            for which in ["scaling_min_freq", "scaling_max_freq"] {
                let key = format!("{k}.{which}");
                if usable(probe, &key) {
                    let cur = probe.s(&format!("{key}.cur")).to_string();
                    b.add(
                        &key,
                        "sysfs",
                        &format!("{base}/{which}"),
                        strs(&freqs),
                        &cur,
                        Some(&format!("{k}.freq")),
                        Some(&key),
                    );
                }
            }
        }
        let govs: Vec<String> = probe
            .s(&format!("{k}.avail_governors"))
            .split_whitespace()
            .map(String::from)
            .collect();
        let gkey = format!("{k}.scaling_governor");
        if !govs.is_empty() && usable(probe, &gkey) {
            let cur = probe.s(&format!("{gkey}.cur")).to_string();
            b.add(
                &gkey,
                "sysfs",
                &format!("{base}/scaling_governor"),
                govs,
                &cur,
                None,
                Some(&gkey),
            );
        }
    }

    // ── GPU ──
    let kgsl = "/sys/class/kgsl/kgsl-3d0";
    let nlv: i64 = probe.s("gpu.num_pwrlevels").parse().unwrap_or(0);
    if nlv > 1 {
        // kgsl 语义: 0 = 最快档, 数字越大越慢。min_pwrlevel 是「最慢准到哪」,
        // max_pwrlevel 是「最快准到哪」, 因此恒有 max_pwrlevel <= min_pwrlevel。
        let lv: Vec<i64> = (0..nlv).collect();
        for which in ["min_pwrlevel", "max_pwrlevel"] {
            let key = format!("gpu.{which}");
            if usable(probe, &key) {
                let cur = probe.s(&format!("{key}.cur")).to_string();
                b.add(
                    &key,
                    "sysfs",
                    &format!("{kgsl}/{which}"),
                    strs(&lv),
                    &cur,
                    Some("gpu.pwrlevel"),
                    Some(&key),
                );
            }
        }
    }
    let gfreqs = ints(probe.s("gpu.devfreq.avail_freqs"));
    if !gfreqs.is_empty() {
        for which in ["min_freq", "max_freq"] {
            let key = format!("gpu.devfreq.{which}");
            if usable(probe, &key) {
                let cur = probe.s(&format!("{key}.cur")).to_string();
                b.add(
                    &key,
                    "sysfs",
                    &format!("{kgsl}/devfreq/{which}"),
                    strs(&gfreqs),
                    &cur,
                    Some("gpu.devfreq"),
                    Some(&key),
                );
            }
        }
    }
    let ggov: Vec<String> = probe
        .s("gpu.devfreq.avail_governors")
        .split_whitespace()
        .map(String::from)
        .collect();
    if !ggov.is_empty() && usable(probe, "gpu.devfreq.governor") {
        let cur = probe.s("gpu.devfreq.governor.cur").to_string();
        b.add(
            "gpu.devfreq.governor",
            "sysfs",
            &format!("{kgsl}/devfreq/governor"),
            ggov,
            &cur,
            None,
            Some("gpu.devfreq.governor"),
        );
    }

    // ── 总线 DDR / LLCC 下限 (已验证过的那一条就在这里) ──
    for n in ["DDR", "LLCC"] {
        let key = format!("bus.{n}.boost_freq");
        if !usable(probe, &key) {
            continue;
        }
        let mut vals = ints(probe.s(&format!("bus.{n}.avail_freqs")));
        if vals.is_empty() {
            let lo = probe.s(&format!("bus.{n}.hw_min_freq"));
            let hi = probe.s(&format!("bus.{n}.hw_max_freq"));
            vals = ints(&format!("{lo} {hi}"));
        }
        let cur = probe.s(&format!("{key}.cur")).to_string();
        b.add(
            &key,
            "sysfs",
            &format!("/sys/devices/system/cpu/bus_dcvs/{n}/boost_freq"),
            strs(&vals),
            &cur,
            None,
            Some(&key),
        );
    }

    // ── 刷新率 ──
    // 两条路都试: AOSP 的 peak/min_refresh_rate, 以及厂商自己的 refresh_rate_mode。
    // 本机 (红魔 NX809J) AOSP 那两个键是 null, 走的是 refresh_rate_mode。
    let modes = fps_list(probe.s("display.modes"));
    for (pid, skey) in [
        ("setting.system.peak_refresh_rate", "peak_refresh_rate"),
        ("setting.system.min_refresh_rate", "min_refresh_rate"),
    ] {
        let cur = probe.s(&format!("setting.system.{skey}.cur")).to_string();
        if cur.is_empty() || cur == "null" || modes.is_empty() {
            continue;
        }
        b.add(
            pid,
            "setting",
            &format!("system:{skey}"),
            strs(&modes),
            &cur,
            Some("refresh"),
            None,
        );
    }

    // 厂商键的取值语义没有文档, 所以不按 all_refresh_rate 的下标猜, 只认探测时
    // **真的把活动刷新率改掉了**的那几档 —— 探测脚本逐档写进去看 SurfaceFlinger
    // 的活动模式 fps 跟不跟着变, 结果落在 refresh_rate_mode.mode<i>_fps 行里。
    let rrm_cur = probe.s("setting.system.refresh_rate_mode.cur").to_string();
    if !rrm_cur.is_empty() && rrm_cur != "null" && is_digits(&rrm_cur) {
        // base_fps 必须是个真数字。探不到活动刷新率 (dumpsys 措辞不同 / 没有
        // mActiveSfDisplayMode 那一行) 时 base_fps 与各档 fps 都是空, 这时候一档都
        // 不能收 —— 没有 fps 证据就收下去, 等于在按下标猜, 正是这里不干的事。
        let base_fps = probe.s("setting.system.refresh_rate_mode.base_fps").to_string();
        let mut live = vec![rrm_cur.clone()];
        if is_digits(&base_fps) {
            let rb_re = Regex::new(r"readback=([^)\s]*)").expect("readback re");
            const PRE: &str = "setting.system.refresh_rate_mode.mode";
            for (k, v) in probe.iter() {
                let Some(rest) = k.strip_prefix(PRE).and_then(|r| r.strip_suffix("_fps")) else {
                    continue;
                };
                if !is_digits(rest) {
                    continue;
                }
                let fps = v.split_whitespace().next().unwrap_or("");
                // 回读整串精确比对 (不能用子串: readback=1 会命中 readback=10),
                // 再要求活动刷新率确实是个数字且与基准不同 = 这一档真生效
                let rb_ok = rb_re
                    .captures(v)
                    .is_some_and(|c| c.get(1).map(|m| m.as_str()) == Some(rest));
                if rb_ok && is_digits(fps) && fps != base_fps {
                    live.push(rest.to_string());
                }
            }
        }
        let mut uniq = live.clone();
        uniq.sort();
        uniq.dedup();
        if uniq.len() > 1 {
            uniq.sort_by_key(|s| s.parse::<i64>().unwrap_or(0));
            b.add(
                "setting.system.refresh_rate_mode",
                "setting",
                "system:refresh_rate_mode",
                uniq,
                &rrm_cur,
                Some("refresh"),
                None,
            );
        }
    }

    b.wl
}

/// Python `str.isdigit()` 在这里的用法: 非空且全是 ASCII 数字。
fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

// ══════════════ 校验 / plan / 说明 ══════════════

/// Python 列表切片的 repr: `['a', 'b']`
fn list_repr(xs: &[String]) -> String {
    format!(
        "[{}]",
        xs.iter()
            .map(|v| format!("'{v}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Python `int(cand.get(id, wl[id]['current'] or 默认))` 那一串的等价物。
fn cross_val(cand: &[(String, String)], wl: &Whitelist, pid: &str, dflt: i64) -> i64 {
    if let Some((_, v)) = cand.iter().find(|(k, _)| k == pid) {
        return v.parse().unwrap_or(dflt);
    }
    match wl_get(wl, pid).map(|s| s.current.as_str()) {
        // Python 的 `or`: 空串是假值, 退回默认
        Some(c) if !c.is_empty() => c.parse().unwrap_or(dflt),
        _ => dflt,
    }
}

fn cross_val_f(cand: &[(String, String)], wl: &Whitelist, pid: &str, dflt: f64) -> f64 {
    if let Some((_, v)) = cand.iter().find(|(k, _)| k == pid) {
        return v.parse().unwrap_or(dflt);
    }
    match wl_get(wl, pid).map(|s| s.current.as_str()) {
        Some(c) if !c.is_empty() => c.parse().unwrap_or(dflt),
        _ => dflt,
    }
}

/// Python 的 `1 << 62`
const HUGE: i64 = 1 << 62;

/// 校验大模型挑的一组参数。返回 (是否合法, 原因)。
///
/// 三层检查, 任何一层不过整组作废 —— 不做「剔掉违规项后凑合跑」, 因为那样跑出来的
/// 数据对应的不是模型提出的那组参数, 证据会对不上账。
pub fn validate_candidate(cand: &[(String, String)], wl: &Whitelist) -> (bool, String) {
    if cand.is_empty() {
        return (false, "空参数组".into());
    }
    for (pid, val) in cand {
        let Some(spec) = wl_get(wl, pid) else {
            return (false, format!("{pid} 不在白名单"));
        };
        if denied(pid) || denied(&spec.path) {
            return (false, format!("{pid} 命中温控保护关键词"));
        }
        if !spec.values.contains(val) {
            let head: Vec<String> = spec.values.iter().take(6).cloned().collect();
            return (
                false,
                format!("{pid}={val} 不在合法取值表 {}...", list_repr(&head)),
            );
        }
    }

    // 跨项约束: CPU 频率 min <= max
    for (pid, v) in cand {
        if let Some(stem) = pid.strip_suffix(".scaling_min_freq") {
            let mx_id = format!("{stem}.scaling_max_freq");
            let mn: i64 = v.parse().unwrap_or(0);
            let mx = cross_val(cand, wl, &mx_id, HUGE);
            if mn > mx {
                return (false, format!("{pid}={mn} 超过同簇 max={mx}"));
            }
        }
    }
    // GPU pwrlevel: 0 最快, 恒有 max_pwrlevel <= min_pwrlevel。
    // 缺省值一律取「最宽松」的那一端 (未知的 max 当 0, 未知的 min 当无穷大),
    // 否则 min_pwrlevel 没进白名单时会把一切合法的 max_pwrlevel 都误判成倒置。
    let has = |k: &str| cand.iter().any(|(p, _)| p == k);
    if has("gpu.max_pwrlevel") || has("gpu.min_pwrlevel") {
        let mx = cross_val(cand, wl, "gpu.max_pwrlevel", 0);
        let mn = cross_val(cand, wl, "gpu.min_pwrlevel", HUGE);
        if mx > mn {
            return (
                false,
                format!("gpu.max_pwrlevel={mx} 必须 <= gpu.min_pwrlevel={mn} (0 是最快档)"),
            );
        }
    }
    // GPU devfreq min <= max
    if has("gpu.devfreq.min_freq") || has("gpu.devfreq.max_freq") {
        let mn = cross_val(cand, wl, "gpu.devfreq.min_freq", 0);
        let mx = cross_val(cand, wl, "gpu.devfreq.max_freq", HUGE);
        if mn > mx {
            return (false, format!("gpu.devfreq.min_freq={mn} 超过 max={mx}"));
        }
    }
    // 刷新率 min <= peak
    if has("setting.system.min_refresh_rate") || has("setting.system.peak_refresh_rate") {
        let mn = cross_val_f(cand, wl, "setting.system.min_refresh_rate", 0.0);
        let pk = cross_val_f(cand, wl, "setting.system.peak_refresh_rate", 1e9);
        if mn > pk {
            return (
                false,
                format!(
                    "min_refresh_rate={} 超过 peak={}",
                    PyVal::Float(mn).py_str(),
                    PyVal::Float(pk).py_str()
                ),
            );
        }
    }
    (true, "ok".into())
}

/// 一组参数 → 设备端 plan 文件文本。
///
/// 输出按 param_id 排序, 所以同一组参数每次生成逐字节相同 (证据可比对)。
/// 写入顺序里把「放宽上限」排在「抬高下限」之前, 少踩一次内核的 min<=max 夹取;
/// knob_sysparam.sh 另有两遍写兜底。
pub fn plan_text(cand: &[(String, String)], wl: &Whitelist) -> String {
    let rank = |pid: &str| -> (u8, String) {
        let first = if pid.ends_with("max_freq")
            || pid.ends_with("max_pwrlevel")
            || pid.contains("peak_refresh")
        {
            0
        } else {
            1
        };
        (first, pid.to_string())
    };
    let mut ids: Vec<&(String, String)> = cand.iter().collect();
    ids.sort_by_key(|(p, _)| rank(p));
    let mut lines = vec!["# loop_v1 sysparam plan".to_string()];
    for (pid, val) in ids {
        if let Some(s) = wl_get(wl, pid) {
            lines.push(format!("{}\t{}\t{}", s.kind, s.path, val));
        }
    }
    lines.join("\n") + "\n"
}

/// 给大模型看的白名单说明 (紧凑, 取值表超过 8 个时只给首尾与档数)。
pub fn describe_whitelist(wl: &Whitelist) -> String {
    let mut ids: Vec<&(String, Spec)> = wl.iter().collect();
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    ids.iter()
        .map(|(pid, s)| {
            let v = &s.values;
            let vs = if v.len() <= 8 {
                v.join(", ")
            } else {
                format!(
                    "{} .. {} (共 {} 档: {}, ...)",
                    v[0],
                    v[v.len() - 1],
                    v.len(),
                    v[..3].join(", ")
                )
            };
            format!("- {pid} (当前 {}) 可选: {vs}", s.current)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Spec {
    pub fn to_pyval(&self) -> PyVal {
        pyobj! {
            "kind" => self.kind.clone(),
            "path" => self.path.clone(),
            "values" => self.values.clone(),
            "current" => self.current.clone(),
            "group" => self.group.clone(),
            "effect" => self.effect.clone(),
        }
    }
}

pub fn whitelist_to_pyval(wl: &Whitelist) -> PyVal {
    PyVal::Obj(wl.iter().map(|(k, s)| (k.clone(), s.to_pyval())).collect())
}

// ══════════════ 判定规则 ══════════════

/// n v n 精确置换检验能取到的最小双侧 p 值 = 2 / C(2n, n)。
fn min_reachable_p(pairs: i64) -> Option<f64> {
    if pairs < 2 {
        return None;
    }
    // C(2n, n) 用乘除交替算, 不会溢出到需要大整数的量级 (pairs 实际 <= 10)
    let mut c: f64 = 1.0;
    for i in 1..=pairs {
        c = c * (pairs + i) as f64 / i as f64;
    }
    Some(2.0 / c)
}

fn active_metrics(power_available: bool) -> Vec<(&'static str, &'static str)> {
    PRIMARY_METRICS
        .into_iter()
        .filter(|(m, _)| power_available || *m != "power_w_mean")
        .collect()
}

/// 落盘用的规则快照。autoloop 在采第一组候选数据之前写它。
pub fn rule_doc(temp_cap_c: f64, pairs: i64, power_available: bool, power_note: &str) -> PyVal {
    let metrics = active_metrics(power_available);
    let k = metrics.len();
    let minp = min_reachable_p(pairs);
    pyobj! {
        "version" => RULE_VERSION,
        "alpha_regression" => ALPHA,
        "alpha_win_bonferroni" => if k > 0 { PyVal::Float(py_round(ALPHA / k as f64, 6)) } else { PyVal::Null },
        "n_metrics" => k,
        "metrics" => PyVal::List(metrics.iter().map(|(m, d)| pyobj!{ "id" => *m, "better" => *d }).collect()),
        "pairs_per_candidate" => pairs,
        "min_reachable_p" => minp,
        "reachable" => minp.unwrap_or(1.0) < if k > 0 { ALPHA / k as f64 } else { 0.0 },
        "temp_cap_c" => temp_cap_c,
        "power_in_verdict" => power_available,
        "power_note" => power_note,
        "keep_condition" => "至少一个指标显著改善 (p < alpha_win) 且没有任何指标显著变差 (p < 0.05)",
        "abort_conditions" => vec!["旋钮未全部生效", "SoC 结温超过 temp_cap_c", "轮末快照与轮前不一致"],
    }
}

// ══════════════ 设备遥测解析 ══════════════

/// `sample_env.sh` 输出 → 功耗与温度统计。
///
/// 功耗在本机有两个坑, 都是实测踩出来的, 所以这里**不做任何美化**, 原始量与折算量
/// 一起落盘, 由上层决定用不用:
///
/// 1. **充电态量不到整机功耗**。插着 USB 时 USB 输入功率里有约一半是在给电池充电,
///    且充电电流随电量单调衰减 —— 这个衰减会被当成"功耗随时间下降"混进 A/B 比较。
///    所以 `battery_charging` 为真时 `power_w_mean` 一律 None。
/// 2. **`battery/power_now` 在本机单位是错的**, 读出过 777W。所以不像
///    `hwcond.rs::PowerSample::watt` 那样优先用它: 这里以 |V x I| 为准, `power_now`
///    只作为原始值记录, 并标一个 plausible 位 —— 不合理的数不进任何均值。
///
/// 另外把 `power_rail` / `battery_status` / `current_now` / `voltage_now` 原样留下,
/// 停充测量做好之后可以回头核这批数据。
pub fn env_stats(text: &str) -> PyVal {
    let (mut vi_watts, mut now_watts, mut usb_watts) = (vec![], vec![], vec![]);
    let (mut currents, mut voltages): (Vec<f64>, Vec<f64>) = (vec![], vec![]);
    let mut temps: Vec<f64> = vec![];
    let mut status = String::new();
    let mut capacity: Option<String> = None;
    let mut charge_suspended = false;
    let mut fan_state: Option<String> = None;
    let mut n = 0usize;

    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("#battery_status=") {
            status = v.trim().to_string();
            continue;
        }
        if let Some(v) = line.strip_prefix("#battery_capacity=") {
            let v = v.trim();
            capacity = (!v.is_empty()).then(|| v.to_string());
            continue;
        }
        if let Some(v) = line.strip_prefix("#charge_suspended=") {
            charge_suspended = v.trim() == "yes";
            continue;
        }
        if let Some(v) = line.strip_prefix("#fan_state=") {
            // 主动散热风扇自己耗电, 会进功耗读数。逐轮记下来, 好复核同一组对照的
            // 两臂是不是同一风扇状态 —— 风扇开关前后的数据不能混着比。
            let v = v.trim();
            fan_state = (!v.is_empty()).then(|| v.to_string());
            continue;
        }
        if !line.starts_with("ENV ") {
            continue;
        }
        // ENV <uptime> <batt_uv> <batt_ua> <batt_uw|NA> <usb_uv> <usb_ua> <热区> <毫摄氏度>
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        // 计数在解析之前 —— 解析失败的样本照样算采到过 (与旧版一致)
        n += 1;
        let (Ok(bv), Ok(bi)) = (parts[2].parse::<i64>(), parts[3].parse::<i64>()) else {
            continue;
        };
        let bp = if parts[4] == "NA" {
            None
        } else {
            match parts[4].parse::<i64>() {
                Ok(v) => Some(v),
                Err(_) => continue,
            }
        };
        let (Ok(uv), Ok(ui)) = (parts[5].parse::<i64>(), parts[6].parse::<i64>()) else {
            continue;
        };
        let Ok(t) = parts[8].parse::<i64>() else {
            continue;
        };

        if bv != 0 {
            voltages.push(bv as f64);
        }
        currents.push(bi as f64);
        if bv != 0 && bi != 0 {
            // Python 的 `bv / 1e6 * bi / 1e6` 是从左往右算的, 即 ((bv/1e6) * bi) / 1e6。
            // 写成 (bv/1e6) * (bi/1e6) 中间舍入位置就不一样, 实测 4 位小数上会差一个数。
            vi_watts.push((bv as f64 / 1e6 * bi as f64 / 1e6).abs());
        }
        if let Some(bp) = bp {
            if bp != 0 {
                now_watts.push((bp as f64).abs() / 1e6);
            }
        }
        if uv != 0 && ui != 0 {
            usb_watts.push((uv as f64 / 1e6 * ui as f64 / 1e6).abs());
        }
        if 0 < t && t < 100_000 {
            temps.push(t as f64 / 1000.0);
        }
    }

    let avg = |xs: &[f64]| -> Option<f64> {
        (!xs.is_empty()).then(|| py_round(py_sum(xs.iter().copied()) / xs.len() as f64, 4))
    };

    // on_battery 的判据与 phonefarm hwcond.rs::BatteryState::on_battery 一致:
    // status 不是 Charging **且** 电流确实是放电方向。两个判据都要看, 因为两个都会
    // 单独骗人 —— 停充之后有的内核仍写 "Not charging" 而不是 "Discharging",
    // 而 current_now 在充放平衡的瞬间会过零。
    let cur_mean = avg(&currents);
    let charging = status.to_lowercase() == "charging";
    let on_battery = !charging && cur_mean.is_some_and(|c| c < 0.0);

    let now_mean = avg(&now_watts);
    // 手机整机功耗合理区间。777W 这种读数只能说明单位不对, 不能进任何均值。
    let now_plausible = now_mean.is_some_and(|m| PLAUSIBLE_W.0 <= m && m <= PLAUSIBLE_W.1);

    let (mut power_w_mean, mut power_w_median) = (PyVal::Null, PyVal::Null);
    let reason: String = if !on_battery {
        format!(
            "非放电态 (status={}, current_now 均值={}): USB 输入功率里含给电池充电的部分, \
且充电电流随电量单调衰减, 量不到整机功耗。跑之前用 charge_suspend.sh 停充即可",
            if status.is_empty() { "?" } else { &status },
            PyVal::from(cur_mean).py_str()
        )
    } else if vi_watts.is_empty() {
        "读不到 voltage_now/current_now".to_string()
    } else {
        power_w_mean = PyVal::from(avg(&vi_watts));
        power_w_median = PyVal::Float(py_round(
            crate::pyjson::median_f64(&vi_watts).unwrap_or(0.0),
            4,
        ));
        format!(
            "放电态{}, 用 |V x I| (本机 power_now 单位有误, 不采用)",
            if charge_suspended { "(已停充)" } else { "" }
        )
    };

    pyobj! {
        "n_samples" => n,
        "battery_status" => status.clone(),
        "battery_capacity_pct" => capacity,
        "battery_charging" => charging,
        "charge_suspended" => charge_suspended,
        "on_battery" => on_battery,
        "power_source" => if on_battery { "battery" } else { "usb" },
        "fan_state" => fan_state,
        "power_rail" => "battery",
        "current_now_ua_mean" => cur_mean,
        "voltage_now_uv_mean" => avg(&voltages),
        "power_now_w_mean" => now_mean,
        "power_now_plausible" => now_plausible,
        "power_vi_w_mean" => avg(&vi_watts),
        "usb_input_w_mean" => avg(&usb_watts),
        "power_w_mean" => power_w_mean,
        "power_w_median" => power_w_median,
        "power_usable_reason" => reason,
        "soc_temp_max_c" => temps.iter().copied().reduce(f64::max),
        "soc_temp_mean_c" => (!temps.is_empty())
            .then(|| py_round(py_sum(temps.iter().copied()) / temps.len() as f64, 3)),
    }
}

// ══════════════ 判定 ══════════════

fn significant(c: &PyVal, alpha: f64) -> bool {
    c.get("perm_p_two_sided")
        .and_then(|v| v.as_f64())
        .is_some_and(|p| p < alpha)
}

/// `comparisons`: {指标: analyze compare 的输出}。返回判定结论。
///
/// comparisons 里 a 臂是对照 (旋钮已还原), b 臂是旋钮臂 —— 与 compare 的参数顺序
/// 一致, diff = b - a。
pub struct DecideCtx {
    pub temp_max_c: Option<f64>,
    pub temp_cap_c: f64,
    pub apply_ok: bool,
    pub snapshot_identical: bool,
    pub power_available: bool,
}

pub fn decide(comparisons: &PyVal, ctx: &DecideCtx) -> PyVal {
    let metrics = active_metrics(ctx.power_available);
    let k = metrics.len().max(1);
    let alpha_win = ALPHA / k as f64;

    // 三条作废线。注意这三条返回的 alpha_win 是**未舍入**的原值 —— 与旧版一致。
    if !ctx.apply_ok {
        return pyobj! {
            "verdict" => "ABORT",
            "reason" => "旋钮未全部生效, 本轮数据不算数",
            "alpha_win" => alpha_win,
        };
    }
    if let Some(tm) = ctx.temp_max_c {
        if tm > ctx.temp_cap_c {
            return pyobj! {
                "verdict" => "ABORT",
                "reason" => format!("SoC 结温 {}C 超过上限 {}C",
                                    PyVal::Float(tm).py_str(), PyVal::Float(ctx.temp_cap_c).py_str()),
                "temp_max_c" => tm,
                "alpha_win" => alpha_win,
            };
        }
    }
    if !ctx.snapshot_identical {
        return pyobj! {
            "verdict" => "ABORT",
            "reason" => "轮末设备快照与轮前不一致, 留痕",
            "alpha_win" => alpha_win,
        };
    }

    let (mut wins, mut regressions): (Vec<String>, Vec<String>) = (vec![], vec![]);
    let mut detail: Vec<(String, PyVal)> = Vec::new();
    for (m, direction) in &metrics {
        let c = comparisons.get(m);
        let missing = match c {
            None => true,
            Some(c) => {
                c.get("error").is_some()
                    || matches!(c.get("diff_mean"), None | Some(PyVal::Null))
            }
        };
        if missing {
            detail.push((m.to_string(), pyobj! { "status" => "缺数据" }));
            continue;
        }
        let c = c.expect("checked above");
        let diff = c.get("diff_mean").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let improved = if *direction == "lower" {
            diff < 0.0
        } else {
            diff > 0.0
        };
        let status = if improved && significant(c, alpha_win) {
            wins.push(m.to_string());
            "显著改善"
        } else if !improved && diff != 0.0 && significant(c, ALPHA) {
            regressions.push(m.to_string());
            "显著变差"
        } else {
            "无显著变化"
        };
        let g = |k: &str| c.get(k).cloned().unwrap_or(PyVal::Null);
        detail.push((
            m.to_string(),
            pyobj! {
                "status" => status,
                "diff_mean" => g("diff_mean"),
                "diff_pct" => g("diff_pct"),
                "p" => g("perm_p_two_sided"),
                "a_mean" => g("a_mean"),
                "b_mean" => g("b_mean"),
                "ci95" => g("ci95"),
            },
        ));
    }

    let keep = !wins.is_empty() && regressions.is_empty();
    pyobj! {
        "verdict" => if keep { "KEEP" } else { "REJECT" },
        "wins" => wins.clone(),
        "regressions" => regressions.clone(),
        "alpha_win" => py_round(alpha_win, 6),
        "alpha_regression" => ALPHA,
        "temp_max_c" => ctx.temp_max_c,
        "temp_cap_c" => ctx.temp_cap_c,
        "per_metric" => PyVal::Obj(detail),
        "reason" => if keep {
            format!("改善 {} 且无显著退化", wins.join(","))
        } else if !regressions.is_empty() {
            format!("存在显著退化: {}", regressions.join(","))
        } else {
            "没有任何指标显著改善".to_string()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::dumps;
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
    }

    const PROBE: &str = "# probe_sysparam v1
cpu.policies=0,3
cpu.policy0.avail_freqs=300000 1000000 2000000
cpu.policy0.avail_governors=schedutil performance
cpu.policy0.scaling_min_freq.cur=300000
cpu.policy0.scaling_max_freq.cur=2000000
cpu.policy0.scaling_governor.cur=schedutil
cpu.policy0.scaling_min_freq.writable=yes
cpu.policy0.scaling_min_freq.effect=live
cpu.policy0.scaling_max_freq.writable=yes
cpu.policy0.scaling_governor.writable=yes
cpu.policy3.avail_freqs=500000 3000000
cpu.policy3.scaling_min_freq.cur=500000
cpu.policy3.scaling_min_freq.writable=yes
cpu.policy3.scaling_min_freq.effect=rejected(wrote=3000000 readback=500000)
gpu.num_pwrlevels=4
gpu.min_pwrlevel.cur=3
gpu.max_pwrlevel.cur=0
gpu.min_pwrlevel.writable=yes
gpu.min_pwrlevel.effect=live
gpu.max_pwrlevel.writable=yes
gpu.devfreq.avail_freqs=220000000 1000000000
gpu.devfreq.min_freq.cur=220000000
gpu.devfreq.min_freq.writable=no
bus.DDR.boost_freq.cur=0
bus.DDR.hw_min_freq=200000
bus.DDR.hw_max_freq=5333000
bus.DDR.avail_freqs=200000 3200000 5333000
bus.DDR.boost_freq.writable=yes
bus.DDR.boost_freq.effect=live
setting.system.peak_refresh_rate.cur=120
setting.system.min_refresh_rate.cur=60
display.modes=fps=60,fps=120
thermal.cpu-1-0=42000
";

    fn wl_of(extra: &str) -> Whitelist {
        build_whitelist(&parse_probe(&format!("{PROBE}{extra}")))
    }
    fn has(wl: &Whitelist, pid: &str) -> bool {
        wl_get(wl, pid).is_some()
    }
    fn cand(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    // ── 白名单 ──

    #[test]
    fn only_writable_and_effective_nodes_get_in() {
        let wl = wl_of("");
        assert!(has(&wl, "cpu.policy0.scaling_min_freq"));
        // 写了被内核退回 → 不进
        assert!(!has(&wl, "cpu.policy3.scaling_min_freq"));
        // 不可写 → 不进
        assert!(!has(&wl, "gpu.devfreq.min_freq"));
    }

    /// 温控保护相关的东西, 哪怕探测报可写可生效也不准进白名单。
    #[test]
    fn thermal_nodes_can_never_enter() {
        let wl = wl_of("gpu.thermal_pwrlevel.writable=yes\ngpu.thermal_pwrlevel.effect=live\n");
        assert!(wl.iter().all(|(k, s)| !k.contains("thermal") && !s.path.contains("thermal")));
    }

    /// 风扇转速是系统参数的一种, 但它不进自动调参白名单 —— 只当测试条件记录。
    #[test]
    fn fan_and_charge_nodes_can_never_enter() {
        let wl = wl_of(
            "fan._sys_kernel_fan_speed.writable=yes\nfan._sys_kernel_fan_speed.effect=live\n\
charge.qcom_battery_charging_enabled.writable=yes\ncharge.qcom_battery_charging_enabled.effect=live\n",
        );
        assert!(wl.iter().all(|(k, s)| {
            !k.contains("fan") && !s.path.to_lowercase().contains("fan")
        }));
        assert!(denied("/sys/kernel/fan/speed"));
        assert!(!validate_candidate(&cand(&[("/sys/kernel/fan/speed", "3")]), &wl).0);
        assert!(!validate_candidate(
            &cand(&[("/sys/class/qcom-battery/charging_enabled", "0")]),
            &wl
        )
        .0);
    }

    /// 取值表只有一个值 = 没什么可调的, 不进白名单。
    #[test]
    fn single_valued_params_are_dropped() {
        let wl = build_whitelist(&parse_probe(
            "cpu.policies=0\ncpu.policy0.avail_freqs=1000000\n\
cpu.policy0.scaling_min_freq.cur=1000000\ncpu.policy0.scaling_min_freq.writable=yes\n",
        ));
        assert!(!has(&wl, "cpu.policy0.scaling_min_freq"));
    }

    #[test]
    fn refresh_rate_values_come_from_display_modes() {
        let wl = wl_of("");
        let s = wl_get(&wl, "setting.system.peak_refresh_rate").unwrap();
        assert_eq!(s.values, vec!["60", "120"]);
        assert_eq!(s.kind, "setting");
        assert_eq!(wl_get(&wl, "bus.DDR.boost_freq").unwrap().values,
                   vec!["200000", "3200000", "5333000"]);
    }

    #[test]
    fn fps_parser_takes_both_dumpsys_wordings() {
        assert_eq!(fps_list("60.000004 fps,144.00002 fps,120.00001 fps"), vec![60, 120, 144]);
        assert_eq!(fps_list("fps=60,fps=120,fps=120"), vec![60, 120]);
        assert!(fps_list("").is_empty());
    }

    /// 厂商键的取值语义没文档, 只认探测时真把活动刷新率改掉的那几档。
    #[test]
    fn vendor_refresh_mode_only_takes_modes_that_really_changed_fps() {
        let wl = wl_of(
            "setting.system.refresh_rate_mode.cur=0
setting.system.refresh_rate_mode.base_fps=120
setting.system.refresh_rate_mode.mode1_fps=60 (readback=1)
setting.system.refresh_rate_mode.mode2_fps=120 (readback=2)
setting.system.refresh_rate_mode.mode3_fps=144 (readback=9)
",
        );
        // mode1 真的把 120 变成了 60 → 收; mode2 fps 没变 → 不收;
        // mode3 fps 变了但回读对不上 (写 3 读回 9) → 不收
        assert_eq!(
            wl_get(&wl, "setting.system.refresh_rate_mode").unwrap().values,
            vec!["0", "1"]
        );
    }

    #[test]
    fn vendor_refresh_mode_is_absent_without_real_evidence() {
        // 一档都没真生效
        assert!(!has(
            &wl_of("setting.system.refresh_rate_mode.cur=0
setting.system.refresh_rate_mode.base_fps=120
setting.system.refresh_rate_mode.mode1_fps=120 (readback=1)
"),
            "setting.system.refresh_rate_mode"
        ));
        // 活动刷新率读不出来 → 一档都不许收, 不能按下标猜
        assert!(!has(
            &wl_of("setting.system.refresh_rate_mode.cur=0
setting.system.refresh_rate_mode.base_fps=
setting.system.refresh_rate_mode.mode1_fps= (readback=1)
setting.system.refresh_rate_mode.mode2_fps= (readback=2)
"),
            "setting.system.refresh_rate_mode"
        ));
        // readback=10 不是 mode 1 的回读 —— 子串匹配会把它当成生效
        assert!(!has(
            &wl_of("setting.system.refresh_rate_mode.cur=0
setting.system.refresh_rate_mode.base_fps=120
setting.system.refresh_rate_mode.mode1_fps=60 (readback=10)
"),
            "setting.system.refresh_rate_mode"
        ));
        // 厂商键回的是 auto 这种符号值时安静跳过, 不能让整条闭环崩在这儿
        assert!(!has(
            &wl_of("setting.system.refresh_rate_mode.cur=auto
setting.system.refresh_rate_mode.base_fps=120
setting.system.refresh_rate_mode.mode1_fps=60 (readback=1)
"),
            "setting.system.refresh_rate_mode"
        ));
    }

    // ── 校验 ──

    #[test]
    fn validate_accepts_legal_and_rejects_the_rest() {
        let wl = wl_of("");
        assert!(validate_candidate(&cand(&[("bus.DDR.boost_freq", "5333000")]), &wl).0);
        assert!(!validate_candidate(&[], &wl).0);

        let (ok, why) = validate_candidate(
            &cand(&[("/sys/class/thermal/thermal_zone0/mode", "disabled")]),
            &wl,
        );
        assert!(!ok && why.contains("不在白名单"), "{why}");

        let (ok, why) = validate_candidate(&cand(&[("bus.DDR.boost_freq", "9999999")]), &wl);
        assert!(!ok && why.contains("不在合法取值表"), "{why}");

        let (ok, why) = validate_candidate(
            &cand(&[
                ("cpu.policy0.scaling_min_freq", "2000000"),
                ("cpu.policy0.scaling_max_freq", "1000000"),
            ]),
            &wl,
        );
        assert!(!ok && why.contains("超过同簇 max"), "{why}");

        // 0 是最快档, 所以 max_pwrlevel 必须 <= min_pwrlevel
        let (ok, why) = validate_candidate(
            &cand(&[("gpu.min_pwrlevel", "0"), ("gpu.max_pwrlevel", "3")]),
            &wl,
        );
        assert!(!ok && why.contains("必须 <="), "{why}");

        assert!(!validate_candidate(
            &cand(&[
                ("setting.system.min_refresh_rate", "120"),
                ("setting.system.peak_refresh_rate", "60"),
            ]),
            &wl
        )
        .0);
    }

    /// 取值表的报错文本里带 Python 列表 repr, 证据里是逐字节比的。
    #[test]
    fn value_table_error_keeps_python_list_repr() {
        let wl = wl_of("");
        let (_, why) = validate_candidate(&cand(&[("bus.DDR.boost_freq", "9999999")]), &wl);
        assert_eq!(
            why,
            "bus.DDR.boost_freq=9999999 不在合法取值表 ['200000', '3200000', '5333000']..."
        );
    }

    // ── plan ──

    #[test]
    fn plan_is_tab_separated_and_deterministic() {
        let wl = wl_of("");
        let c = cand(&[
            ("cpu.policy0.scaling_min_freq", "1000000"),
            ("cpu.policy0.scaling_max_freq", "2000000"),
            ("setting.system.peak_refresh_rate", "60"),
        ]);
        let mut rev = c.clone();
        rev.reverse();
        let t1 = plan_text(&c, &wl);
        assert_eq!(t1, plan_text(&rev, &wl)); // 同一组参数, 逐字节相同
        for l in t1.lines().filter(|l| !l.starts_with('#')) {
            assert_eq!(l.split('\t').count(), 3, "{l}");
        }
        // 放宽上限的项排在抬高下限之前, 少踩一次内核的 min<=max 夹取
        assert!(t1.find("scaling_max_freq") < t1.find("scaling_min_freq"));
        assert!(t1.contains("setting\tsystem:peak_refresh_rate\t60"));
    }

    // ── 功耗温度解析 ──

    /// 本机 battery/power_now 单位有误 (读出过 777W), 所以以 |V x I| 为准。
    #[test]
    fn discharging_uses_volts_times_amps_not_power_now() {
        let s = env_stats(
            "#sample_env v1\n#battery_status=Discharging\n\
ENV 100.0 4000000 -1500000 777000000 0 0 cpu-1-0 41000\n\
ENV 102.0 4000000 -1500000 777000000 0 0 gpuss-0 43500\n",
        );
        assert_eq!(s.get("power_w_mean").unwrap().as_f64(), Some(6.0));
        assert_eq!(s.get("power_vi_w_mean").unwrap().as_f64(), Some(6.0));
        // power_now 原样记录, 但标成不合理, 不进任何均值
        assert_eq!(s.get("power_now_w_mean").unwrap().as_f64(), Some(777.0));
        assert_eq!(s.get("power_now_plausible"), Some(&PyVal::Bool(false)));
        assert_eq!(s.get("soc_temp_max_c").unwrap().as_f64(), Some(43.5));
        assert_eq!(s.get("n_samples"), Some(&PyVal::Int(2)));
    }

    /// `bv / 1e6 * bi / 1e6` 在 Python 里是从左往右算的, 即 `((bv/1e6) * bi) / 1e6`。
    /// 单个样本上两种写法看不出差别, 平均到四位小数上就差一个数 —— 这四行是实测出来的。
    #[test]
    fn watt_math_follows_python_left_to_right() {
        let s = env_stats(
            "#battery_status=Discharging
ENV 1.0 4000000 -1500000 NA 0 0 cpu-1-0 41000
ENV 2.0 3800000 -1000000 NA 0 0 cpu-1-0 41000
ENV 3.0 3800000 -1000000 NA 0 0 cpu-1-0 41000
ENV 4.0 3800000 771000 NA 0 0 cpu-1-0 41000
",
        );
        // python3: round(sum(abs(bv/1e6*bi/1e6) for ...)/4, 4) == 4.1325
        assert_eq!(s.get("power_vi_w_mean").unwrap().as_f64(), Some(4.1325));
    }

    #[test]
    fn plausible_power_now_is_still_only_recorded() {
        let s = env_stats(
            "#battery_status=Discharging\nENV 1.0 4000000 -1500000 5000000 0 0 cpu-1-0 40000\n",
        );
        assert_eq!(s.get("power_now_plausible"), Some(&PyVal::Bool(true)));
        assert_eq!(s.get("power_now_w_mean").unwrap().as_f64(), Some(5.0));
        assert_eq!(s.get("power_w_mean").unwrap().as_f64(), Some(6.0)); // 仍用 V x I
    }

    /// 充电态: USB 输入里含给电池充电的部分, 且充电电流随电量单调衰减 ——
    /// 那个衰减会被当成「功耗随时间下降」混进 A/B 比较。所以如实报 None。
    #[test]
    fn charging_makes_power_unusable() {
        let s = env_stats(
            "#battery_status=Charging\nENV 1.0 4000000 1000000 5000000 5000000 2000000 cpu-1-0 40000\n",
        );
        assert_eq!(s.get("power_w_mean"), Some(&PyVal::Null));
        assert_eq!(s.get("battery_charging"), Some(&PyVal::Bool(true)));
        assert_eq!(s.get("power_source"), Some(&PyVal::Str("usb".into())));
        assert!(matches!(s.get("power_usable_reason"), Some(PyVal::Str(r)) if r.contains("非放电态")));
        assert_eq!(s.get("usb_input_w_mean").unwrap().as_f64(), Some(10.0));
        assert_eq!(s.get("current_now_ua_mean").unwrap().as_f64(), Some(1000000.0));
    }

    #[test]
    fn sentinel_temps_and_missing_fan_are_not_guessed() {
        let s = env_stats(
            "#battery_status=Discharging\nENV 1.0 4000000 -1000000 NA 0 0 cpu-1-0 0\n\
ENV 2.0 4000000 -1000000 NA 0 0 cpu-1-0 200000\n",
        );
        assert_eq!(s.get("soc_temp_max_c"), Some(&PyVal::Null));
        let s = env_stats("#battery_status=Discharging\n#fan_state=\nENV 1.0 4000000 -1000000 NA 0 0 cpu-1-0 41000\n");
        assert_eq!(s.get("fan_state"), Some(&PyVal::Null));
        let s = env_stats("#battery_status=Discharging\n#fan_state=speed=3,fan1_input=4200\nENV 1.0 4000000 -1000000 NA 0 0 cpu-1-0 41000\n");
        assert_eq!(s.get("fan_state"), Some(&PyVal::Str("speed=3,fan1_input=4200".into())));
    }

    /// on_battery 要 status 与电流方向同时成立 —— 两个判据都会单独骗人。
    #[test]
    fn on_battery_needs_both_status_and_current_direction() {
        // 停充后内核仍写 "Not charging", 但电流确实在放 → 算放电态
        let s = env_stats(
            "#battery_status=Not charging\n#battery_capacity=88\n#charge_suspended=yes\n\
ENV 1.0 4000000 -1500000 NA 5000000 100000 cpu-1-0 41000\n",
        );
        assert_eq!(s.get("on_battery"), Some(&PyVal::Bool(true)));
        assert_eq!(s.get("power_source"), Some(&PyVal::Str("battery".into())));
        assert_eq!(s.get("power_w_mean").unwrap().as_f64(), Some(6.0));
        assert_eq!(s.get("battery_capacity_pct"), Some(&PyVal::Str("88".into())));
        assert!(matches!(s.get("power_usable_reason"), Some(PyVal::Str(r)) if r.contains("已停充")));

        // status 说 Not charging 但电流是正的 = 还在往电池里灌
        let s = env_stats("#battery_status=Not charging\nENV 1.0 4000000 771000 NA 0 0 cpu-1-0 41000\n");
        assert_eq!(s.get("on_battery"), Some(&PyVal::Bool(false)));
        assert_eq!(s.get("power_w_mean"), Some(&PyVal::Null));
        // 充放平衡的瞬间电流过零 —— 判不出放电态就不算
        let s = env_stats("#battery_status=Not charging\nENV 1.0 4000000 0 NA 0 0 cpu-1-0 41000\n");
        assert_eq!(s.get("on_battery"), Some(&PyVal::Bool(false)));
        assert_eq!(s.get("power_w_mean"), Some(&PyVal::Null));
    }

    // ── 判定 ──

    fn cmp_of(diff: f64, p: f64) -> PyVal {
        pyobj! {
            "metric" => "m", "diff_mean" => diff, "diff_pct" => diff / 100.0 * 100.0,
            "perm_p_two_sided" => p, "a_mean" => 100.0, "b_mean" => 100.0 + diff,
            "ci95" => PyVal::Null,
        }
    }
    fn base_ctx() -> DecideCtx {
        DecideCtx {
            temp_max_c: Some(42.0),
            temp_cap_c: 46.0,
            apply_ok: true,
            snapshot_identical: true,
            power_available: true,
        }
    }
    fn verdict_of(v: &PyVal) -> &str {
        match v.get("verdict") {
            Some(PyVal::Str(s)) => s,
            _ => "",
        }
    }

    #[test]
    fn keep_needs_a_significant_win_and_no_regression() {
        let c = pyobj! {
            "frame_p95" => cmp_of(-2.0, 0.001),
            "fps_mean" => cmp_of(0.0, 1.0),
            "power_w_mean" => cmp_of(0.0, 1.0),
        };
        let d = decide(&c, &base_ctx());
        assert_eq!(verdict_of(&d), "KEEP");
        assert_eq!(d.get("wins"), Some(&PyVal::List(vec![PyVal::Str("frame_p95".into())])));
    }

    /// 3 个指标时 alpha_win=0.0167, p=0.03 的改善还不够格; 同样的 p 在「变差」
    /// 一侧就算数 —— 两侧故意不对称。
    #[test]
    fn bonferroni_tightens_only_the_win_side() {
        let c = pyobj! {
            "frame_p95" => cmp_of(-2.0, 0.03),
            "fps_mean" => cmp_of(0.0, 1.0),
            "power_w_mean" => cmp_of(0.0, 1.0),
        };
        let d = decide(&c, &base_ctx());
        assert_eq!(verdict_of(&d), "REJECT");
        assert_eq!(d.get("alpha_win").unwrap().as_f64(), Some(py_round(0.05 / 3.0, 6)));

        let c = pyobj! {
            "frame_p95" => cmp_of(-2.0, 0.001),
            "fps_mean" => cmp_of(-1.0, 0.03),
            "power_w_mean" => cmp_of(0.0, 1.0),
        };
        let d = decide(&c, &base_ctx());
        assert_eq!(verdict_of(&d), "REJECT");
        assert_eq!(d.get("regressions"), Some(&PyVal::List(vec![PyVal::Str("fps_mean".into())])));
    }

    #[test]
    fn power_win_counts_too() {
        let c = pyobj! {
            "frame_p95" => cmp_of(0.0, 1.0),
            "fps_mean" => cmp_of(0.0, 1.0),
            "power_w_mean" => cmp_of(-0.5, 0.005),
        };
        let d = decide(&c, &base_ctx());
        assert_eq!(verdict_of(&d), "KEEP");
        assert_eq!(d.get("wins"), Some(&PyVal::List(vec![PyVal::Str("power_w_mean".into())])));
    }

    #[test]
    fn three_abort_lines_beat_any_improvement() {
        let c = pyobj! { "frame_p95" => cmp_of(-9.0, 0.0001) };
        for ctx in [
            DecideCtx { temp_max_c: Some(50.0), ..base_ctx() },
            DecideCtx { snapshot_identical: false, ..base_ctx() },
            DecideCtx { apply_ok: false, ..base_ctx() },
        ] {
            assert_eq!(verdict_of(&decide(&c, &ctx)), "ABORT");
        }
        let d = decide(&c, &DecideCtx { temp_max_c: Some(50.0), ..base_ctx() });
        assert!(matches!(d.get("reason"), Some(PyVal::Str(r)) if r.contains("超过上限")));
    }

    /// 功耗关掉时只剩两个指标 (Bonferroni 放宽到 0.025), 且功耗哪怕显著变差也不算退化
    /// —— 它根本不在判定里, 不能拿一个量不准的数去否决一个真实的帧时改善。
    #[test]
    fn power_out_of_verdict_drops_the_metric_entirely() {
        let ctx = DecideCtx { power_available: false, ..base_ctx() };
        let d = decide(&pyobj! { "frame_p95" => cmp_of(-2.0, 0.02), "fps_mean" => cmp_of(0.0, 1.0) }, &ctx);
        assert_eq!(d.get("alpha_win").unwrap().as_f64(), Some(0.025));
        assert_eq!(verdict_of(&d), "KEEP");

        let d = decide(
            &pyobj! {
                "frame_p95" => cmp_of(-2.0, 0.001),
                "fps_mean" => cmp_of(0.0, 1.0),
                "power_w_mean" => cmp_of(5.0, 0.0001),
            },
            &ctx,
        );
        assert_eq!(verdict_of(&d), "KEEP");
        assert_eq!(d.get("regressions"), Some(&PyVal::List(vec![])));
        assert!(d.get("per_metric").unwrap().get("power_w_mean").is_none());
    }

    #[test]
    fn rule_doc_records_reachability_and_why_power_is_out() {
        let r = rule_doc(46.0, 5, false, "功耗不计入判定, 仅记录供参考 (充电态)");
        assert_eq!(r.get("power_in_verdict"), Some(&PyVal::Bool(false)));
        assert_eq!(r.get("n_metrics"), Some(&PyVal::Int(2)));
        assert!(matches!(r.get("power_note"), Some(PyVal::Str(s)) if s.contains("充电态")));
        // 4v4 最小可达 p = 2/C(8,4) = 0.0286, 够不着 alpha_win=0.0167
        assert_eq!(rule_doc(46.0, 4, true, "").get("reachable"), Some(&PyVal::Bool(false)));
        // 5v5 最小可达 p = 2/C(10,5) = 0.0079, 够得着
        let r = rule_doc(46.0, 5, true, "");
        assert_eq!(r.get("reachable"), Some(&PyVal::Bool(true)));
        assert!((r.get("min_reachable_p").unwrap().as_f64().unwrap() - 2.0 / 252.0).abs() < 1e-12);
        assert_eq!(rule_doc(46.0, 1, true, "").get("min_reachable_p"), Some(&PyVal::Null));
    }

    // ── 真机录制对照 ──

    /// autoloop 是 `json.dump(..., sort_keys=True)`, 它**递归**排序, 所以对照前也递归排一遍。
    fn sort_deep(v: PyVal) -> PyVal {
        match v {
            PyVal::Obj(mut kvs) => {
                kvs = kvs.into_iter().map(|(k, x)| (k, sort_deep(x))).collect();
                kvs.sort_by(|a, b| a.0.cmp(&b.0));
                PyVal::Obj(kvs)
            }
            PyVal::List(xs) => PyVal::List(xs.into_iter().map(sort_deep).collect()),
            other => other,
        }
    }

    /// 对照源: 两次真机探测的 probe.txt → whitelist.json (旧 Python 版落的盘)。
    #[test]
    fn whitelist_matches_recorded_golden() {
        let mut checked = 0;
        for run in ["probe3", "genshin1"] {
            let dir = repo_root().join("loop_v1/runs_sysparam").join(run);
            let (probe, golden) = (dir.join("probe.txt"), dir.join("whitelist.json"));
            if !probe.exists() || !golden.exists() {
                continue;
            }
            let wl = build_whitelist(&parse_probe(&std::fs::read_to_string(&probe).unwrap()));
            let want = crate::pyjson::loads(&std::fs::read_to_string(&golden).unwrap()).unwrap();
            assert_eq!(
                dumps(&sort_deep(whitelist_to_pyval(&wl))),
                dumps(&sort_deep(want)),
                "{run} 的白名单与录制不一致"
            );
            checked += 1;
        }
        assert_eq!(checked, 2, "两份录制都要比到");
    }

    /// 对照源: 真机跑出来的 result.json。三组候选里 cand1/cand2 在调 decide 之前就被
    /// autoloop 自己拦下了 (温度、旋钮没生效), 只有 cand3 真走到了判定 —— 它留痕作废,
    /// 而且 alpha_win 落的是**未舍入**的 0.016666666666666666, 正好钉住作废那三条线
    /// 与正常路径的取整不一样这件事。
    #[test]
    fn decide_matches_recorded_result() {
        let f = repo_root().join("loop_v1/runs_sysparam/genshin1/gen1/cand3/result.json");
        let v = crate::pyjson::loads(&std::fs::read_to_string(&f).unwrap()).unwrap();
        let got = decide(
            v.get("comparisons").unwrap(),
            &DecideCtx {
                temp_max_c: v.get("temp_max_c").and_then(|x| x.as_f64()),
                temp_cap_c: v.get("temp_cap_c").and_then(|x| x.as_f64()).unwrap(),
                apply_ok: matches!(v.get("apply_ok"), Some(PyVal::Bool(true))),
                // snapshot_check.ours_identical = false 就是这一轮作废的原因
                snapshot_identical: false,
                power_available: true,
            },
        );
        for k in ["verdict", "reason", "alpha_win"] {
            assert_eq!(
                dumps(got.get(k).unwrap()),
                dumps(v.get(k).unwrap()),
                "{k} 与录制不一致"
            );
        }
        // 留痕这条线走的是未舍入的 alpha_win
        assert_eq!(got.get("alpha_win").unwrap().as_f64(), Some(0.05 / 3.0));
    }
}
