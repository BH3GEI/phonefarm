//! loop_v1 的解析与归因: ftrace 文本 → 帧时序指标 → "这一帧的时间花在哪类开销上"。
//!
//! 由 `loop_v1/tools/parse_trace.py` 与 `loop_v1/tools/attribute.py` 逐行搬过来,
//! 口径与输出字节完全一致 (对照测试见本文件末尾, 用的是 `loop_v1/runs/` 里的真实录制)。
//! **不碰设备**, 因此可对着归档的 trace 离线重跑, 结果逐字节可复现 (判据 5 的可回放性)。
//!
//! 指标口径
//! --------
//! frame_ms      : 一帧的墙钟时长。**一帧不等于一次 GPU 提交** —— 原神在本机每帧发
//!                 2 次 cmdbatch (实测间隔严格交替 ~5ms / ~28ms, 每对之和 33.3ms = 30fps)。
//!                 所以先由 [`detect_submits_per_frame`] 从数据自检出每帧几次提交, 再按组算。
//! gpu_active_ms : 一帧内所有提交的 active 字段之和, 单位换算自 19.2MHz GPU tick。
//!                 = GPU 真正在跑这一帧命令的时间, 不含排队等待。
//! queue_ms      : 提交 → 退役的墙钟延迟减去 gpu_active, 即排队 + 同步开销。
//! avg_bw        : kgsl_buslevel 事件的 avg_bw 字段, GPU 侧总线带宽投票 (MB/s 量级)。
//!                 这是直接观测量, 不是从频率反推的。
//!
//! 为什么用提交节奏而不是 vsync: vsync (encoder_vblank_callback) 是显示刷新节奏,
//! 恒定跟着面板刷新率跳, 游戏 30fps 锁帧时它照样 60/120 次每秒; 提交节奏才反映实际出帧速度。

use crate::pyjson::{dumps, median_ints, py_round, PyVal};
use crate::pyobj;
use regex::Regex;
use std::collections::HashMap;

/// ftrace 行首: `  UnityGfxDeviceW-32074   [005] ..... 1044964.694371: event_name: rest`
const LINE_RE: &str =
    r"^\s*(?P<comm>.+?)-(?P<tid>\d+)\s+\[(?P<cpu>\d+)\]\s+\S+\s+(?P<ts>\d+\.\d+):\s+(?P<event>\w+):\s*(?P<rest>.*)$";
const KV_RE: &str = r"(\w+)=(-?\d+)";

/// 实测: Δticks / Δ墙钟 = 19.21e6, 即 XO 19.2MHz
const GPU_TICK_HZ: f64 = 19_200_000.0;

const ACF_POSITIVE_GATE: f64 = 0.30;

const EVENTS: [&str; 8] = [
    "adreno_cmdbatch_submitted",
    "adreno_cmdbatch_retired",
    "kgsl_buslevel",
    "kgsl_pwrlevel",
    "kgsl_gpu_frequency",
    "kgsl_thermal_constraint",
    "kgsl_clock_throttling",
    "kgsl_bcl_clock_throttling",
];

pub const DEFAULT_RENDER_COMM: &str = "UnityGfxDeviceW";

// ══════════════ 基础统计 ══════════════

/// 变异系数 std/mean。空或均值为 0 时返回正无穷, 让它在择优里自然出局。
fn cv(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return f64::INFINITY;
    }
    let mu = xs.iter().sum::<f64>() / xs.len() as f64;
    if mu <= 0.0 {
        return f64::INFINITY;
    }
    let var = xs.iter().map(|x| (x - mu).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    var.sqrt() / mu
}

/// 滞后 lag 的样本自相关 (Pearson)。样本不足或方差为 0 时返回 0。
fn acf(xs: &[f64], lag: usize) -> f64 {
    if xs.len() < lag + 3 {
        return 0.0;
    }
    let n = xs.len() - lag;
    let (a, b) = (&xs[..n], &xs[lag..lag + n]);
    let nf = n as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / nf, b.iter().sum::<f64>() / nf);
    let num: f64 = (0..n).map(|i| (a[i] - ma) * (b[i] - mb)).sum();
    let da = a.iter().map(|v| (v - ma).powi(2)).sum::<f64>().sqrt();
    let db = b.iter().map(|v| (v - mb).powi(2)).sum::<f64>().sqrt();
    if da > 0.0 && db > 0.0 {
        num / (da * db)
    } else {
        0.0
    }
}

/// 线性插值分位数 (与 numpy 默认口径一致), 空列表返回 None。
pub fn pct(xs: &[f64], p: f64) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if s.len() == 1 {
        return Some(s[0]);
    }
    let k = (s.len() - 1) as f64 * p;
    let lo = k.trunc() as usize;
    let hi = (lo + 1).min(s.len() - 1);
    Some(s[lo] + (s[hi] - s[lo]) * (k - lo as f64))
}

/// 每帧几次提交的自检结果: 判据 (自相关) 与旁证 (归一化 CV) 都留着。
pub struct SpfScores {
    /// (lag, 自相关系数)
    pub acf: Vec<(usize, f64)>,
    /// (每帧提交数, 归一化 CV)
    pub cv_norm: Vec<(usize, f64)>,
}

impl SpfScores {
    fn to_pyval(&self) -> PyVal {
        let m = |xs: &[(usize, f64)]| {
            PyVal::Obj(
                xs.iter()
                    .map(|(k, v)| (k.to_string(), PyVal::Float(py_round(*v, 5))))
                    .collect(),
            )
        };
        pyobj! { "acf" => m(&self.acf), "cv_norm" => m(&self.cv_norm) }
    }
}

/// 从提交间隔序列自检"每帧几次提交"——用自相关, 不用方差。
///
/// 为什么不用"哪个 N 的方差最小"
/// ------------------------------
/// 试过, 不稳。把 N 个间隔求和当一帧, 均值按 N 涨而标准差只按 sqrt(N) 涨, 所以裸 CV
/// 天然随 N 单调下降, 永远选出最大的 N。按 sqrt(N) 归一化能缓解, 但帧内间隔本身是
/// 相关的 (短-长-短-长), 不满足独立假设, 残余偏置仍在: 实测 night1 的归一化 CV
/// N=2 是 0.155 / N=6 是 0.150, 只差 3.5% 能靠容差救回来; 到了 night4 变成
/// N=2 是 0.174 / N=6 是 0.147, 差 18%, 容差救不回来, 于是把周期判成 6,
/// 帧时间整整虚高 3 倍 (100.6ms 而不是 33.5ms)。
///
/// 自相关直接量周期性
/// ------------------
/// 交替的短-长序列, 滞后 1 是强负相关、滞后 2 是强正相关。这是周期本身的性质,
/// 与"求和平均掉多少方差"无关, 所以不存在上面那种偏置。实测四个 run 全部给出
/// lag1 ≈ -0.93, lag2 ≈ +0.90 —— 判别余量极大, 不靠任何容差。
///
/// 判据: 取**最小**的、自相关为正且超过阈值的滞后。周期的倍数也会是正相关
/// (lag4/lag6 同样正), 取最小的那个才是真周期。一个正峰都没有 = 看不出周期性,
/// 退回"一次提交一帧"并如实记录。
pub fn detect_submits_per_frame(gaps: &[f64], max_n: usize) -> (usize, SpfScores) {
    let acf_scores: Vec<(usize, f64)> = (1..=max_n)
        .filter(|l| gaps.len() >= l + 3)
        .map(|l| (l, acf(gaps, l)))
        .collect();
    let mut cv_norm: Vec<(usize, f64)> = Vec::new();
    for n in 1..=max_n {
        if gaps.len() < n * 3 {
            continue;
        }
        let groups: Vec<f64> = (0..=gaps.len() - n)
            .step_by(n)
            .map(|i| gaps[i..i + n].iter().sum::<f64>())
            .collect();
        cv_norm.push((n, cv(&groups) * (n as f64).sqrt()));
    }
    let best = acf_scores
        .iter()
        .find(|(_, r)| *r >= ACF_POSITIVE_GATE)
        .map(|(l, _)| *l)
        .unwrap_or(1);
    (
        best,
        SpfScores {
            acf: acf_scores,
            cv_norm,
        },
    )
}

// ══════════════ 解析 ══════════════

/// 保序的 meta 字典 (Python dict 语义: 重复键就地覆盖, 不改位置)。
#[derive(Default)]
pub struct Meta(Vec<(String, String)>);

impl Meta {
    fn set(&mut self, k: &str, v: &str) {
        match self.0.iter_mut().find(|(ek, _)| ek == k) {
            Some(slot) => slot.1 = v.to_string(),
            None => self.0.push((k.to_string(), v.to_string())),
        }
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.as_str())
    }
    fn to_pyval(&self) -> PyVal {
        PyVal::Obj(
            self.0
                .iter()
                .map(|(k, v)| (k.clone(), PyVal::Str(v.clone())))
                .collect(),
        )
    }
}

pub struct Parsed {
    pub meta: Meta,
    pub render_comm: String,
    pub render_ctxs: Vec<i64>,
    pub span_s: f64,
    pub n_submits: usize,
    pub submits_per_frame: usize,
    pub spf_scores: SpfScores,
    pub fps_mean: Option<f64>,
    pub frame_ms: Vec<f64>,
    pub gpu_active_ms: Vec<f64>,
    pub queue_ms: Vec<f64>,
    /// (t, avg_bw, pwrlevel)
    pub buslevel: Vec<(f64, i64, i64)>,
    pub pwrlevel: Vec<(f64, i64)>,
    pub gpufreq: Vec<(f64, i64)>,
    pub thermal_events: Vec<(f64, String)>,
}

struct Retire {
    wall: f64,
    active: i64,
}

/// `ctx=41 ts=7151 active=3146` 这类尾串 → 字典 (只取整数字段)。
fn kv(re: &Regex, rest: &str) -> HashMap<String, i64> {
    let mut m = HashMap::new();
    for c in re.captures_iter(rest) {
        if let Ok(v) = c[2].parse::<i64>() {
            m.insert(c[1].to_string(), v);
        }
    }
    m
}

pub fn parse(text: &str, render_comm: &str) -> Parsed {
    let line_re = Regex::new(LINE_RE).expect("LINE_RE");
    let kv_re = Regex::new(KV_RE).expect("KV_RE");

    let mut meta = Meta::default();
    let mut submits: Vec<(f64, i64, i64)> = Vec::new();
    let mut retires: HashMap<(i64, i64), Retire> = HashMap::new();
    let mut buslevel = Vec::new();
    let mut pwrlevel = Vec::new();
    let mut gpufreq = Vec::new();
    let mut thermal_events = Vec::new();
    let mut render_ctxs: Vec<i64> = Vec::new();

    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix("#loop_v1_meta") {
            for tok in rest.split_whitespace() {
                if let Some((k, v)) = tok.split_once('=') {
                    meta.set(k, v);
                }
            }
            continue;
        }
        if raw.starts_with('#') {
            continue;
        }
        // 预筛: 不含任何目标事件名的行, 正则匹配上了也会因 event 不在白名单被丢掉,
        // 先用子串判一刀, 22MB trace 省掉绝大部分正则调用, 结果完全等价。
        if !raw.contains("adreno_cmdbatch") && !raw.contains("kgsl_") {
            continue;
        }
        let Some(m) = line_re.captures(raw) else {
            continue;
        };
        let ev = &m["event"];
        if !EVENTS.contains(&ev) {
            continue;
        }
        let Ok(wall) = m["ts"].parse::<f64>() else {
            continue;
        };
        let f = kv(&kv_re, &m["rest"]);

        match ev {
            "adreno_cmdbatch_submitted" => {
                if m["comm"].trim() == render_comm {
                    if let (Some(&ctx), Some(&ts)) = (f.get("ctx"), f.get("ts")) {
                        if !render_ctxs.contains(&ctx) {
                            render_ctxs.push(ctx);
                        }
                        submits.push((wall, ctx, ts));
                    }
                }
            }
            "adreno_cmdbatch_retired" => {
                if let (Some(&ctx), Some(&ts)) = (f.get("ctx"), f.get("ts")) {
                    retires.insert(
                        (ctx, ts),
                        Retire {
                            wall,
                            active: f.get("active").copied().unwrap_or(0),
                        },
                    );
                }
            }
            "kgsl_buslevel" => buslevel.push((
                wall,
                f.get("avg_bw").copied().unwrap_or(0),
                f.get("pwrlevel").copied().unwrap_or(-1),
            )),
            "kgsl_pwrlevel" => pwrlevel.push((wall, f.get("pwrlevel").copied().unwrap_or(-1))),
            "kgsl_gpu_frequency" => {
                // 字段名各内核版本不一, 取第一个像频率的值。Python 用的是 `or` 链,
                // 所以值为 0 也会继续往下找 —— 这里照搬。
                let freq = [f.get("gpu_freq"), f.get("freq"), f.get("new_freq")]
                    .into_iter()
                    .flatten()
                    .copied()
                    .find(|v| *v != 0)
                    .unwrap_or(0);
                gpufreq.push((wall, freq));
            }
            _ => thermal_events.push((wall, ev.to_string())),
        }
    }

    submits.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    render_ctxs.sort_unstable();

    // ── 提交间隔 → 自检每帧提交数 → 按帧分组 ──
    let gaps: Vec<f64> = submits
        .windows(2)
        .map(|w| (w[1].0 - w[0].0) * 1000.0)
        .collect();
    let (spf, spf_scores) = detect_submits_per_frame(&gaps, 6);

    // 帧时长 = 每 spf 个间隔一组求和。组边界同时用来聚合该帧的 GPU 活跃时长。
    let mut frame_ms = Vec::new();
    let mut gpu_active_ms = Vec::new();
    let mut queue_ms = Vec::new();
    if gaps.len() >= spf {
        for i in (0..=gaps.len() - spf).step_by(spf) {
            frame_ms.push(gaps[i..i + spf].iter().sum::<f64>());
            let mut act_sum = 0.0;
            let mut q_sum = 0.0;
            let mut matched = 0usize;
            for &(wall, ctx, ts) in &submits[i..(i + spf).min(submits.len())] {
                let Some(r) = retires.get(&(ctx, ts)) else {
                    continue;
                };
                let a = r.active as f64 / GPU_TICK_HZ * 1000.0;
                act_sum += a;
                q_sum += ((r.wall - wall) * 1000.0 - a).max(0.0);
                matched += 1;
            }
            if matched > 0 {
                gpu_active_ms.push(act_sum);
                queue_ms.push(q_sum);
            }
        }
    }

    let span = if submits.len() >= 2 {
        submits[submits.len() - 1].0 - submits[0].0
    } else {
        0.0
    };

    Parsed {
        fps_mean: if span > 0.0 {
            Some(py_round(frame_ms.len() as f64 / span, 3))
        } else {
            None
        },
        meta,
        render_comm: render_comm.to_string(),
        render_ctxs,
        span_s: py_round(span, 4),
        n_submits: submits.len(),
        submits_per_frame: spf,
        spf_scores,
        frame_ms,
        gpu_active_ms,
        queue_ms,
        buslevel,
        pwrlevel,
        gpufreq,
        thermal_events,
    }
}

impl Parsed {
    /// `--full`: 逐帧序列全出。
    pub fn to_pyval(&self) -> PyVal {
        pyobj! {
            "meta" => self.meta.to_pyval(),
            "render_comm" => self.render_comm.clone(),
            "render_ctxs" => self.render_ctxs.clone(),
            "span_s" => self.span_s,
            "n_submits" => self.n_submits,
            "submits_per_frame" => self.submits_per_frame,
            "spf_scores" => self.spf_scores.to_pyval(),
            "n_matched_retires" => self.gpu_active_ms.len(),
            "fps_mean" => self.fps_mean,
            "frame_ms" => self.frame_ms.clone(),
            "gpu_active_ms" => self.gpu_active_ms.clone(),
            "queue_ms" => self.queue_ms.clone(),
            "buslevel" => PyVal::List(self.buslevel.iter().map(|(t, bw, pl)| {
                pyobj!{ "t" => py_round(*t, 4), "avg_bw" => *bw, "pwrlevel" => *pl }
            }).collect()),
            "pwrlevel" => PyVal::List(self.pwrlevel.iter().map(|(t, pl)| {
                pyobj!{ "t" => py_round(*t, 4), "pwrlevel" => *pl }
            }).collect()),
            "gpufreq" => PyVal::List(self.gpufreq.iter().map(|(t, f)| {
                pyobj!{ "t" => py_round(*t, 4), "freq" => *f }
            }).collect()),
            "thermal_events" => PyVal::List(self.thermal_events.iter().map(|(t, e)| {
                pyobj!{ "t" => py_round(*t, 4), "event" => e.clone() }
            }).collect()),
        }
    }

    /// 把逐帧序列压成一行可比的账。每个字段都是确定性函数, 无随机。
    pub fn summarize(&self) -> PyVal {
        let (f, g, q) = (&self.frame_ms, &self.gpu_active_ms, &self.queue_ms);
        let bw: Vec<i64> = self.buslevel.iter().map(|(_, b, _)| *b).collect();
        let r4 = |xs: &[f64], p: f64| pct(xs, p).map(|v| py_round(v, 4));
        let mean4 = |xs: &[f64]| {
            (!xs.is_empty()).then(|| py_round(xs.iter().sum::<f64>() / xs.len() as f64, 4))
        };
        pyobj! {
            "span_s" => self.span_s,
            "n_frames" => f.len(),
            "n_submits" => self.n_submits,
            "submits_per_frame" => self.submits_per_frame,
            "spf_scores" => self.spf_scores.to_pyval(),
            "fps_mean" => self.fps_mean,
            "frame_p50" => r4(f, 0.50),
            "frame_p95" => r4(f, 0.95),
            "frame_p99" => r4(f, 0.99),
            "frame_mean" => mean4(f),
            "gpu_active_p50" => r4(g, 0.50),
            "gpu_active_p95" => r4(g, 0.95),
            "gpu_active_mean" => mean4(g),
            "queue_p50" => r4(q, 0.50),
            "queue_p95" => r4(q, 0.95),
            "bw_median" => median_ints(&bw).unwrap_or(PyVal::Null),
            "bw_max" => bw.iter().max().copied(),
            "n_buslevel_events" => bw.len(),
            "n_thermal_events" => self.thermal_events.len(),
            "ddr_cur_khz" => self.meta.get("ddr_cur_khz").map(String::from),
            "ddr_boost_khz" => self.meta.get("ddr_boost_khz").map(String::from),
            "llcc_boost_khz" => self.meta.get("llcc_boost_khz").map(String::from),
            "thermal_pwrlevel" => self.meta.get("thermal_pwrlevel").map(String::from),
        }
    }
}

// ══════════════ 归因 (判据 2) ══════════════
//
// 本机没有 Perfetto 的 gpu.renderstages (高通未在该驱动注册 GPU producer), 所以做不到
// render pass 级归因。目标允许的另一半口径是"哪类开销", 这里给的就是这个, 并且每条
// 结论都挂着产生它的那个实测量, 不做无据推断。
//
// 判定树 (顺序即优先级, 先命中先返回)
//   1. 热降频        : trace 里出现 kgsl_thermal_constraint / kgsl_clock_throttling
//                      → 再快的 GPU 也没用, 先解热
//   2. GPU 计算受限  : gpu_active / frame_period > 0.90
//   3. 限帧器封顶    : frame_period 贴着某个常见帧率上限的 ±2%, 且 GPU 占比 < 0.90
//                      → 帧时间被游戏自己的限帧器钉住, **此时 p50 物理上不可能降低**,
//                        能动的只有抖动 (p95-p50) 和每帧 GPU 工作量 (省功耗与热预算)
//   4. 提交/同步受限 : queue / frame_period > 0.25
//   5. 其余          : 无单一主因
//
// 带宽压力是正交的一维, 单独给, 不进上面的判定树: GPU 侧对总线的带宽投票
// (kgsl_buslevel.avg_bw) 与 DDR 实际运行频率一起看, DDR 跑在上限的比例越高说明访存越吃紧。
// 这一维决定"带宽旋钮值不值得拧"。

const FPS_CAPS: [i64; 6] = [30, 45, 60, 90, 120, 144];
const GPU_BOUND_RATIO: f64 = 0.90;
const QUEUE_BOUND_RATIO: f64 = 0.25;
const CAP_TOLERANCE: f64 = 0.02;

/// 帧周期是否贴着某个常见限帧上限。贴得上说明是软件限帧, 不是硬件跑不动。
pub fn detect_fps_cap(frame_p50_ms: f64) -> Option<i64> {
    FPS_CAPS.into_iter().find(|cap| {
        let target = 1000.0 / *cap as f64;
        (frame_p50_ms - target).abs() / target <= CAP_TOLERANCE
    })
}

/// Python `x or 0.0` 的数字版: 缺失、null、0 —— 以及 **-0.0** —— 都落到默认值。
/// `bool(-0.0)` 在 Python 里是 False, 照搬这一条, 否则 `-0.0` 会原样写进 JSON。
fn num_or_zero(s: &serde_json::Value, k: &str) -> f64 {
    match s.get(k).and_then(|v| v.as_f64()) {
        Some(v) if v != 0.0 => v,
        _ => 0.0,
    }
}

/// Python `float(s.get(k) or 0)`: 值是字符串时按十进制解析。
///
/// 解析不了就是数据坏了。Python 在这里会抛 ValueError 把整轮打掉, 这边照样返回错误
/// —— 悄悄填个 0 等于凭空造一个测量值, 比整轮失败更糟。
fn str_num_or_zero(s: &serde_json::Value, k: &str) -> Result<f64, String> {
    match s.get(k) {
        Some(serde_json::Value::String(t)) if !t.is_empty() => t
            .trim()
            .parse()
            .map_err(|_| format!("{k} 不是数字: {t:?}")),
        Some(v) => Ok(match v.as_f64() {
            Some(x) if x != 0.0 => x,
            _ => 0.0,
        }),
        None => Ok(0.0),
    }
}

pub fn attribute(s: &serde_json::Value) -> Result<PyVal, String> {
    let fp50 = num_or_zero(s, "frame_p50");
    let fp95 = num_or_zero(s, "frame_p95");
    let gpu = num_or_zero(s, "gpu_active_mean");
    let queue = num_or_zero(s, "queue_p50");
    let n_thermal = s
        .get("n_thermal_events")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let gpu_share = if fp50 != 0.0 { gpu / fp50 } else { 0.0 };
    let queue_share = if fp50 != 0.0 { queue / fp50 } else { 0.0 };
    let jitter_ms = fp95 - fp50;
    let cap = detect_fps_cap(fp50);

    // ── 带宽维 (正交) ──
    let bw_med = s.get("bw_median");
    let bw_max = s.get("bw_max");
    let (med_f, max_f) = (
        bw_med.and_then(|v| v.as_f64()).unwrap_or(0.0),
        bw_max.and_then(|v| v.as_f64()).unwrap_or(0.0),
    );
    let headroom = if med_f != 0.0 && max_f != 0.0 {
        PyVal::Float(py_round((1.0 - med_f / max_f) * 100.0, 2))
    } else {
        PyVal::Null
    };
    let bw = pyobj! {
        "ddr_cur_khz" => str_num_or_zero(s, "ddr_cur_khz")?,
        "ddr_boost_khz" => str_num_or_zero(s, "ddr_boost_khz")?,
        "gpu_bus_vote_median" => bw_med.map(PyVal::from).unwrap_or(PyVal::Null),
        "gpu_bus_vote_max" => bw_max.map(PyVal::from).unwrap_or(PyVal::Null),
        "bus_vote_headroom_pct" => headroom,
    };

    // ── 主因判定 ──
    let (verdict, why, actionable) = if n_thermal > 0 {
        (
            "热降频受限".to_string(),
            format!("trace 内出现 {n_thermal} 次 kgsl 热/降频事件"),
            "先解热: 降低每帧工作量或放宽散热, 提频无效".to_string(),
        )
    } else if gpu_share > GPU_BOUND_RATIO {
        (
            "GPU 计算受限".to_string(),
            format!(
                "GPU 活跃 {gpu:.2}ms 占帧周期 {fp50:.2}ms 的 {:.1}% (>{:.0}%)",
                gpu_share * 100.0,
                GPU_BOUND_RATIO * 100.0
            ),
            "减少每帧 GPU 工作量 (分辨率/着色/overdraw) 才有收益".to_string(),
        )
    } else if let Some(cap) = cap {
        (
            format!("限帧器封顶 @{cap}fps"),
            format!(
                "帧周期 {fp50:.2}ms 贴着 {cap}fps 的 {:.2}ms (±{:.0}%), 而 GPU 只用掉 {:.1}%, 余量 {:.2}ms",
                1000.0 / cap as f64,
                CAP_TOLERANCE * 100.0,
                gpu_share * 100.0,
                fp50 - gpu
            ),
            format!(
                "p50 被软件限帧钉死, 物理上降不下去。可动的只有: 抖动 p95-p50={jitter_ms:.2}ms, 以及每帧 GPU 工作量 {gpu:.2}ms (省功耗/热预算)"
            ),
        )
    } else if queue_share > QUEUE_BOUND_RATIO {
        (
            "提交/同步受限".to_string(),
            format!(
                "排队开销 {queue:.2}ms 占帧周期 {:.1}% (>{:.0}%)",
                queue_share * 100.0,
                QUEUE_BOUND_RATIO * 100.0
            ),
            "查 CPU 侧提交节奏与 fence 等待".to_string(),
        )
    } else {
        (
            "无单一主因".to_string(),
            format!(
                "GPU 占比 {:.1}%, 排队占比 {:.1}%, 无热事件, 帧周期不贴任何常见限帧上限",
                gpu_share * 100.0,
                queue_share * 100.0
            ),
            "需要更细的归因面才能定位".to_string(),
        )
    };

    Ok(pyobj! {
        "verdict" => verdict,
        "evidence" => why,
        "actionable" => actionable,
        "measures" => pyobj!{
            "frame_p50_ms" => py_round(fp50, 4),
            "frame_p95_ms" => py_round(fp95, 4),
            "jitter_p95_minus_p50_ms" => py_round(jitter_ms, 4),
            "gpu_active_mean_ms" => py_round(gpu, 4),
            "gpu_share_pct" => py_round(gpu_share * 100.0, 2),
            "queue_p50_ms" => py_round(queue, 4),
            "queue_share_pct" => py_round(queue_share * 100.0, 2),
            "n_thermal_events" => n_thermal,
            "detected_fps_cap" => cap,
        },
        "bandwidth" => bw,
    })
}

// ══════════════ 子命令 ══════════════

const PARSE_USAGE: &str = "用法: phonefarm parse-trace <trace.txt> [--full] [--comm 线程名]";
const ATTR_USAGE: &str = "用法: phonefarm attribute <summary.json>";

pub fn run_parse_trace(args: &[String]) -> i32 {
    let mut path: Option<&str> = None;
    let mut comm = DEFAULT_RENDER_COMM.to_string();
    let mut full = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--full" => full = true,
            "--comm" => {
                if let Some(v) = it.next() {
                    comm = v.clone();
                }
            }
            v if !v.starts_with("--") && path.is_none() => path = Some(v),
            _ => {}
        }
    }
    let Some(path) = path else {
        eprintln!("{PARSE_USAGE}");
        return 2;
    };
    // Python 侧是 errors="replace", 非法字节不致命
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("读不到 {path}: {e}");
            return 1;
        }
    };
    let d = parse(&String::from_utf8_lossy(&bytes), &comm);
    println!("{}", dumps(&if full { d.to_pyval() } else { d.summarize() }));
    0
}

pub fn run_attribute(args: &[String]) -> i32 {
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("{ATTR_USAGE}");
        return 2;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读不到 {path}: {e}");
            return 1;
        }
    };
    let s: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{path} 不是合法 JSON: {e}");
            return 1;
        }
    };
    match attribute(&s) {
        Ok(v) => {
            println!("{}", dumps(&v));
            0
        }
        Err(e) => {
            eprintln!("{path}: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        // 本文件在 <repo>/src/, 测试跑在 <repo>/src 下
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
    }

    /// 归因的对照源: `loop_v1/runs/*/summary.json` → `attribution.json`,
    /// 21 组全是真机录制后由旧 Python 版落盘的, 逐字节比。
    #[test]
    fn attribute_matches_recorded_golden() {
        let runs = repo_root().join("loop_v1/runs");
        let mut checked = 0;
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&runs)
            .expect("loop_v1/runs 不在")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        dirs.sort();
        for d in dirs {
            let (summary, golden) = (d.join("summary.json"), d.join("attribution.json"));
            if !summary.exists() || !golden.exists() {
                continue;
            }
            let s: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&summary).unwrap()).unwrap();
            let want = std::fs::read_to_string(&golden).unwrap();
            assert_eq!(
                dumps(&attribute(&s).unwrap()),
                want.trim_end_matches('\n'),
                "归因输出与 {} 不一致",
                golden.display()
            );
            checked += 1;
        }
        assert!(checked >= 20, "对照样本只有 {checked} 组, 太少");
    }

    /// 解析的对照源: 切片过的真实 trace → 旧 Python 版的 summary / --full / 归因输出。
    /// 两份切片分别取自原神 (每帧 2 次提交) 与 MegaCity (每帧 1 次), 覆盖自检的两个分支;
    /// 切片保留原始事件交错与 meta 头, 且含其它线程的提交 (渲染线程过滤那一刀也在测)。
    #[test]
    fn parse_matches_recorded_golden() {
        let dir = repo_root().join("loop_v1/fixtures");
        for stem in ["trace_slice", "trace_slice_mc"] {
            let text = std::fs::read_to_string(dir.join(format!("{stem}.txt")))
                .unwrap_or_else(|e| panic!("{stem}.txt: {e}"));
            let d = parse(&text, DEFAULT_RENDER_COMM);
            let summary = dumps(&d.summarize());
            let parsed: serde_json::Value = serde_json::from_str(&summary).unwrap();
            for (name, got) in [
                (format!("{stem}.summary.json"), summary.clone()),
                (format!("{stem}.full.json"), dumps(&d.to_pyval())),
                (format!("{stem}.attr.json"), dumps(&attribute(&parsed).unwrap())),
            ] {
                let want = std::fs::read_to_string(dir.join(&name)).unwrap();
                assert_eq!(got, want.trim_end_matches('\n'), "{name} 不一致");
            }
        }
    }

    #[test]
    fn spf_detection_prefers_smallest_positive_lag() {
        // 短-长交替: lag1 强负、lag2 强正 → 真周期是 2, 不是 4 或 6
        let gaps: Vec<f64> = (0..60).map(|i| if i % 2 == 0 { 5.0 } else { 28.0 }).collect();
        let (spf, sc) = detect_submits_per_frame(&gaps, 6);
        assert_eq!(spf, 2);
        assert!(sc.acf[0].1 < -0.9 && sc.acf[1].1 > 0.9);
    }

    #[test]
    fn spf_falls_back_to_one_without_periodicity() {
        let gaps: Vec<f64> = (0..40).map(|i| 10.0 + (i % 7) as f64 * 1e-9).collect();
        assert_eq!(detect_submits_per_frame(&gaps, 6).0, 1);
    }

    #[test]
    fn pct_interpolates_like_numpy_default() {
        let xs = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(pct(&xs, 0.5), Some(2.5));
        assert_eq!(pct(&xs, 0.0), Some(1.0));
        assert_eq!(pct(&xs, 1.0), Some(4.0));
        assert_eq!(pct(&[], 0.5), None);
        assert_eq!(pct(&[7.0], 0.9), Some(7.0));
    }

    /// Python 的 `or 0.0` 把 -0.0 也当假值。不照搬的话 attribution.json 里会冒出 "-0.0"。
    #[test]
    fn negative_zero_normalizes_like_python() {
        let s: serde_json::Value =
            serde_json::from_str(r#"{"frame_p50":-0.0,"n_thermal_events":0}"#).unwrap();
        let out = dumps(&attribute(&s).unwrap());
        assert!(out.contains("\"frame_p50_ms\": 0.0"), "{out}");
        assert!(!out.contains("-0.0"), "{out}");
    }

    /// meta 里的 ddr 频率坏了就整轮失败, 不静默填 0 造一个假测量值。
    #[test]
    fn unparseable_ddr_meta_is_an_error_not_a_zero() {
        let s: serde_json::Value =
            serde_json::from_str(r#"{"frame_p50":33.5,"ddr_cur_khz":"N/A"}"#).unwrap();
        let e = attribute(&s).unwrap_err();
        assert!(e.contains("ddr_cur_khz"), "{e}");
    }

    #[test]
    fn fps_cap_detection_uses_2pct_band() {
        assert_eq!(detect_fps_cap(33.536), Some(30));
        assert_eq!(detect_fps_cap(16.6), Some(60));
        assert_eq!(detect_fps_cap(25.0), None);
    }

    /// 没有 retire 配对、没有周期性的退化输入不应 panic, 且字段该空就空。
    #[test]
    fn degenerate_trace_stays_sane() {
        let d = parse("#loop_v1_meta a=1\ngarbage\n", DEFAULT_RENDER_COMM);
        assert_eq!(d.n_submits, 0);
        assert_eq!(d.submits_per_frame, 1);
        assert_eq!(d.fps_mean, None);
        assert!(dumps(&d.summarize()).contains("\"bw_median\": null"));
    }

    #[test]
    fn meta_dict_keeps_insertion_order_on_overwrite() {
        let mut m = Meta::default();
        m.set("a", "1");
        m.set("b", "2");
        m.set("a", "3");
        assert_eq!(dumps(&m.to_pyval()), "{\n \"a\": \"3\",\n \"b\": \"2\"\n}");
    }

    #[test]
    fn share_pct_rounds_like_the_recorded_report() {
        // base1 的 gpu_share_pct: 21.676/33.536*100 = 64.6382... -> 64.64
        assert_eq!(py_round(21.676 / 33.536 * 100.0, 2), 64.64);
    }
}
