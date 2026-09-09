//! bench: 端侧 TFLite 模型的物理延迟标尺 (SPEC_SR_LOOP Gate 0)。
//!
//! 职责边界: 锁频 → 等冷 → 推 benchmark_model (GPU Delegate) → 解析算子日志 → 出判定。
//! 不侵入任何游戏进程, 不写持久化配置; 所有 sysfs 写入(CPU 调速器 / kgsl 功率档位 / 强制供电)
//! 都在退出前恢复原值, 无论中途成功失败。判定全部由程序从真机日志产出, 不含人工评分。
//!
//! 两个延迟口径 (2026-09-09 NX809J 实测定论, 见 SPEC_SR_LOOP §3):
//!   - gpu   : GPU Delegate 内核总时延 (OpenCL 事件计时, 只含模型算子) —— 结构进化的门禁口径
//!   - invoke: benchmark_model 端到端 Invoke 时延 (含 CPU<->GPU 张量拷贝与同步) —— 参考口径
//!   540x960->1080x1920 时拷贝量 31MB/帧, invoke 比 gpu 高 7ms 且与结构无关, 故门禁不用它。
//!
//! 每轮协议 (2026-09-09 实测定论): 未锁频等冷 → 锁频 → 测 → 立刻解锁。锁频只持续跑测那几秒。
//! 反例: 先锁再等冷, performance 调速器让 8 核待机在最高电压, 结温从 33C 爬到 48C 永远过不了
//! 40C 门禁, 进程被杀后设备还停在锁频态。故锁前把快照存到本机状态文件, 下次启动/--unlock 先回滚。
use serde_json::{json, Value};
use std::io::Write;
use std::time::{Duration, Instant};

const KGSL: &str = "/sys/class/kgsl/kgsl-3d0";
const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
/// 内存总线 DCVS (Qualcomm bus_dcvs): DDR 与 LLCC 的用户态下限 boost_freq 在跑测期间钉到 hw_max_freq
/// (hw_min_freq 是内核私有只读节点, root 也写不进)。
/// 反例 (2026-09-09): DDR 待机 547MHz 随负载爬升, resize 这类访存算子轮间离散 5.3%; 钉住后消除。
const BUS_DCVS: &str = "/sys/devices/system/cpu/bus_dcvs";
const BUS_NODES: [&str; 2] = ["DDR", "LLCC"];
/// 单轮内 GPU 内核时延变异系数 std/avg 超过此值 = 该轮受到干扰 (总线抢占 / 后台突发), 不计入判定窗口
const CLEAN_CV_LIMIT_PCT: f64 = 5.0;
const REMOTE_DIR: &str = "/data/local/tmp/phonefarm_bench";
const DEFAULT_BIN: &str = "tools/tflite/android_aarch64_benchmark_model";
const BIN_URL: &str = "https://storage.googleapis.com/tensorflow-nightly-public/prod/tensorflow/release/lite/tools/nightly/latest/android_aarch64_benchmark_model";
/// 热区前缀: SoC 结温 (CPU 各核 / CPU LLC / GPU 子系统); 皮肤温等外壳传感器不参与冷机判定
const SOC_ZONE_PREFIXES: [&str; 3] = ["cpu-", "cpullc", "gpuss"];
/// 轮间离散度门槛 (Max-Min)/Median, 百分比 (SPEC_SR_LOOP Gate 0 验收判据)
const DISPERSION_LIMIT_PCT: f64 = 5.0;
/// 锁频前的设备快照落到本机状态文件 (按 serial 区分); 进程被杀留下的锁态由下次启动或 --unlock 回滚。
/// (kgsl 的 force_clk_on/force_rail_on 等强制供电位在此内核上写入即失败, 不纳入协议。)
fn lock_state_path(serial: &Option<String>) -> std::path::PathBuf {
    let tag: String = serial.as_deref().unwrap_or("default").chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    std::env::temp_dir().join(format!("phonefarm-bench-lock-{tag}.txt"))
}

/// 进度一律走 stderr: --json 时 stdout 只有 JSON, 而卡在哪一步必须能被看见 (等冷死循环的教训)
fn progress(msg: &str) {
    eprintln!("[bench] {msg}");
}

fn emit(text: &str) {
    let mut out = std::io::stdout().lock();
    if out.write_all(text.as_bytes()).is_err() || out.flush().is_err() {
        std::process::exit(0);
    }
}

// ══════════════ 参数 ══════════════

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Metric { Gpu, Invoke }

pub struct BenchArgs {
    serial: Option<String>,
    model: String,
    runs: u32,
    max_rounds: u32,
    json: bool,
    num_runs: u32,
    warmup: u32,
    limit_ms: f64,
    metric: Metric,
    gpu_level: Option<u32>,
    gpu_mhz: Option<u64>,
    lock: bool,
    cool_c: f64,
    cool_timeout_s: u64,
    fp16: bool,
    threads: u32,
    bin: Option<String>,
    out: Option<String>,
    keep: bool,
    unlock: bool,
}

const USAGE: &str = "用法: phonefarm bench --serial <设备> --model <PATH.tflite> [--runs 3] [--max-rounds 6] [--json]\n\
      [--num-runs 100] [--warmup 20] [--limit-ms 4.0] [--metric gpu|invoke] [--threads 1]\n\
      [--gpu-level N | --gpu-mhz M] [--no-lock] [--cool-c 40] [--cool-timeout-s 600]\n\
      [--fp32] [--bin <benchmark_model>] [--out <目录>] [--keep]\n\
      phonefarm bench --serial <设备> --unlock      (回滚上次异常退出遗留的锁频态)";

fn parse_args(args: &[String]) -> Result<BenchArgs, String> {
    let mut a = BenchArgs {
        serial: None, model: String::new(), runs: 3, max_rounds: 0, json: false, num_runs: 100, warmup: 20,
        limit_ms: 4.0, metric: Metric::Gpu, gpu_level: None, gpu_mhz: None, lock: true,
        cool_c: 40.0, cool_timeout_s: 600, fp16: true, threads: 1, bin: None, out: None, keep: false,
        unlock: false,
    };
    let mut i = 0;
    let need = |args: &[String], i: usize, name: &str| -> Result<String, String> {
        args.get(i + 1).cloned().ok_or_else(|| format!("{name} 需要一个值\n{USAGE}"))
    };
    while i < args.len() {
        match args[i].as_str() {
            "--serial" => { a.serial = Some(need(args, i, "--serial")?); i += 1; }
            "--model" => { a.model = need(args, i, "--model")?; i += 1; }
            "--runs" => { a.runs = need(args, i, "--runs")?.parse().map_err(|_| "--runs 需为正整数")?; i += 1; }
            "--max-rounds" => { a.max_rounds = need(args, i, "--max-rounds")?.parse().map_err(|_| "--max-rounds 需为正整数")?; i += 1; }
            "--num-runs" => { a.num_runs = need(args, i, "--num-runs")?.parse().map_err(|_| "--num-runs 需为正整数")?; i += 1; }
            "--warmup" => { a.warmup = need(args, i, "--warmup")?.parse().map_err(|_| "--warmup 需为整数")?; i += 1; }
            "--limit-ms" => { a.limit_ms = need(args, i, "--limit-ms")?.parse().map_err(|_| "--limit-ms 需为数字")?; i += 1; }
            "--threads" => { a.threads = need(args, i, "--threads")?.parse().map_err(|_| "--threads 需为正整数")?; i += 1; }
            "--metric" => {
                a.metric = match need(args, i, "--metric")?.as_str() {
                    "gpu" => Metric::Gpu,
                    "invoke" => Metric::Invoke,
                    other => return Err(format!("--metric 只认 gpu|invoke, 收到 '{other}'")),
                };
                i += 1;
            }
            "--gpu-level" => { a.gpu_level = Some(need(args, i, "--gpu-level")?.parse().map_err(|_| "--gpu-level 需为整数")?); i += 1; }
            "--gpu-mhz" => { a.gpu_mhz = Some(need(args, i, "--gpu-mhz")?.parse().map_err(|_| "--gpu-mhz 需为整数")?); i += 1; }
            "--no-lock" => a.lock = false,
            "--cool-c" => { a.cool_c = need(args, i, "--cool-c")?.parse().map_err(|_| "--cool-c 需为数字")?; i += 1; }
            "--cool-timeout-s" => { a.cool_timeout_s = need(args, i, "--cool-timeout-s")?.parse().map_err(|_| "--cool-timeout-s 需为整数")?; i += 1; }
            "--fp32" => a.fp16 = false,
            "--bin" => { a.bin = Some(need(args, i, "--bin")?); i += 1; }
            "--out" => { a.out = Some(need(args, i, "--out")?); i += 1; }
            "--keep" => a.keep = true,
            "--unlock" => a.unlock = true,
            "--json" => a.json = true,
            other => return Err(format!("无法识别的参数 '{other}'\n{USAGE}")),
        }
        i += 1;
    }
    if a.model.is_empty() && !a.unlock { return Err(format!("缺 --model\n{USAGE}")); }
    if a.runs == 0 || a.num_runs == 0 { return Err("--runs / --num-runs 必须 >= 1".into()); }
    // 缺省多给 3 轮余量: 受干扰的轮不计入连续窗口, 但总轮数有界
    if a.max_rounds == 0 { a.max_rounds = a.runs + 3; }
    if a.max_rounds < a.runs { return Err("--max-rounds 不能小于 --runs".into()); }
    if a.gpu_level.is_some() && a.gpu_mhz.is_some() { return Err("--gpu-level 与 --gpu-mhz 二选一".into()); }
    Ok(a)
}

// ══════════════ 日志解析 (纯函数, 单测覆盖) ══════════════

/// "count=108 first=8622 curr=8726 min=8268 max=9464 avg=8708.66 std=193 p5=8407 median=8705 p95=8982"
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Dist {
    pub count: u64,
    pub first: f64,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub std: f64,
    pub median: Option<f64>,
    pub p5: Option<f64>,
    pub p95: Option<f64>,
}

fn kv_num(line: &str, key: &str) -> Option<f64> {
    line.split_whitespace()
        .find_map(|tok| tok.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .and_then(|v| v.parse().ok())
}

fn parse_dist(line: &str) -> Option<Dist> {
    Some(Dist {
        count: kv_num(line, "count")? as u64,
        first: kv_num(line, "first").unwrap_or(0.0),
        min: kv_num(line, "min")?,
        max: kv_num(line, "max")?,
        avg: kv_num(line, "avg")?,
        std: kv_num(line, "std").unwrap_or(0.0),
        median: kv_num(line, "median"),
        p5: kv_num(line, "p5"),
        p95: kv_num(line, "p95"),
    })
}

impl Dist {
    fn to_json(&self) -> Value {
        json!({"count": self.count, "first": self.first, "min": self.min, "max": self.max,
               "avg": self.avg, "std": self.std, "median": self.median, "p5": self.p5, "p95": self.p95})
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpRow {
    pub node_type: String,
    pub avg_ms: f64,
    pub pct: f64,
    pub name: String,
    /// 名字带 "Delegate/" 前缀 = GPU Delegate 内核; 其余是 CPU 上跑的算子(即回退)
    pub on_gpu: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Delegate { pub replaced: u32, pub total: u32, pub partitions: u32 }

#[derive(Debug, Default, Clone)]
pub struct RoundStats {
    pub init_us: Option<f64>,
    pub first_us: Option<f64>,
    pub warmup_avg_us: Option<f64>,
    pub invoke_avg_us: Option<f64>,
    pub invoke: Option<Dist>,
    pub profile_total: Option<Dist>,
    pub ops: Vec<OpRow>,
    pub delegate: Option<Delegate>,
    pub backend: Option<String>,
    pub error: Option<String>,
    /// 日志里出现过 GPU delegate 创建: 之后的任何失败都归因于模型与 OpenCL 路径不兼容 (回退否决), 而非环境错误
    pub delegate_attempted: bool,
}

impl RoundStats {
    /// GPU 内核总时延 (us): Delegate 各内核 avg 之和; 没有算子档案时退回 profile 总计
    pub fn gpu_kernel_us(&self) -> Option<f64> {
        if self.ops.iter().any(|o| o.on_gpu) {
            return Some(self.ops.iter().filter(|o| o.on_gpu).map(|o| o.avg_ms).sum::<f64>() * 1000.0);
        }
        self.profile_total.as_ref().map(|d| d.avg)
    }
    /// 全图落在 GPU (OpenCL): 委托覆盖 N/N 且单分区, 算子档案里没有任何 CPU 节点, 后端是 OpenCL, 无 ERROR 行。
    /// OpenCL 不支持的算子会让 delegate 退到 OpenGL 后端 (无 per-op profiler, 且不是标尺校准的路径),
    /// 所以命令行强制 --gpu_backend=cl, 这时不支持的算子表现为 delegate 申请失败 + CPU 行, 直接一票否决。
    pub fn full_gpu(&self) -> bool {
        let cov = matches!(self.delegate, Some(d) if d.total > 0 && d.replaced == d.total && d.partitions == 1);
        let no_cpu_op = !self.ops.iter().any(|o| !o.on_gpu);
        cov && no_cpu_op && self.error.is_none() && self.backend.as_deref() == Some("opencl")
    }
    /// 回退原因 (给上层变异器的反馈): CPU 算子清单 / 后端 / delegate 错误
    pub fn fallback_reason(&self) -> Option<String> {
        if self.full_gpu() { return None; }
        let mut parts = Vec::new();
        let cpu_ops: Vec<&str> = self.ops.iter().filter(|o| !o.on_gpu).map(|o| o.node_type.as_str()).collect();
        if !cpu_ops.is_empty() { parts.push(format!("cpu_ops={}", cpu_ops.join(","))); }
        match self.delegate {
            Some(d) if d.replaced < d.total || d.partitions != 1 => parts.push(format!("coverage={}/{} partitions={}", d.replaced, d.total, d.partitions)),
            None => parts.push("delegate_not_applied".into()),
            _ => {}
        }
        match self.backend.as_deref() {
            Some("opencl") => {}
            Some(b) => parts.push(format!("backend={b}")),
            None => parts.push("backend=none".into()),
        }
        if let Some(e) = &self.error { parts.push(e.clone()); }
        Some(parts.join("; "))
    }
}

fn int_after(text: &str, marker: &str) -> Option<u32> {
    let i = text.find(marker)? + marker.len();
    let digits: String = text[i..].trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn num_after(text: &str, marker: &str) -> Option<f64> {
    let i = text.find(marker)? + marker.len();
    let s: String = text[i..].trim_start().chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    s.parse().ok()
}

/// 解析一次 benchmark_model 的完整日志 (stderr+stdout 合流)
pub fn parse_round_log(text: &str) -> RoundStats {
    let mut r = RoundStats::default();
    let mut regular_profile = false;
    let mut in_run_order = false;
    let mut count_lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.contains("Replacing ") && t.contains("with delegate") {
            if let (Some(rep), Some(tot), Some(parts)) =
                (int_after(t, "Replacing "), int_after(t, " out of "), int_after(t, "yielding "))
            {
                // 多子图时取覆盖最差的一条 (主子图 0 之外的子图也不许回退)
                let cand = Delegate { replaced: rep, total: tot, partitions: parts };
                r.delegate = Some(match r.delegate {
                    Some(prev) if prev.replaced * cand.total <= cand.replaced * prev.total => prev,
                    _ => cand,
                });
            }
        } else if t.contains("Created TensorFlow Lite delegate for GPU") {
            r.delegate_attempted = true;
        } else if t.contains("Initialized OpenCL-based API") {
            r.backend = Some("opencl".into());
        } else if t.contains("Initialized OpenGL-based API") {
            r.backend = Some("opengl".into());
        } else if t.starts_with("INFO: count=") {
            count_lines.push(t);
        } else if t.contains("Inference timings in us:") {
            r.init_us = num_after(t, "Init:");
            r.first_us = num_after(t, "First inference:");
            r.warmup_avg_us = num_after(t, "Warmup (avg):");
            r.invoke_avg_us = num_after(t, "Inference (avg):");
        } else if t.contains("Operator-wise Profiling Info for Regular Benchmark Runs") {
            regular_profile = true;
            in_run_order = false;
        } else if regular_profile && t.contains("Run Order") {
            in_run_order = true;
        } else if regular_profile && t.contains("Top by Computation Time") {
            in_run_order = false;
        } else if regular_profile && t.starts_with("Timings (microseconds):") {
            r.profile_total = parse_dist(t);
        } else if (t.starts_with("ERROR:") || t.starts_with("Failed")) && r.error.is_none() {
            r.error = Some(t.to_string());
        } else if in_run_order && line.starts_with('\t') && !t.starts_with("[node type]") {
            let cols: Vec<&str> = line.split('\t').map(|c| c.trim()).filter(|c| !c.is_empty()).collect();
            if cols.len() >= 8 {
                if let (Ok(avg), Ok(pct)) = (cols[2].parse::<f64>(), cols[3].trim_end_matches('%').parse::<f64>()) {
                    let name = cols[7].to_string();
                    r.ops.push(OpRow {
                        node_type: cols[0].to_string(), avg_ms: avg, pct,
                        on_gpu: name.starts_with("Delegate/"), name,
                    });
                }
            }
        }
    }
    // "INFO: count=..." 出现两次: 预热轮与正式轮, 正式轮在后
    if let Some(last) = count_lines.last() {
        r.invoke = parse_dist(last);
    }
    r
}

// ══════════════ 设备状态快照 (纯解析) ══════════════

#[derive(Debug, Clone, PartialEq)]
pub struct CpuPolicy { pub path: String, pub governor: String, pub min_khz: u64, pub max_khz: u64, pub cur_khz: u64, pub cpus: String }

#[derive(Debug, Clone, PartialEq)]
/// floor_khz = boost_freq (用户态下限), max_khz = hw_max_freq, cur_khz = cur_freq
pub struct BusNode { pub name: String, pub floor_khz: u64, pub max_khz: u64, pub cur_khz: u64 }

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub cpu: Vec<CpuPolicy>,
    /// bus_dcvs 节点 (DDR / LLCC), 没有该 sysfs 的内核为空
    pub bus: Vec<BusNode>,
    pub gpu_max_level: Option<u32>,
    pub gpu_min_level: Option<u32>,
    pub gpu_thermal_level: Option<u32>,
    pub gpuclk_hz: Option<u64>,
    pub gpu_model: Option<String>,
    /// 按档位索引排列的 GPU 频率表 (Hz), 索引 0 = 最高档
    pub gpu_freqs: Vec<u64>,
}

/// 快照命令 (root): 一次 su 往返把 CPU 各簇 / kgsl 档位 / 频率表 / 强制供电位全采回
fn snapshot_cmd() -> String {
    format!(
        "su -c 'for p in {CPUFREQ}/policy*; do echo CPU $p $(cat $p/scaling_governor) $(cat $p/scaling_min_freq) $(cat $p/scaling_max_freq) $(cat $p/scaling_cur_freq) $(cat $p/related_cpus | tr \" \" ,); done; \
K={KGSL}; echo GPU $(cat $K/max_pwrlevel) $(cat $K/min_pwrlevel) $(cat $K/thermal_pwrlevel) $(cat $K/gpuclk) $(cat $K/gpu_model); \
echo GPUFREQS $(cat $K/gpu_available_frequencies); \
for b in {}; do d={BUS_DCVS}/$b; [ -d $d ] && echo BUS $b $(cat $d/boost_freq) $(cat $d/hw_max_freq) $(cat $d/cur_freq); done'",
        BUS_NODES.join(" ")
    )
}

pub fn parse_snapshot(text: &str) -> Snapshot {
    let mut s = Snapshot::default();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        match cols.first().copied() {
            Some("CPU") if cols.len() >= 6 => s.cpu.push(CpuPolicy {
                path: cols[1].to_string(),
                governor: cols[2].to_string(),
                min_khz: cols[3].parse().unwrap_or(0),
                max_khz: cols[4].parse().unwrap_or(0),
                cur_khz: cols[5].parse().unwrap_or(0),
                cpus: cols.get(6).unwrap_or(&"").to_string(),
            }),
            Some("GPU") if cols.len() >= 5 => {
                s.gpu_max_level = cols[1].parse().ok();
                s.gpu_min_level = cols[2].parse().ok();
                s.gpu_thermal_level = cols[3].parse().ok();
                s.gpuclk_hz = cols[4].parse().ok();
                s.gpu_model = cols.get(5).map(|v| v.to_string());
            }
            Some("GPUFREQS") => s.gpu_freqs = cols[1..].iter().filter_map(|v| v.parse().ok()).collect(),
            Some("BUS") if cols.len() >= 5 => s.bus.push(BusNode {
                name: cols[1].to_string(),
                floor_khz: cols[2].parse().unwrap_or(0),
                max_khz: cols[3].parse().unwrap_or(0),
                cur_khz: cols[4].parse().unwrap_or(0),
            }),
            _ => {}
        }
    }
    s
}

/// 热区行 "type temp_millidegree" → (type, 毫摄氏度)
pub fn parse_thermal(text: &str) -> Vec<(String, i64)> {
    text.lines().filter_map(|l| {
        let mut it = l.split_whitespace();
        let ty = it.next()?;
        let temp: i64 = it.next()?.parse().ok()?;
        Some((ty.to_string(), temp))
    }).collect()
}

/// SoC 结温最高的热区 (只看 cpu-/cpullc/gpuss 前缀; 0 或 >=100C 的哨兵值跳过)
pub fn soc_max_c(zones: &[(String, i64)]) -> Option<(String, f64)> {
    zones.iter()
        .filter(|(ty, t)| SOC_ZONE_PREFIXES.iter().any(|p| ty.starts_with(p)) && *t > 0 && *t < 100_000)
        .max_by_key(|(_, t)| *t)
        .map(|(ty, t)| (ty.clone(), *t as f64 / 1000.0))
}

/// 目标 MHz → 频率表里最接近的档位索引
pub fn nearest_level(freqs: &[u64], mhz: u64) -> Option<u32> {
    let target = mhz * 1_000_000;
    freqs.iter().enumerate()
        .min_by_key(|(_, f)| (**f as i64 - target as i64).abs())
        .map(|(i, _)| i as u32)
}

/// (Max-Min)/Median, 百分比; 样本不足 2 个记 0
pub fn dispersion_pct(vals: &[f64]) -> (f64, f64, f64, f64) {
    let mut v: Vec<f64> = vals.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() { return (0.0, 0.0, 0.0, 0.0); }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let median = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
    let (min, max) = (v[0], v[n - 1]);
    let disp = if median > 0.0 && n >= 2 { (max - min) / median * 100.0 } else { 0.0 };
    (median, min, max, disp)
}

/// 采样器输出 "gpuclk cpu0 cpu6 ..." 行 → 各列的 (众数, 最小, 最大)
pub fn parse_samples(text: &str) -> Vec<(u64, u64, u64)> {
    let rows: Vec<Vec<u64>> = text.lines()
        .map(|l| l.split_whitespace().filter_map(|v| v.parse().ok()).collect::<Vec<u64>>())
        .filter(|r| !r.is_empty())
        .collect();
    let Some(width) = rows.iter().map(|r| r.len()).min() else { return vec![] };
    (0..width).map(|c| {
        let col: Vec<u64> = rows.iter().map(|r| r[c]).collect();
        let mut counts: std::collections::HashMap<u64, usize> = Default::default();
        for v in &col { *counts.entry(*v).or_default() += 1; }
        let mode = counts.iter().max_by_key(|(v, n)| (**n, **v)).map(|(v, _)| *v).unwrap_or(0);
        (mode, *col.iter().min().unwrap(), *col.iter().max().unwrap())
    }).collect()
}

// ══════════════ 设备操作 ══════════════

fn root_ok(phone: &crate::device::Device) -> bool {
    phone.shell("su -c id", 8000).contains("uid=0(")
}

fn read_snapshot(phone: &crate::device::Device) -> Snapshot {
    parse_snapshot(&phone.shell(&snapshot_cmd(), 15000))
}

fn read_thermal(phone: &crate::device::Device) -> Vec<(String, i64)> {
    parse_thermal(&phone.shell(
        "for z in /sys/class/thermal/thermal_zone*; do echo \"$(cat $z/type) $(cat $z/temp)\"; done 2>/dev/null", 15000))
}

/// 冷机门禁: SoC 结温最高热区 < cool_c 才放行; 超时即报错(安全阀, 不是时间预设)
fn wait_cool(phone: &crate::device::Device, cool_c: f64, timeout_s: u64) -> Result<(String, f64, u64), String> {
    let t0 = Instant::now();
    loop {
        let (zone, c) = soc_max_c(&read_thermal(phone))
            .ok_or("读不到任何 cpu-/gpuss 热区, 无法做冷机判定")?;
        if c < cool_c {
            return Ok((zone, c, t0.elapsed().as_secs()));
        }
        if t0.elapsed() > Duration::from_secs(timeout_s) {
            return Err(format!("等冷超时 {timeout_s}s: {zone}={c:.1}C 仍 >= {cool_c}C"));
        }
        progress(&format!("等冷: {zone}={c:.1}C >= {cool_c}C, 3s 后重测 (已等 {}s)", t0.elapsed().as_secs()));
        std::thread::sleep(Duration::from_secs(3));
    }
}

struct Lock {
    before: Snapshot,
    state_path: std::path::PathBuf,
    applied: bool,
}

/// 快照 → 状态文件文本 (与 snapshot_cmd 输出同形, parse_snapshot 可直接回读)
fn snapshot_text(s: &Snapshot) -> String {
    let mut t = String::new();
    for p in &s.cpu {
        t.push_str(&format!("CPU {} {} {} {} {} {}\n", p.path, p.governor, p.min_khz, p.max_khz, p.cur_khz, p.cpus));
    }
    if let (Some(mx), Some(mn)) = (s.gpu_max_level, s.gpu_min_level) {
        t.push_str(&format!("GPU {} {} {} {} {}\n", mx, mn, s.gpu_thermal_level.unwrap_or(0),
            s.gpuclk_hz.unwrap_or(0), s.gpu_model.clone().unwrap_or_default()));
    }
    if !s.gpu_freqs.is_empty() {
        t.push_str(&format!("GPUFREQS {}\n", s.gpu_freqs.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(" ")));
    }
    for b in &s.bus {
        t.push_str(&format!("BUS {} {} {} {}\n", b.name, b.floor_khz, b.max_khz, b.cur_khz));
    }
    t
}

/// 把快照里的调速器与 kgsl 档位写回设备; 返回回读是否一致
fn restore_snapshot(phone: &crate::device::Device, before: &Snapshot) -> bool {
    let mut cmd = String::from("su -c '");
    for p in &before.cpu {
        cmd.push_str(&format!("echo {} > {}/scaling_governor; ", p.governor, p.path));
    }
    // 恢复顺序: 先 min(数值大) 再 max, 与 kgsl 钳位规则一致
    if let (Some(mx), Some(mn)) = (before.gpu_max_level, before.gpu_min_level) {
        cmd.push_str(&format!("K={KGSL}; echo {mn} > $K/min_pwrlevel; echo {mx} > $K/max_pwrlevel; "));
    }
    for b in &before.bus {
        cmd.push_str(&format!("echo {} > {BUS_DCVS}/{}/boost_freq; ", b.floor_khz, b.name));
    }
    cmd.push_str("echo RESTORED'");
    let out = phone.shell(&cmd, 15000);
    if !out.contains("RESTORED") { return false; }
    let after = read_snapshot(phone);
    let cpu_ok = after.cpu.len() == before.cpu.len()
        && after.cpu.iter().zip(&before.cpu).all(|(a, b)| a.governor == b.governor);
    let bus_ok = after.bus.len() == before.bus.len()
        && after.bus.iter().zip(&before.bus).all(|(a, b)| a.floor_khz == b.floor_khz);
    cpu_ok && bus_ok && after.gpu_max_level == before.gpu_max_level && after.gpu_min_level == before.gpu_min_level
}

/// 上次异常退出遗留的锁态: 状态文件在就按它回滚。返回 Some(是否回滚成功); 无遗留 None
fn recover_stale_lock(phone: &crate::device::Device, state_path: &std::path::Path) -> Option<bool> {
    let text = std::fs::read_to_string(state_path).ok()?;
    let before = parse_snapshot(&text);
    if before.cpu.is_empty() && before.gpu_max_level.is_none() {
        let _ = std::fs::remove_file(state_path);
        return None;
    }
    progress(&format!("发现上次遗留的锁频状态 {}, 先回滚", state_path.display()));
    let ok = restore_snapshot(phone, &before);
    if ok { let _ = std::fs::remove_file(state_path); }
    Some(ok)
}

impl Lock {
    fn apply(phone: &crate::device::Device, state_path: &std::path::Path, before: Snapshot, level: u32) -> Result<Lock, String> {
        // 先落状态文件再动设备: 中途被杀也能回滚
        std::fs::write(state_path, snapshot_text(&before))
            .map_err(|e| format!("写不了锁频状态文件 {}: {e}", state_path.display()))?;
        let mut cmd = String::from("su -c '");
        for p in &before.cpu {
            cmd.push_str(&format!("echo performance > {}/scaling_governor; ", p.path));
        }
        // 先 max 再 min 再 max: kgsl 的 store 互相钳位 (max<=min, min>=max), 这个顺序对任意目标档都成立
        cmd.push_str(&format!("K={KGSL}; echo {level} > $K/max_pwrlevel; echo {level} > $K/min_pwrlevel; echo {level} > $K/max_pwrlevel; "));
        // 总线钉在 hw_max_freq: 访存算子的时延不再随 DDR/LLCC DCVS 漂移
        for b in &before.bus {
            cmd.push_str(&format!("echo {} > {BUS_DCVS}/{}/boost_freq; ", b.max_khz, b.name));
        }
        cmd.push_str("echo LOCKED'");
        let out = phone.shell(&cmd, 15000);
        let lock = Lock { before, state_path: state_path.to_path_buf(), applied: true };
        if !out.contains("LOCKED") {
            lock.restore(phone);
            return Err(format!("锁频命令未执行完: {}; 已回滚", out.trim()));
        }
        let after = read_snapshot(phone);
        let cpu_ok = !after.cpu.is_empty() && after.cpu.iter().all(|p| p.governor == "performance");
        let gpu_ok = after.gpu_max_level == Some(level) && after.gpu_min_level == Some(level);
        let bus_ok = after.bus.len() == lock.before.bus.len() && after.bus.iter().all(|b| b.floor_khz == b.max_khz);
        if !cpu_ok || !gpu_ok || !bus_ok {
            lock.restore(phone);
            return Err(format!("锁频回读不符: cpu_governor={:?} gpu(max,min)=({:?},{:?}) 目标档 {level} bus={:?}; 已回滚",
                after.cpu.iter().map(|p| p.governor.as_str()).collect::<Vec<_>>(),
                after.gpu_max_level, after.gpu_min_level,
                after.bus.iter().map(|b| format!("{}:{}/{}", b.name, b.floor_khz, b.max_khz)).collect::<Vec<_>>()));
        }
        Ok(lock)
    }

    /// 恢复原值并清掉状态文件; 返回回读是否与锁前一致
    fn restore(&self, phone: &crate::device::Device) -> bool {
        if !self.applied { return true; }
        let ok = restore_snapshot(phone, &self.before);
        if ok { let _ = std::fs::remove_file(&self.state_path); }
        ok
    }
}

fn locate_bin(a: &BenchArgs) -> Result<String, String> {
    let cands: Vec<String> = a.bin.iter().cloned()
        .chain(std::env::var("PF_BENCH_BIN").ok())
        .chain(std::iter::once(DEFAULT_BIN.to_string()))
        .collect();
    for c in &cands {
        if std::fs::metadata(c).map(|m| m.is_file()).unwrap_or(false) {
            return Ok(c.clone());
        }
    }
    Err(format!("找不到 benchmark_model 二进制 (试过 {:?}); 下载: curl -L -o {DEFAULT_BIN} {BIN_URL}", cands))
}

/// 差量部署: 远端同大小即跳过 push
fn deploy_bin(phone: &crate::device::Device, local: &str) -> Result<(), String> {
    let size = std::fs::metadata(local).map(|m| m.len()).map_err(|e| format!("{local}: {e}"))?;
    let remote = format!("{REMOTE_DIR}/benchmark_model");
    phone.shell(&format!("mkdir -p {REMOTE_DIR}"), 5000);
    let cur = phone.shell(&format!("stat -c %s {remote} 2>/dev/null"), 5000).trim().parse::<u64>().unwrap_or(0);
    if cur != size {
        if !phone.push_file(local, &remote) {
            return Err(format!("推送 {local} 到 {remote} 失败"));
        }
    }
    phone.shell(&format!("chmod 755 {remote}"), 5000);
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn device_info(phone: &crate::device::Device) -> Value {
    let get = |k: &str| phone.shell(&format!("getprop {k}"), 5000).trim().to_string();
    // 前台窗口: 标尺的前提是没有游戏在前台渲染, 记进报告作证据
    let focus = phone.shell("dumpsys window | grep -m1 mCurrentFocus", 6000);
    let focus = focus.split("u0 ").nth(1).map(|t| t.trim_end_matches('}').trim().to_string()).unwrap_or_default();
    json!({
        "model": get("ro.product.model"),
        "platform": get("ro.board.platform"),
        "soc": get("ro.soc.model"),
        "android": get("ro.build.version.release"),
        "sdk": get("ro.build.version.sdk"),
        "focus": focus,
    })
}

/// 跑一轮 benchmark_model, 同时起一路 root 采样器记录 gpuclk 与各簇当前频率 (锁频是否真生效的地面真值)
fn run_round(phone: &crate::device::Device, a: &BenchArgs, remote_model: &str, snap: &Snapshot) -> (String, Vec<(u64, u64, u64)>) {
    let mut sampler = None;
    if !snap.cpu.is_empty() {
        let reads: Vec<String> = std::iter::once(format!("$(cat {KGSL}/gpuclk)"))
            .chain(snap.cpu.iter().map(|p| format!("$(cat {}/scaling_cur_freq)", p.path)))
            .chain(snap.bus.iter().map(|b| format!("$(cat {BUS_DCVS}/{}/cur_freq)", b.name)))
            .collect();
        // 采样上限 240 x 0.25s = 60s, adb 通道断开后远端循环也会自行到头, 不留常驻进程
        let cmd = format!("su -c 'i=0; while [ $i -lt 240 ]; do echo {}; sleep 0.25; i=$((i+1)); done'", reads.join(" "));
        sampler = phone.stream_shell(&cmd).ok();
        std::thread::sleep(Duration::from_millis(300));
    }
    let cmd = format!(
        "cd {REMOTE_DIR} && ./benchmark_model --graph={remote_model} --use_gpu=true --gpu_precision_loss_allowed={} \
--gpu_backend=cl --num_threads={} --warmup_runs={} --warmup_min_secs=0 --num_runs={} --min_secs=0 --enable_op_profiling=true \
--max_profiling_buffer_entries=4096 2>&1",
        a.fp16, a.threads, a.warmup, a.num_runs, );
    let log = phone.shell(&cmd, 180_000);
    let mut samples = Vec::new();
    if let Some(mut child) = sampler {
        let _ = child.kill();
        if let Ok(out) = child.wait_with_output() {
            samples = parse_samples(&String::from_utf8_lossy(&out.stdout));
        }
    }
    (log, samples)
}

// ══════════════ 主流程 ══════════════

pub fn run_bench(args: &[String]) -> i32 {
    let a = match parse_args(args) {
        Ok(a) => a,
        Err(e) => { eprintln!("{e}"); return 2; }
    };
    match bench(&a) {
        Ok((report, code)) => {
            if a.json {
                emit(&format!("{}\n", serde_json::to_string_pretty(&report).unwrap_or_default()));
            } else {
                emit(&render_text(&report));
            }
            code
        }
        Err(e) => {
            if a.json {
                emit(&format!("{}\n", json!({"v": 1, "ok": false, "verdict": "ERROR", "error": e})));
            } else {
                eprintln!("bench 失败: {e}");
            }
            2
        }
    }
}

fn bench(a: &BenchArgs) -> Result<(Value, i32), String> {
    let t_all = Instant::now();
    let serial = a.serial.clone();
    if serial.as_deref().map(|s| s.starts_with("hdc:")).unwrap_or(false) {
        return Err("bench 只支持 Android/adb 设备 (kgsl + TFLite GPU Delegate)".into());
    }
    let out_dir = match &a.out {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::temp_dir().join(format!("phonefarm-bench-{}", std::process::id())),
    };
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("建不了输出目录 {}: {e}", out_dir.display()))?;
    let phone = crate::device::Device::new(serial.clone(), out_dir.to_string_lossy().to_string());
    if !phone.health_check(8000) {
        return Err("设备无心跳 (adb devices 里不是 device 态, 或未指定 --serial)".into());
    }
    let state_path = lock_state_path(&serial);
    let root = root_ok(&phone);
    let recovered = if root { recover_stale_lock(&phone, &state_path) } else { None };
    if a.unlock {
        let msg = match recovered {
            Some(true) => "已回滚遗留锁频态",
            Some(false) => "遗留锁频态回滚失败 (回读不符)",
            None => if root { "无遗留锁频态" } else { "无 root, 无法回滚" },
        };
        let code = if matches!(recovered, Some(false)) || !root { 1 } else { 0 };
        return Ok((json!({"v": 1, "ok": code == 0, "verdict": "UNLOCK", "serial": serial, "recovered": recovered, "message": msg}), code));
    }
    if a.lock && !root {
        return Err("锁频需要 root (su -c); 无 root 设备请加 --no-lock (标尺不受控, 结果只作参考)".into());
    }
    let model_bytes = std::fs::read(&a.model).map_err(|e| format!("读不到模型 {}: {e}", a.model))?;
    let sha = sha256_hex(&model_bytes);
    let bin = locate_bin(a)?;
    let info = device_info(&phone);
    progress(&format!("设备 {} ({}), root={root}, 模型 {} ({} B)", info["model"].as_str().unwrap_or("?"),
        info["soc"].as_str().unwrap_or("?"), a.model, model_bytes.len()));
    deploy_bin(&phone, &bin)?;
    let remote_model = format!("{REMOTE_DIR}/model_{}.tflite", &sha[..12]);
    if !phone.push_file(&a.model, &remote_model) {
        return Err(format!("推送模型到 {remote_model} 失败"));
    }

    // 档位选择只看一次基线快照; 每轮锁前再取当轮基线 (恢复以当轮为准)
    let base = if root { read_snapshot(&phone) } else { Snapshot::default() };
    let mut target: Option<(u32, u64)> = None;
    if a.lock {
        let level = match (a.gpu_level, a.gpu_mhz) {
            (Some(l), _) => l,
            (None, Some(m)) => nearest_level(&base.gpu_freqs, m).ok_or("读不到 kgsl 频率表, --gpu-mhz 无法换算")?,
            // 缺省锁在厂商当前允许的最高档 (max_pwrlevel), 不越过其正常模式上限
            (None, None) => base.gpu_max_level.ok_or("读不到 kgsl max_pwrlevel")?,
        };
        let hz = *base.gpu_freqs.get(level as usize)
            .ok_or_else(|| format!("GPU 档位 {level} 超出频率表 (共 {} 档)", base.gpu_freqs.len()))?;
        if base.cpu.is_empty() { return Err("读不到 cpufreq policy, 无法锁 CPU".into()); }
        target = Some((level, hz));
        progress(&format!("锁频目标: CPU {} 簇 performance | GPU 档位 {level} = {} MHz ({} 档, 当前 max/min = {:?}/{:?})",
            base.cpu.len(), hz / 1_000_000, base.gpu_freqs.len(), base.gpu_max_level, base.gpu_min_level));
    }

    let mut rounds = Vec::new();
    let mut all_restored = true;
    let mut first_err: Option<String> = None;
    let mut clean_flags: Vec<bool> = Vec::new();
    let mut metric_vals: Vec<Option<f64>> = Vec::new();
    let mut window: Option<(usize, usize)> = None; // 满足判据的连续窗口 [start, end) (0 起)
    let need = a.runs as usize;
    let mut i = 0u32;
    while i < a.max_rounds {
        i += 1;
        // 1. 未锁频等冷: 结温门禁在待机态判定, 锁频只覆盖跑测那几秒
        let (zone, c, waited) = match wait_cool(&phone, a.cool_c, a.cool_timeout_s) {
            Ok(v) => v,
            Err(e) => { first_err = Some(format!("第{i}轮: {e}")); break; }
        };
        // 2. 锁 (CPU performance + kgsl 档位 + DDR/LLCC 钉顶)
        let before = if root { read_snapshot(&phone) } else { Snapshot::default() };
        let lock = match target {
            Some((level, _)) => match Lock::apply(&phone, &state_path, before.clone(), level) {
                Ok(l) => Some(l),
                Err(e) => { first_err = Some(format!("第{i}轮: {e}")); break; }
            },
            None => None,
        };
        let locked = if root { read_snapshot(&phone) } else { Snapshot::default() };
        // 3. 测
        let t0 = Instant::now();
        let (log, samples) = run_round(&phone, a, &remote_model, &before);
        let wall_ms = t0.elapsed().as_millis() as u64;
        // 4. 立刻解锁, 无论日志好坏
        let restored = lock.as_ref().map(|l| l.restore(&phone)).unwrap_or(true);
        all_restored &= restored;
        let log_path = out_dir.join(format!("round{i}.log"));
        let _ = std::fs::write(&log_path, &log);
        let st = parse_round_log(&log);
        let (zone_end, c_end) = soc_max_c(&read_thermal(&phone)).unwrap_or((String::new(), 0.0));
        let gpu_sample = samples.first().copied();
        let ncpu = before.cpu.len();
        let bus_samples: Vec<(u64, u64, u64)> = samples.iter().skip(1 + ncpu).copied().collect();
        let gpu_verified = target.map(|(_, hz)| gpu_sample.map(|(mode, _, _)| mode == hz).unwrap_or(false));
        let bus_verified = if before.bus.is_empty() || target.is_none() { None } else {
            Some(bus_samples.len() == before.bus.len()
                && bus_samples.iter().zip(&before.bus).all(|((mode, _, _), b)| *mode == b.max_khz))
        };
        let lock_verified = gpu_verified.map(|g| g && bus_verified.unwrap_or(true));
        // 干扰判定: 用门禁口径那条分布的变异系数
        let dist = match a.metric { Metric::Gpu => st.profile_total.clone(), Metric::Invoke => st.invoke.clone() };
        let cv_pct = dist.as_ref().and_then(|d| if d.avg > 0.0 { Some(d.std / d.avg * 100.0) } else { None });
        let clean = st.full_gpu() && cv_pct.map(|v| v <= CLEAN_CV_LIMIT_PCT).unwrap_or(false);
        let metric_val = match a.metric { Metric::Gpu => st.gpu_kernel_us(), Metric::Invoke => st.invoke_avg_us };
        progress(&format!("第{i}轮: gpu_kernel={} us, invoke_avg={} us, init={} ms, 覆盖={:?}, gpuclk={} MHz, bus={}, cv={}%, 温度 {:.1}->{:.1}C, 恢复={restored}, 干净={clean}, 用时 {wall_ms} ms",
            st.gpu_kernel_us().map(|v| format!("{v:.0}")).unwrap_or("-".into()),
            st.invoke_avg_us.map(|v| format!("{v:.0}")).unwrap_or("-".into()),
            st.init_us.map(|v| format!("{:.0}", v / 1000.0)).unwrap_or("-".into()),
            st.delegate.map(|d| format!("{}/{} x{}", d.replaced, d.total, d.partitions)),
            gpu_sample.map(|(m, _, _)| (m / 1_000_000).to_string()).unwrap_or("-".into()),
            bus_samples.iter().map(|(m, _, _)| (m / 1000).to_string()).collect::<Vec<_>>().join("/"),
            cv_pct.map(|v| format!("{v:.1}")).unwrap_or("-".into()), c, c_end));
        if log.trim().is_empty() {
            first_err = Some(format!("第{i}轮 benchmark_model 无输出 (超时或二进制不可执行), 日志 {}", log_path.display()));
            break;
        }
        rounds.push(json!({
            "round": i,
            "clean": clean,
            "cv_pct": cv_pct,
            "gpu_kernel_us": st.gpu_kernel_us(),
            "invoke_avg_us": st.invoke_avg_us,
            "invoke": st.invoke.as_ref().map(|d| d.to_json()),
            "profile_total": st.profile_total.as_ref().map(|d| d.to_json()),
            "init_us": st.init_us, "first_us": st.first_us, "warmup_avg_us": st.warmup_avg_us,
            "delegate": st.delegate.map(|d| json!({"replaced": d.replaced, "total": d.total, "partitions": d.partitions})),
            "backend": st.backend,
            "full_gpu": st.full_gpu(),
            "delegate_attempted": st.delegate_attempted,
            "fallback_reason": st.fallback_reason(),
            "ops": st.ops.iter().map(|o| json!({"type": o.node_type, "avg_ms": o.avg_ms, "pct": o.pct, "name": o.name, "gpu": o.on_gpu})).collect::<Vec<_>>(),
            "error": st.error,
            "thermal": {"start_zone": zone, "start_c": c, "waited_s": waited, "end_zone": zone_end, "end_c": c_end},
            "lock": {
                "applied": lock.is_some(),
                "verified": lock_verified,
                "gpu_verified": gpu_verified,
                "bus_verified": bus_verified,
                "restored": restored,
                "cpu": locked.cpu.iter().map(|p| json!({"policy": p.path.rsplit('/').next().unwrap_or(""), "cpus": p.cpus,
                    "governor": p.governor, "cur_khz": p.cur_khz, "max_khz": p.max_khz})).collect::<Vec<_>>(),
                "bus": locked.bus.iter().map(|b| json!({"name": b.name, "floor_khz": b.floor_khz, "max_khz": b.max_khz, "cur_khz": b.cur_khz})).collect::<Vec<_>>(),
                "gpu_thermal_level": locked.gpu_thermal_level,
                "gpu_throttled": matches!((locked.gpu_thermal_level, target), (Some(t), Some((l, _))) if t > l),
            },
            "gpuclk_hz": gpu_sample.map(|(mode, min, max)| json!({"mode": mode, "min": min, "max": max})),
            "cpu_khz": samples.iter().skip(1).take(ncpu).map(|(mode, min, max)| json!({"mode": mode, "min": min, "max": max})).collect::<Vec<_>>(),
            "bus_khz": before.bus.iter().zip(&bus_samples).map(|(b, (mode, min, max))| json!({"name": b.name, "mode": mode, "min": min, "max": max})).collect::<Vec<_>>(),
            "wall_ms": wall_ms,
            "log": log_path.to_string_lossy(),
        }));
        clean_flags.push(clean);
        metric_vals.push(metric_val);
        // 委托失败 / 出错的模型没有继续测的意义 (零样本秒筛要快), 首轮即定案
        if !st.full_gpu() || st.error.is_some() { break; }
        // 连续 N 轮全干净且离散度达标 → 判定窗口成立, 停
        let n = rounds.len();
        if n >= need {
            let tail_clean = clean_flags[n - need..].iter().all(|c| *c);
            let vals: Vec<f64> = metric_vals[n - need..].iter().filter_map(|v| *v).collect();
            if tail_clean && vals.len() == need && dispersion_pct(&vals).3 <= DISPERSION_LIMIT_PCT {
                window = Some((n - need, n));
                break;
            }
        }
    }
    if !a.keep {
        phone.shell(&format!("rm -f {remote_model}"), 5000);
    }
    if let Some(e) = first_err {
        return Err(format!("{e} (已完成 {} 轮, 锁频全部恢复={all_restored})", rounds.len()));
    }

    // 汇总判定: 有窗口用窗口; 没有窗口就按最后 N 轮 (或全部) 如实汇报
    let metric_name = match a.metric { Metric::Gpu => "gpu", Metric::Invoke => "invoke" };
    let n = rounds.len();
    let (ws, we) = window.unwrap_or((n.saturating_sub(need), n));
    let vals: Vec<f64> = metric_vals[ws..we].iter().filter_map(|v| *v).collect();
    let errors: Vec<String> = rounds.iter().filter_map(|r| r["error"].as_str().map(String::from)).collect();
    let full_gpu = !rounds.is_empty() && rounds.iter().all(|r| r["full_gpu"].as_bool() == Some(true));
    let (median_us, min_us, max_us, disp) = dispersion_pct(&vals);
    let have_all = vals.len() == need && n >= need;
    let dispersion_ok = window.is_some();
    let within = have_all && median_us / 1000.0 <= a.limit_ms;
    let gpu_lock_ok = target.map(|_| rounds[ws..we].iter().all(|r| r["lock"]["verified"].as_bool() == Some(true)));
    let interfered = clean_flags.iter().filter(|c| !**c).count();
    let ran = rounds.iter().all(|r| r["invoke_avg_us"].as_f64().is_some());
    let attempted = rounds.iter().all(|r| r["delegate_attempted"].as_bool() == Some(true));
    let fallback_reason: Option<String> = rounds.iter().find_map(|r| r["fallback_reason"].as_str().map(String::from));
    // 回退否决优先于 ERROR: delegate 已创建但整图没落在 OpenCL 上 (含申请失败、benchmark 中止) 都是模型的问题, 不是环境的
    let verdict = if rounds.is_empty() || !attempted { "ERROR" }
        else if !full_gpu { "FAIL_FALLBACK" }
        else if !ran { "ERROR" }
        else if !dispersion_ok { "FAIL_UNSTABLE" }
        else if !within { "FAIL_LATENCY" }
        else { "PASS" };
    let code = match verdict { "PASS" => 0, "ERROR" => 2, _ => 1 };
    let ops_ref = rounds.last().and_then(|r| r["ops"].as_array().cloned()).unwrap_or_default();
    let report = json!({
        "v": 1,
        "ok": verdict == "PASS",
        "verdict": verdict,
        "serial": serial,
        "device": info,
        "model": {"path": a.model, "bytes": model_bytes.len(), "sha256": sha},
        "bench_bin": bin,
        "params": {"runs": a.runs, "max_rounds": a.max_rounds, "num_runs": a.num_runs, "warmup": a.warmup, "threads": a.threads,
                   "fp16": a.fp16, "metric": metric_name, "limit_ms": a.limit_ms, "cool_c": a.cool_c,
                   "clean_cv_limit_pct": CLEAN_CV_LIMIT_PCT},
        "lock": {
            "enabled": target.is_some(),
            "gpu": target.map(|(level, hz)| json!({"model": base.gpu_model, "level": level, "hz": hz,
                "num_levels": base.gpu_freqs.len(), "level_before": [base.gpu_max_level, base.gpu_min_level]})),
            "cpu_before": base.cpu.iter().map(|p| json!({"policy": p.path.rsplit('/').next().unwrap_or(""), "cpus": p.cpus, "governor": p.governor})).collect::<Vec<_>>(),
            "bus": base.bus.iter().map(|b| json!({"name": b.name, "pin_khz": b.max_khz, "floor_before_khz": b.floor_khz})).collect::<Vec<_>>(),
            "recovered_stale": recovered,
            "restored_all": all_restored,
        },
        "delegate": rounds.last().map(|r| r["delegate"].clone()),
        "backend": rounds.last().map(|r| r["backend"].clone()),
        "ops": ops_ref,
        "rounds": rounds,
        "summary": {
            "metric": metric_name,
            "rounds_total": n,
            "rounds_interfered": interfered,
            "window": window.map(|(s, e)| json!([s + 1, e])),
            "latency_ms": median_us / 1000.0,
            "min_ms": min_us / 1000.0,
            "max_ms": max_us / 1000.0,
            "dispersion_pct": disp,
            "dispersion_limit_pct": DISPERSION_LIMIT_PCT,
            "dispersion_ok": dispersion_ok,
            "limit_ms": a.limit_ms,
            "within_limit": within,
            "full_gpu": full_gpu,
            "fallback_reason": fallback_reason,
            "gpu_lock_verified": gpu_lock_ok,
            "feasible": full_gpu && within,
            "errors": errors,
        },
        "wall_s": t_all.elapsed().as_secs_f64(),
    });
    Ok((report, code))
}

fn render_text(r: &Value) -> String {
    let mut s = String::new();
    let d = &r["device"];
    s.push_str(&format!("bench {} ({} / {}) 模型 {} ({} B, sha {})\n",
        d["model"].as_str().unwrap_or("?"), d["soc"].as_str().unwrap_or("?"),
        r["lock"]["gpu"]["model"].as_str().unwrap_or("gpu?"),
        r["model"]["path"].as_str().unwrap_or("?"), r["model"]["bytes"],
        &r["model"]["sha256"].as_str().unwrap_or("").chars().take(12).collect::<String>()));
    if r["verdict"].as_str() == Some("UNLOCK") {
        return format!("{}\n", r["message"].as_str().unwrap_or(""));
    }
    if r["lock"]["enabled"].as_bool() == Some(true) {
        s.push_str(&format!("锁频: CPU performance x{} | GPU 档位 {} = {} MHz | 逐轮锁/解锁, 全部恢复={} | 锁频核验={}\n",
            r["lock"]["cpu_before"].as_array().map(|v| v.len()).unwrap_or(0),
            r["lock"]["gpu"]["level"], r["lock"]["gpu"]["hz"].as_u64().unwrap_or(0) / 1_000_000,
            r["lock"]["restored_all"], r["summary"]["gpu_lock_verified"]));
    } else {
        s.push_str("锁频: 未启用 (--no-lock)\n");
    }
    if let Some(dl) = r["delegate"].as_object() {
        s.push_str(&format!("GPU 覆盖: {}/{} 节点, {} 分区, {} -> {}\n", dl["replaced"], dl["total"], dl["partitions"],
            r["backend"].as_str().unwrap_or("?"),
            if r["summary"]["full_gpu"].as_bool() == Some(true) { "100% GPU (OpenCL)" } else { "回退, 一票否决" }));
    } else {
        s.push_str("GPU 覆盖: 未见委托日志 (delegate 未生效, 整图 CPU)\n");
    }
    if let Some(why) = r["summary"]["fallback_reason"].as_str() {
        s.push_str(&format!("回退原因: {why}\n"));
    }
    s.push_str("轮  GPU核时延us  cv%   invoke avg/median/p95 us    init ms  gpuclk MHz  DDR/LLCC MHz  温度C(等冷s)\n");
    for rd in r["rounds"].as_array().cloned().unwrap_or_default() {
        s.push_str(&format!("{:<3} {:>11.0}  {:>4.1}{} {:>7.0}/{:>7.0}/{:>7.0}       {:>7.1}  {:>10}  {:>12}  {:.1}->{:.1}\n",
            rd["round"], rd["gpu_kernel_us"].as_f64().unwrap_or(0.0),
            rd["cv_pct"].as_f64().unwrap_or(0.0), if rd["clean"].as_bool() == Some(true) { " " } else { "!" },
            rd["invoke_avg_us"].as_f64().unwrap_or(0.0),
            rd["invoke"]["median"].as_f64().unwrap_or(0.0), rd["invoke"]["p95"].as_f64().unwrap_or(0.0),
            rd["init_us"].as_f64().unwrap_or(0.0) / 1000.0,
            rd["gpuclk_hz"]["mode"].as_u64().map(|v| (v / 1_000_000).to_string()).unwrap_or("-".into()),
            rd["bus_khz"].as_array().map(|v| v.iter().map(|b| (b["mode"].as_u64().unwrap_or(0) / 1000).to_string()).collect::<Vec<_>>().join("/")).unwrap_or("-".into()),
            rd["thermal"]["start_c"].as_f64().unwrap_or(0.0), rd["thermal"]["end_c"].as_f64().unwrap_or(0.0)));
        s.truncate(s.trim_end_matches('\n').len());
        s.push_str(&format!("({})\n", rd["thermal"]["waited_s"]));
        if let Some(e) = rd["error"].as_str() { s.push_str(&format!("    错误: {e}\n")); }
    }
    let ops: Vec<String> = r["ops"].as_array().cloned().unwrap_or_default().iter()
        .map(|o| format!("{}{} {:.1}% ({:.3}ms)", if o["gpu"].as_bool() == Some(true) { "" } else { "[CPU]" },
            o["type"].as_str().unwrap_or("?"), o["pct"].as_f64().unwrap_or(0.0), o["avg_ms"].as_f64().unwrap_or(0.0)))
        .collect();
    if !ops.is_empty() { s.push_str(&format!("算子分布: {}\n", ops.join(" | "))); }
    let sm = &r["summary"];
    s.push_str(&format!("结论: {}  窗口={} (共 {} 轮, 受干扰 {} 轮)  {}口径 median {:.3}ms (min {:.3} / max {:.3}) 上限 {}ms | 离散度 {:.2}% (上限 {}%) | 全图GPU={}\n",
        r["verdict"].as_str().unwrap_or("?"), sm["window"], sm["rounds_total"], sm["rounds_interfered"], sm["metric"].as_str().unwrap_or("?"),
        sm["latency_ms"].as_f64().unwrap_or(0.0), sm["min_ms"].as_f64().unwrap_or(0.0), sm["max_ms"].as_f64().unwrap_or(0.0),
        sm["limit_ms"], sm["dispersion_pct"].as_f64().unwrap_or(0.0), sm["dispersion_limit_pct"],
        sm["full_gpu"]));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "INFO: STARTING!\n\
INFO: Loaded model /data/local/tmp/phonefarm_bench/gen0-espcn-ps.tflite\n\
INFO: Created TensorFlow Lite delegate for GPU.\n\
VERBOSE: Replacing 4 out of 4 node(s) with delegate (TfLiteGpuDelegateV2) node, yielding 1 partitions for subgraph 0.\n\
INFO: Initialized OpenCL-based API.\n\
INFO: Initialized session in 355.447ms.\n\
INFO: count=50 first=45401 curr=8772 min=8499 max=45401 avg=9580.26 std=5119 p5=8534 median=8894 p95=9062\n\
\n\
INFO: count=108 first=8622 curr=8726 min=8268 max=9464 avg=8708.66 std=193 p5=8407 median=8705 p95=8982\n\
\n\
INFO: Inference timings in us: Init: 355447, First inference: 45401, Warmup (avg): 9580.26, Inference (avg): 8708.66\n\
INFO: Profiling Info for Benchmark Initialization:\n\
============================== Run Order ==============================\n\
\t                             [node type]\t  [first]\t [avg ms]\t     [%]\t  [cdf%]\t  [mem KB]\t[times called]\t[Name]\n\
\t                 ModifyGraphWithDelegate\t  354.149\t  354.149\t 99.992%\t 99.992%\t 46900.000\t        1\tModifyGraphWithDelegate/0\n\
Timings (microseconds): count=1 curr=354176\n\
INFO: Operator-wise Profiling Info for Regular Benchmark Runs:\n\
============================== Run Order ==============================\n\
\t                             [node type]\t  [first]\t [avg ms]\t     [%]\t  [cdf%]\t  [mem KB]\t[times called]\t[Name]\n\
\t              convolution_2d 0 -> relu 1\t    0.265\t    0.272\t 15.773%\t 15.773%\t     0.000\t        1\tDelegate/convolution_2d 0 -> relu 1:0\n\
\t              convolution_2d 2 -> relu 3\t    0.367\t    0.365\t 21.173%\t 36.946%\t     0.000\t        1\tDelegate/convolution_2d 2 -> relu 3:1\n\
\t                        convolution_2d 4\t    0.452\t    0.455\t 26.418%\t 63.364%\t     0.000\t        1\tDelegate/convolution_2d 4:2\n\
\t                        depth_to_space 5\t    0.629\t    0.631\t 36.636%\t100.000%\t     0.000\t        1\tDelegate/depth_to_space 5:3\n\
\n\
============================== Top by Computation Time ==============================\n\
\t                        depth_to_space 5\t    0.629\t    0.631\t 36.636%\t 36.636%\t     0.000\t        1\tDelegate/depth_to_space 5:3\n\
Number of nodes executed: 4\n\
Timings (microseconds): count=108 first=1713 curr=1712 min=1692 max=2254 avg=1722.29 std=52\n\
Memory (bytes): count=0\n\
4 nodes observed\n";

    #[test]
    fn round_log_full_gpu() {
        let r = parse_round_log(LOG);
        assert_eq!(r.delegate, Some(Delegate { replaced: 4, total: 4, partitions: 1 }));
        assert_eq!(r.backend.as_deref(), Some("opencl"));
        assert_eq!(r.invoke_avg_us, Some(8708.66));
        assert_eq!(r.init_us, Some(355447.0));
        let inv = r.invoke.clone().unwrap();
        assert_eq!((inv.count, inv.min, inv.max, inv.median, inv.p95), (108, 8268.0, 9464.0, Some(8705.0), Some(8982.0)));
        // 预热轮的 count 行不能被误采
        assert_ne!(inv.first, 45401.0);
        assert_eq!(r.ops.len(), 4, "只取 Regular 段的 Run Order, 不取初始化段也不取 Top 段");
        assert!(r.ops.iter().all(|o| o.on_gpu));
        assert_eq!(r.ops[3].node_type, "depth_to_space 5");
        assert!((r.gpu_kernel_us().unwrap() - 1723.0).abs() < 1.0);
        assert_eq!(r.profile_total.as_ref().map(|d| d.count), Some(108));
        assert!(r.full_gpu());
        assert!(r.error.is_none());
    }

    #[test]
    fn round_log_partial_and_failed_delegate() {
        let partial = "VERBOSE: Replacing 3 out of 4 node(s) with delegate (TfLiteGpuDelegateV2) node, yielding 2 partitions for subgraph 0.\n\
INFO: Operator-wise Profiling Info for Regular Benchmark Runs:\n\
============================== Run Order ==============================\n\
\t                             [node type]\t  [first]\t [avg ms]\t     [%]\t  [cdf%]\t  [mem KB]\t[times called]\t[Name]\n\
\t                     TfLiteGpuDelegateV2\t    1.000\t    1.100\t 50.000%\t 50.000%\t     0.000\t        1\tDelegate/x:0\n\
\t                                 CONV_2D\t    1.000\t    1.100\t 50.000%\t100.000%\t     0.000\t        1\t[conv]:1\n\
============================== Top by Computation Time ==============================\n";
        let r = parse_round_log(partial);
        assert_eq!(r.delegate, Some(Delegate { replaced: 3, total: 4, partitions: 2 }));
        assert!(!r.full_gpu());
        assert!(r.ops.iter().any(|o| !o.on_gpu));
        let failed = "INFO: Created TensorFlow Lite delegate for GPU.\nERROR: Failed to apply GPU delegate.\nINFO: Inference timings in us: Init: 1, First inference: 2, Warmup (avg): 3, Inference (avg): 4\n";
        let r = parse_round_log(failed);
        assert!(r.delegate_attempted && r.delegate.is_none());
        assert!(!r.full_gpu());
        assert_eq!(r.error.as_deref(), Some("ERROR: Failed to apply GPU delegate."));
        assert_eq!(r.invoke_avg_us, Some(4.0));
        assert!(r.fallback_reason().unwrap().contains("delegate_not_applied"));
        // OpenCL 不认的算子: 覆盖 4/4 却退到 OpenGL 后端 (2026-09-09 floor_mod 实测), 同样否决且原因带算子名
        let gl = "VERBOSE: Replacing 4 out of 4 node(s) with delegate (TfLiteGpuDelegateV2) node, yielding 1 partitions for subgraph 0.\n\
ERROR: No selector for floor_mod\nERROR: Falling back to OpenGL\nINFO: Initialized OpenGL-based API.\n\
INFO: Inference timings in us: Init: 188735, First inference: 26307, Warmup (avg): 7857.5, Inference (avg): 6928.99\n";
        let r = parse_round_log(gl);
        assert!(!r.delegate_attempted, "该片段没有创建行");
        assert_eq!(r.backend.as_deref(), Some("opengl"));
        assert!(!r.full_gpu());
        let why = r.fallback_reason().unwrap();
        assert!(why.contains("backend=opengl") && why.contains("floor_mod"), "{why}");
        assert_eq!(parse_round_log(LOG).fallback_reason(), None);
    }

    #[test]
    fn snapshot_thermal_and_levels() {
        let text = "CPU /sys/devices/system/cpu/cpufreq/policy0 walt 787200 3628800 1324800 0,1,2,3,4,5\n\
CPU /sys/devices/system/cpu/cpufreq/policy6 walt 883200 4396800 883200 6,7\n\
GPU 3 17 0 191000000 Adreno840v2\n\
GPUFREQS 1200000000 1050000000 967000000 902000000 826000000\n\
BUS DDR 547000 5333000 547000\n\
BUS LLCC 282000 1350000 605600\n";
        let s = parse_snapshot(text);
        assert_eq!(s.cpu.len(), 2);
        assert_eq!(s.cpu[1].governor, "walt");
        assert_eq!(s.cpu[1].cpus, "6,7");
        assert_eq!((s.gpu_max_level, s.gpu_min_level, s.gpu_thermal_level), (Some(3), Some(17), Some(0)));
        assert_eq!(s.gpuclk_hz, Some(191000000));
        assert_eq!(s.gpu_model.as_deref(), Some("Adreno840v2"));
        assert_eq!(s.gpu_freqs[3], 902000000);
        assert_eq!(s.bus.len(), 2);
        assert_eq!((s.bus[0].name.as_str(), s.bus[0].floor_khz, s.bus[0].max_khz, s.bus[0].cur_khz), ("DDR", 547000, 5333000, 547000));
        // 状态文件往返: 写出再解析必须与原快照一致 (崩溃回滚的地基)
        assert_eq!(parse_snapshot(&snapshot_text(&s)), s);
        assert_eq!(nearest_level(&s.gpu_freqs, 900), Some(3));
        assert_eq!(nearest_level(&s.gpu_freqs, 1300), Some(0));
        let zones = parse_thermal("cpu-0-0-0 33300\ngpuss-2 41900\nskin-msm-therm 45000\ncpu-hw-trip-0 105000\nsocd 0\nbatt-therm 25470\n");
        assert_eq!(zones.len(), 6);
        // 皮肤温 45C 不参与; 105000 哨兵与 0 值跳过; 取 gpuss-2
        assert_eq!(soc_max_c(&zones), Some(("gpuss-2".into(), 41.9)));
        assert_eq!(soc_max_c(&[]), None);
    }

    #[test]
    fn dispersion_and_samples() {
        let (median, min, max, d) = dispersion_pct(&[1722.0, 1700.0, 1750.0]);
        assert_eq!((median, min, max), (1722.0, 1700.0, 1750.0));
        assert!((d - 50.0 / 1722.0 * 100.0).abs() < 1e-9);
        let (median, _, _, d) = dispersion_pct(&[10.0, 20.0]);
        assert_eq!(median, 15.0);
        assert!((d - 200.0 / 3.0).abs() < 1e-9);
        assert_eq!(dispersion_pct(&[]).3, 0.0);
        let s = parse_samples("902000000 3628800 4396800\n902000000 3628800 4396800\n191000000 3628800 4396800\n");
        assert_eq!(s[0], (902000000, 191000000, 902000000));
        assert_eq!(s[1], (3628800, 3628800, 3628800));
        assert!(parse_samples("").is_empty());
    }

    #[test]
    fn args_forms() {
        let a = parse_args(&["--serial".into(), "X".into(), "--model".into(), "m.tflite".into()]).unwrap();
        assert_eq!((a.runs, a.num_runs, a.warmup, a.limit_ms, a.metric, a.lock, a.fp16), (3, 100, 20, 4.0, Metric::Gpu, true, true));
        let a = parse_args(&["--model".into(), "m".into(), "--runs".into(), "5".into(), "--metric".into(), "invoke".into(),
                             "--no-lock".into(), "--fp32".into(), "--gpu-mhz".into(), "902".into(), "--json".into()]).unwrap();
        assert_eq!((a.runs, a.metric, a.lock, a.fp16, a.gpu_mhz, a.json), (5, Metric::Invoke, false, false, Some(902), true));
        assert!(parse_args(&[]).is_err(), "缺 --model");
        assert!(parse_args(&["--model".into(), "m".into(), "--metric".into(), "cpu".into()]).is_err());
        assert!(parse_args(&["--model".into(), "m".into(), "--gpu-level".into(), "3".into(), "--gpu-mhz".into(), "902".into()]).is_err());
        assert!(parse_args(&["--model".into(), "m".into(), "--bogus".into()]).is_err());
    }

    #[test]
    fn snapshot_cmd_is_single_quoted_root_payload() {
        let c = snapshot_cmd();
        assert!(c.starts_with("su -c '") && c.ends_with("'"));
        // su 载荷内不许出现单引号, 否则远端 sh 会把命令截断
        assert_eq!(c[7..c.len() - 1].matches('\'').count(), 0);
        assert!(c.contains("gpu_available_frequencies"));
        assert!(c.contains("bus_dcvs/$b") && c.contains("DDR LLCC") && c.contains("boost_freq") && !c.contains("hw_min_freq"));
    }

    #[test]
    fn max_rounds_defaults_and_bounds() {
        let a = parse_args(&["--model".into(), "m".into()]).unwrap();
        assert_eq!((a.runs, a.max_rounds), (3, 6));
        let a = parse_args(&["--model".into(), "m".into(), "--runs".into(), "2".into(), "--max-rounds".into(), "2".into()]).unwrap();
        assert_eq!((a.runs, a.max_rounds), (2, 2));
        assert!(parse_args(&["--model".into(), "m".into(), "--runs".into(), "3".into(), "--max-rounds".into(), "2".into()]).is_err());
    }

    #[test]
    fn args_unlock_needs_no_model() {
        let a = parse_args(&["--serial".into(), "X".into(), "--unlock".into()]).unwrap();
        assert!(a.unlock && a.model.is_empty());
    }
}
