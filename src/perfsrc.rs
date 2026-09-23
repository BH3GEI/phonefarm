//! perfsrc: 性能数据来源抽象 —— 两种设备后端, 一份 JSON。
//!
//! 为什么要这层: 安卓侧的功耗来自 `/sys/class/power_supply` 电源轨 (`hwcond`), 帧时来自
//! Vulkan 时间戳与 ftrace; 鸿蒙侧没有对应口径, 官方通道是 HiSmartPerf 的 `SP_daemon`
//! 与 Xpower (`smartperf`)。采集手段没有任何共同点, 但上层 `game_opt_loop` 消费的
//! `eval_report` 只认一组字段 —— `operator_latency_ms` / `fps_p95_ms` / `power_watt` /
//! `psnr_db`。若让上层自己去分辨「这台是安卓还是鸿蒙」, 每加一个平台就要改一次上层,
//! 违反「新增一条上层通路不应该修改内核」。故在这里收口: 上层只拿 `PerfSnapshot`。
//!
//! 这层负责其中两个字段 —— `fps_p95_ms` 与 `power_watt`。另外两个不归它:
//! `operator_latency_ms` 是算子自身的 GPU compute pass 耗时 (Vulkan 时间戳, `gpuop`),
//! `psnr_db` 是画质 (`gpuop` 的 quality pass)。
//!
//! **三条纪律**:
//!   1. **同一份字段**: 两种来源共用同一个 `PerfSnapshot` 结构体, JSON 键名与顺序
//!      由它唯一决定, 不存在「安卓多一个字段、鸿蒙少一个字段」的可能;
//!   2. **采不到就是 null**: 任何量不到的字段都是 `None` (序列化为 `null`),
//!      并在 `unavailable` 里附一条人能读的原因。**绝不填 0** —— 一个 0.000 W
//!      看起来像个数字, 其实是「这条轨此刻量不了」, 会把上层的 A/B 裁决直接带沟里;
//!   3. **不改安卓侧既有行为**: 安卓实现只是把 `hwcond` 现成的采样与可信度判断包一层,
//!      `bench` / `gpu-op` 原有的调用路径一行不动。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::device::Device;
use crate::gpustat;
use crate::hwcond::{self, PowerRail, PowerSample};
use crate::smartperf::{self, SpFlags, SpSample};

/// 来源标识。进 JSON, 让上层一眼看出这份数字是哪条通路量的。
pub const SRC_ANDROID_SYSFS: &str = "android_sysfs";
pub const SRC_HARMONY_SMARTPERF: &str = "harmony_smartperf";

/// `unavailable` 里的特殊字段名: 不是某一个字段缺了, 是**整条采集通路不可用**
/// (设备端没有 SP_daemon、不是鸿蒙设备等)。调用方据此把退出码与「设备在、但这轮没采到」区分开。
pub const FIELD_PIPELINE: &str = "*";

/// 一条「这个字段为什么是 null」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unavailable {
    pub field: String,
    pub reason: String,
}

/// 一次采集的归一化结果。**两种来源共用这一个结构体**, 字段集合因此天然一致。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerfSnapshot {
    /// `android_sysfs` | `harmony_smartperf`
    pub source: String,
    /// 实际拿到的采样点数 (0 表示这次什么都没采到)
    pub sample_count: usize,
    /// 平均帧率
    pub fps: Option<f64>,
    /// 整帧时间均值 (毫秒)
    pub frame_time_mean_ms: Option<f64>,
    /// 整帧时间 p95 (毫秒) —— 对齐 `eval_report.schema.json` 的同名字段
    pub fps_p95_ms: Option<f64>,
    /// 整机功耗 (瓦) —— 对齐 `eval_report.schema.json` 的 `power_watt`
    pub power_watt: Option<f64>,
    /// SoC 结温 (摄氏度)
    pub soc_temp_c: Option<f64>,
    /// GPU 热区 (摄氏度)
    pub gpu_temp_c: Option<f64>,
    /// 电池温度 (摄氏度)
    pub battery_temp_c: Option<f64>,
    /// 采集侧的元数据: 鸿蒙走设备探测 (`SP_daemon -deviceinfo`) 或 HiSmartPerf 报告的
    /// `general.csv`; 安卓侧这条通路没有对应物, 为 null。原样透传, 不解释不换算。
    pub meta: Option<BTreeMap<String, String>>,
    /// 逐条说明上面哪些字段为什么是 null
    pub unavailable: Vec<Unavailable>,
}

impl PerfSnapshot {
    /// 一份「什么都还没量到」的快照。所有量值字段都是 null, 不是 0。
    pub fn empty(source: &str) -> Self {
        PerfSnapshot {
            source: source.to_string(),
            sample_count: 0,
            fps: None,
            frame_time_mean_ms: None,
            fps_p95_ms: None,
            power_watt: None,
            soc_temp_c: None,
            gpu_temp_c: None,
            battery_temp_c: None,
            meta: None,
            unavailable: Vec::new(),
        }
    }

    fn miss(&mut self, field: &str, reason: impl Into<String>) {
        self.unavailable.push(Unavailable { field: field.into(), reason: reason.into() });
    }
}

/// 性能数据来源。一个设备后端一个实现。
pub trait PerfSource {
    /// 来源标识, 与 `PerfSnapshot::source` 同值。
    fn id(&self) -> &'static str;
    /// 采一轮。`rounds` 是采集点数 (鸿蒙侧即 `SP_daemon -N`, 安卓侧即读电源轨的次数)。
    fn collect(&self, phone: &Device, rounds: u32) -> PerfSnapshot;
}

// ══════════════ 安卓: sysfs 电源轨 ══════════════

/// 两次电源轨采样之间的间隔, 与 `gpu-op` 的功耗探针同节拍。
pub const POWER_SAMPLE_GAP_MS: u64 = 200;

/// 安卓来源: 复用 `hwcond` 现成的电源轨采样与可信度判断, 不改它一行。
pub struct AndroidSysfs {
    pub rail: PowerRail,
}

impl Default for AndroidSysfs {
    fn default() -> Self {
        AndroidSysfs { rail: PowerRail::Usb }
    }
}

/// 纯函数: 一组电源轨采样 → 归一化快照。
///
/// 安卓侧这条通路只量功耗。帧时不在这里 —— 它走 raw ftrace 的 kgsl 事件与 Vulkan
/// 时间戳 (`ftrace` / `gpuop`), 由 `gpu-op` 自己填进 `eval_report`。所以这里的
/// 帧时字段是 null 且附原因, 而不是 0。
pub fn summarize_android(samples: &[PowerSample], rail: PowerRail) -> PerfSnapshot {
    let mut snap = PerfSnapshot::empty(SRC_ANDROID_SYSFS);
    snap.sample_count = samples.len();
    // 轨别必须进快照: usb 轨量的是墙上功率 (含充电与转换损耗), battery 轨量的是电池
    // 真实抽走的功率, 两者不可比。不记下来, 下游拿 usb 轨的基线对 battery 轨的候选,
    // 会凭空多出一个「功耗改善」。
    snap.meta = Some(BTreeMap::from([("power_rail".to_string(), rail.as_str().to_string())]));
    match hwcond::power_usable(rail, samples) {
        Ok(()) => {
            // power_usable 已保证 power_stats 有值
            snap.power_watt = hwcond::power_stats(samples).map(|(mean, _, _, _)| mean);
        }
        Err(e) => snap.miss("power_watt", e),
    }
    for f in ["fps", "frame_time_mean_ms", "fps_p95_ms"] {
        snap.miss(f, "安卓侧帧时走 ftrace kgsl 事件与 Vulkan 时间戳, 不由电源轨采集源提供");
    }
    for f in ["soc_temp_c", "gpu_temp_c", "battery_temp_c"] {
        snap.miss(f, "安卓侧热区由 hwcond 的冷机门禁单独读取, 不由电源轨采集源提供");
    }
    snap
}

impl PerfSource for AndroidSysfs {
    fn id(&self) -> &'static str {
        SRC_ANDROID_SYSFS
    }

    fn collect(&self, phone: &Device, rounds: u32) -> PerfSnapshot {
        let mut samples = Vec::new();
        for i in 0..rounds.max(1) {
            if let Some(s) = hwcond::read_power(phone, self.rail) {
                samples.push(s);
            }
            // 与 gpu-op 的功耗探针同节拍。不隔开就是「把同一瞬间读 30 遍」,
            // 报出来却是 sample_count: 30, 看着像一个 30 秒的窗口。
            if i + 1 < rounds.max(1) {
                std::thread::sleep(std::time::Duration::from_millis(POWER_SAMPLE_GAP_MS));
            }
        }
        summarize_android(&samples, self.rail)
    }
}

// ══════════════ 鸿蒙: HiSmartPerf (SP_daemon + Xpower) ══════════════

/// 鸿蒙来源: 走 hdc 调设备端 `SP_daemon`, 功耗侧另刷 Xpower 落盘。
pub struct HarmonySmartPerf {
    /// 目标应用包名 (`-PKG`)。不给就是整机口径。
    pub pkg: Option<String>,
    /// 采集项开关。
    pub flags: SpFlags,
    /// 给了目录就额外把 Xpower 的 `dubai.db` 刷盘并拉回来, 供离线分析分器件能耗。
    ///
    /// **默认关**: 刷盘要先 `rm -rf` 设备上的旧 db 再跑三条 `hidumper`, 是有副作用的写操作;
    /// 而本模块目前不解析 SQLite, 拉回来也只是留给人看。不主动删设备上的东西,
    /// 需要时由调用方显式要。
    pub xpower_out: Option<std::path::PathBuf>,
}

impl Default for HarmonySmartPerf {
    fn default() -> Self {
        HarmonySmartPerf { pkg: None, flags: SpFlags::default(), xpower_out: None }
    }
}

/// 纯函数: 一组 `SP_daemon` 采样 → 归一化快照。
pub fn summarize_harmony(samples: &[SpSample]) -> PerfSnapshot {
    let mut snap = PerfSnapshot::empty(SRC_HARMONY_SMARTPERF);
    snap.sample_count = samples.len();
    if samples.is_empty() {
        for f in [
            "fps",
            "frame_time_mean_ms",
            "fps_p95_ms",
            "power_watt",
            "soc_temp_c",
            "gpu_temp_c",
            "battery_temp_c",
        ] {
            snap.miss(f, "SP_daemon 没给出任何采样点");
        }
        return snap;
    }

    // ── 帧率 ──
    let fps: Vec<f64> =
        samples.iter().filter_map(|s| s.fps).filter(|v| v.is_finite() && *v > 0.0).collect();
    if fps.is_empty() {
        snap.miss("fps", "SP_daemon 未上报 fps (未开 -f, 或该应用的帧数据拿不到)");
    } else {
        snap.fps = Some(gpustat::mean(&fps));
    }

    // ── 帧时 ──
    let ft: Vec<f64> = samples
        .iter()
        .flat_map(|s| s.frame_intervals_ms())
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
    if ft.is_empty() {
        let why = "SP_daemon 未上报 fpsJitters (未开 -f, 或该应用的逐帧间隔拿不到)";
        snap.miss("frame_time_mean_ms", why);
        snap.miss("fps_p95_ms", why);
    } else {
        snap.frame_time_mean_ms = Some(gpustat::mean(&ft));
        let p95 = gpustat::percentile(&ft, 0.95);
        if p95.is_finite() {
            snap.fps_p95_ms = Some(p95);
        } else {
            snap.miss("fps_p95_ms", "帧间隔样本算不出 p95");
        }
    }

    // ── 功耗 ──
    match smartperf::power_watt_of(samples) {
        Ok(w) => snap.power_watt = Some(w),
        Err(e) => snap.miss("power_watt", e),
    }

    // ── 热区 ──
    // 与帧率同一把尺子: 非有限值和 0 都不是测量结果。
    // 这台机器的 `gpu_max_freq,0` 就是活例 —— 传感器/计数器不支持时设备照样回 0,
    // 把它当成「0 摄氏度」发出去比不给更糟。
    let temps = |pick: fn(&SpSample) -> Option<f64>| -> Vec<f64> {
        samples.iter().filter_map(pick).filter(|v| v.is_finite() && *v > 0.0).collect()
    };
    for (field, vals) in [
        ("soc_temp_c", temps(|s| s.soc_temp_c)),
        ("gpu_temp_c", temps(|s| s.gpu_temp_c)),
        ("battery_temp_c", temps(|s| s.battery_temp_c)),
    ] {
        if vals.is_empty() {
            snap.miss(field, "SP_daemon 未上报该热区 (未开 -t, 或该机型没有这个传感器)");
            continue;
        }
        let mean = gpustat::mean(&vals);
        match field {
            "soc_temp_c" => snap.soc_temp_c = Some(mean),
            "gpu_temp_c" => snap.gpu_temp_c = Some(mean),
            _ => snap.battery_temp_c = Some(mean),
        }
    }

    snap
}

/// `SP_daemon --version` 的回包看着像不像「这台机器有 SP_daemon」。
///
/// 独立成纯函数是因为这是整条鸿蒙通路的第一道岔口: 设备端没有 SP_daemon 时,
/// hdc 回的是 shell 的 "not found" 之类, 不能把它当成一个版本号往下走。
pub fn version_looks_present(out: &str) -> bool {
    let t = out.trim();
    if t.is_empty() {
        return false;
    }
    let low = t.to_ascii_lowercase();
    for bad in ["not found", "no such file", "inaccessible", "permission denied", "[fail]"] {
        if low.contains(bad) {
            return false;
        }
    }
    // 版本号形态: 至少出现一个数字
    t.chars().any(|c| c.is_ascii_digit())
}

impl HarmonySmartPerf {
    /// 设备端探测: SP_daemon 在不在, 以及它自报的设备信息。
    pub fn probe(&self, phone: &Device) -> Result<BTreeMap<String, String>, String> {
        let ver = phone.shell(&smartperf::version_cmd(), 10_000);
        if !version_looks_present(&ver) {
            return Err(format!(
                "设备端没有可用的 SP_daemon (`{}` 回的是: {})。\
                 SP_daemon 自 API 9 起随 OpenHarmony 预置, 拿不到说明这台不是鸿蒙设备或系统裁剪过",
                smartperf::version_cmd(),
                ver.trim().lines().next().unwrap_or("<空>")
            ));
        }
        let mut meta = smartperf::parse_kv_block(&phone.shell(&smartperf::deviceinfo_cmd(), 15_000));
        meta.insert("SP_daemon_version".into(), ver.trim().to_string());
        Ok(meta)
    }

    /// 刷 Xpower 落盘并把 `dubai.db` 拉回本机, 供离线分析分器件能耗。
    ///
    /// 有副作用 (先删设备上的旧 db 再刷), 只在调用方显式给了输出目录时才跑。
    /// 拉回来的是 SQLite, 本模块不解析它 —— 落盘路径写进 `meta`, 由人或别的工具接手。
    pub fn pull_xpower(&self, phone: &Device, out_dir: &std::path::Path) -> Result<String, String> {
        std::fs::create_dir_all(out_dir)
            .map_err(|e| format!("建不了 Xpower 输出目录 {}: {e}", out_dir.display()))?;
        // 先删旧 db 再刷, 免得拉到上一轮的陈旧内容 (HiSmartPerf 自己也是这个顺序)
        phone.shell(&smartperf::xpower_rm_cmd(), 8_000);
        for c in smartperf::xpower_flush_cmds() {
            phone.shell(&c, 8_000);
        }
        let local = out_dir.join("dubai.db");
        let local_s = local.to_string_lossy().to_string();
        if !phone.pull(smartperf::XPOWER_DB_REMOTE, &local_s) || !local.exists() {
            return Err(format!(
                "{} 拉不回来 (Xpower 服务可能没在采, 或这台机器没有这个落盘)",
                smartperf::XPOWER_DB_REMOTE
            ));
        }
        Ok(local_s)
    }
}

impl PerfSource for HarmonySmartPerf {
    fn id(&self) -> &'static str {
        SRC_HARMONY_SMARTPERF
    }

    fn collect(&self, phone: &Device, rounds: u32) -> PerfSnapshot {
        let rounds = rounds.max(1);
        let mut meta = match self.probe(phone) {
            Ok(m) => m,
            Err(e) => {
                let mut snap = PerfSnapshot::empty(SRC_HARMONY_SMARTPERF);
                snap.miss(FIELD_PIPELINE, e);
                return snap;
            }
        };
        // 每个采集点约一秒, 再给 hdc 往返留 15 秒余量
        let budget_ms = (rounds as u64) * 1_000 + 15_000;

        // 上一轮的 data.csv 会原地留在设备上。不先删掉, 这一轮若超时或失败,
        // `cat` 读回来的就是上一轮的数据 —— 一份陈旧读数冒充本次实测, 比采不到更糟。
        phone.shell(&format!("rm -f {}", smartperf::SP_CSV_REMOTE), 8_000);
        phone.shell(&smartperf::clear_cmd(), 8_000);

        let cmd = smartperf::collect_cmd(rounds, self.pkg.as_deref(), self.flags);
        let stdout = phone.shell(&cmd, budget_ms);

        // 默认口径读设备落盘的 data.csv (官方文档指定的产物, 列名有据可查);
        // 读不到再退回 stdout —— stdout 的逐行布局尚未在真机上验证过。
        let csv = phone.shell(&format!("cat {}", smartperf::SP_CSV_REMOTE), 15_000);
        let mut samples = smartperf::parse_sp_csv(&csv);
        let mut via = "data.csv";
        if samples.is_empty() {
            samples = smartperf::parse_sp_stdout(&stdout);
            via = "stdout(未上真机验证的布局)";
        }
        meta.insert("parsed_via".into(), via.to_string());

        if let Some(dir) = &self.xpower_out {
            let (k, v) = match self.pull_xpower(phone, dir) {
                Ok(p) => ("xpower_db", p),
                Err(e) => ("xpower_db_error", e),
            };
            meta.insert(k.into(), v);
        }

        let mut snap = summarize_harmony(&samples);
        snap.meta = Some(meta);
        if snap.sample_count == 0 {
            snap.miss(
                "sample_count",
                format!("`{cmd}` 与 {} 都没拿到可解析的采样", smartperf::SP_CSV_REMOTE),
            );
        }
        snap
    }
}

/// 离线口径: 解析一份已经拉回本机的 `SP_daemon` `data.csv`, 不碰设备。
///
/// 存在的意义: 采集与解析必须能分开跑 —— 数据在盘上, 任何时候重算都该逐字节一致;
/// 也让「没有鸿蒙真机」时仍能用真实产物验证整条解析链。
/// 同目录若有 HiSmartPerf 报告的 `general.csv`, 一并读进 `meta` 当本轮的采集元数据。
pub fn snapshot_from_csv_file(path: &std::path::Path) -> Result<PerfSnapshot, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("读不了 {}: {e}", path.display()))?;
    let mut snap = summarize_harmony(&smartperf::parse_sp_csv(&text));
    if let Some(general) = path.parent().map(|d| d.join("general.csv")) {
        if let Ok(g) = std::fs::read_to_string(&general) {
            let m = smartperf::parse_report_general(&g);
            if !m.is_empty() {
                snap.meta = Some(m);
            }
        }
    }
    Ok(snap)
}

// ══════════════ 按设备后端选来源 ══════════════

/// 设备后端 → 采集源。`hdc:` 前缀的设备走鸿蒙, 其余走安卓, 与 `Device::new` 同一判据。
pub fn for_device(phone: &Device, a: &PerfArgs) -> Box<dyn PerfSource> {
    match phone.backend_name() {
        "hdc" => Box::new(HarmonySmartPerf {
            pkg: a.pkg.clone(),
            xpower_out: a.xpower_out.as_ref().map(std::path::PathBuf::from),
            ..Default::default()
        }),
        _ => Box::new(AndroidSysfs { rail: a.rail }),
    }
}

// ══════════════ 子命令 ══════════════

#[derive(Debug, PartialEq)]
pub struct PerfArgs {
    pub serial: Option<String>,
    pub pkg: Option<String>,
    pub rounds: u32,
    pub rail: PowerRail,
    pub json: bool,
    /// 离线口径: 直接解析一份已拉回本机的 `data.csv`, 完全不碰设备
    pub from_csv: Option<String>,
    /// 额外把 Xpower 的 `dubai.db` 刷盘并拉到这个目录 (鸿蒙侧专用, 有副作用, 默认不做)
    pub xpower_out: Option<String>,
}

/// 参数解析是纯函数, 由单测钉死 —— 免得「换个参数顺序就采错轨」这种事只能上真机才发现。
pub fn parse_perf_args(args: &[String]) -> Result<PerfArgs, String> {
    let mut a = PerfArgs {
        serial: None,
        pkg: None,
        rounds: 10,
        rail: PowerRail::Usb,
        json: false,
        from_csv: None,
        xpower_out: None,
    };
    let mut it = args.iter().peekable();
    // 取一个值: 后面跟的若是另一个开关, 说明这个参数的值漏了 —— 报错, 别把 `--json` 当包名吞掉
    macro_rules! val {
        ($k:expr) => {{
            match it.peek() {
                Some(v) if !v.starts_with("--") => it.next().unwrap().clone(),
                _ => return Err(format!("{} 后面要跟一个值", $k)),
            }
        }};
    }
    while let Some(k) = it.next() {
        match k.as_str() {
            "--serial" => a.serial = Some(val!("--serial")),
            "--app" | "--pkg" => a.pkg = Some(val!(k)),
            "--rounds" => {
                let v = val!("--rounds");
                a.rounds = v.parse().map_err(|_| format!("--rounds 不是数字: {v}"))?;
            }
            "--power-rail" => {
                let v = val!("--power-rail");
                a.rail = match v.as_str() {
                    "usb" => PowerRail::Usb,
                    "battery" => PowerRail::Battery,
                    other => return Err(format!("--power-rail 只能是 usb 或 battery, 给的是 {other}")),
                }
            }
            "--from-csv" => a.from_csv = Some(val!("--from-csv")),
            "--xpower-out" => a.xpower_out = Some(val!("--xpower-out")),
            "--json" => a.json = true,
            other if other.starts_with("--") => return Err(format!("perf: 未知参数 {other}")),
            _ => {}
        }
    }
    if a.rounds == 0 {
        return Err("--rounds 至少为 1".into());
    }
    Ok(a)
}

/// `phonefarm perf`: 采一次归一化性能快照。零 Token, 只读设备。
///
/// 退出码: 0 = 采到了至少一个采样点; 1 = 设备在但什么都没采到; 2 = 参数或设备不可用。
pub fn run_perf(args: &[String]) -> i32 {
    let a = match parse_perf_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    // 离线口径: 盘上已有 data.csv, 不碰设备
    if let Some(p) = &a.from_csv {
        return match snapshot_from_csv_file(std::path::Path::new(p)) {
            Ok(snap) => {
                emit_perf(&snap, a.json);
                exit_code_of(&snap)
            }
            Err(e) => {
                eprintln!("{e}");
                2
            }
        };
    }

    let tmp = std::env::temp_dir().join(format!("phonefarm-perf-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&tmp) {
        eprintln!("建不了临时目录 {}: {e}", tmp.display());
        return 2;
    }
    let phone = Device::new(a.serial.clone(), tmp.to_string_lossy().to_string());
    if !phone.health_check(8_000) {
        eprintln!("设备无心跳 (devices 里不是在线态, 或未指定 --serial)");
        return 2;
    }
    let src = for_device(&phone, &a);
    // 进度一律走 stderr: --json 时 stdout 只能有 JSON, 而「这次走的是哪条采集源」必须看得见
    hwcond::progress("perf", &format!("采集源 {} · {} 个采集点", src.id(), a.rounds));
    let snap = src.collect(&phone, a.rounds);

    emit_perf(&snap, a.json);
    exit_code_of(&snap)
}

/// 退出码: `0` = 采到了至少一个采样点; `1` = 设备在、但这轮什么都没采到;
/// `2` = 整条采集通路不可用 (设备端没有 SP_daemon 等)。
pub fn exit_code_of(snap: &PerfSnapshot) -> i32 {
    if snap.unavailable.iter().any(|u| u.field == FIELD_PIPELINE) {
        return 2;
    }
    if snap.sample_count == 0 {
        1
    } else {
        0
    }
}

fn emit_perf(snap: &PerfSnapshot, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(snap).unwrap_or_default());
    } else {
        print!("{}", render_perf_text(snap));
    }
}

/// 快照 → 人读的文本。采不到的字段显式写「未测到」, 不拿 0 冒充。
pub fn render_perf_text(s: &PerfSnapshot) -> String {
    fn v(x: Option<f64>, unit: &str) -> String {
        match x {
            Some(n) => format!("{n:.3} {unit}"),
            None => "未测到".into(),
        }
    }
    let mut out = format!("来源 {}  采样点 {}\n", s.source, s.sample_count);
    out.push_str(&format!("  帧率        {}\n", v(s.fps, "fps")));
    out.push_str(&format!("  帧时均值    {}\n", v(s.frame_time_mean_ms, "ms")));
    out.push_str(&format!("  帧时 p95    {}\n", v(s.fps_p95_ms, "ms")));
    out.push_str(&format!("  整机功耗    {}\n", v(s.power_watt, "W")));
    out.push_str(&format!("  SoC / GPU / 电池温度  {} / {} / {}\n",
        v(s.soc_temp_c, "C"), v(s.gpu_temp_c, "C"), v(s.battery_temp_c, "C")));
    if !s.unavailable.is_empty() {
        out.push_str("  未测到的字段:\n");
        for u in &s.unavailable {
            out.push_str(&format!("    - {}: {}\n", u.field, u.reason));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smartperf::parse_sp_csv;

    const SP_CSV: &str = include_str!("testdata/sp_daemon_data.csv");

    fn keys_of(snap: &PerfSnapshot) -> Vec<String> {
        let v = serde_json::to_value(snap).unwrap();
        v.as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn both_sources_emit_exactly_the_same_json_field_set() {
        let a = summarize_android(
            &[PowerSample { volt_uv: 5_127_000, curr_ua: 144_000, power_uw: None }],
            PowerRail::Usb,
        );
        let h = summarize_harmony(&parse_sp_csv(SP_CSV));
        assert_eq!(keys_of(&a), keys_of(&h), "两种来源的 JSON 字段集合必须逐字相同");
        assert_eq!(a.source, SRC_ANDROID_SYSFS);
        assert_eq!(h.source, SRC_HARMONY_SMARTPERF);
    }

    #[test]
    fn unmeasured_fields_serialize_as_null_never_zero() {
        let h = summarize_harmony(&[]);
        let v = serde_json::to_value(&h).unwrap();
        for f in ["fps", "frame_time_mean_ms", "fps_p95_ms", "power_watt", "soc_temp_c"] {
            assert!(v[f].is_null(), "{f} 应为 null, 实为 {}", v[f]);
        }
        assert_eq!(v["sample_count"], 0);
        assert_eq!(h.unavailable.len(), 7, "每个量不到的字段都要有一条原因");
    }

    #[test]
    fn harmony_summary_uses_the_real_genshin_numbers() {
        let h = summarize_harmony(&parse_sp_csv(SP_CSV));
        assert_eq!(h.sample_count, 3);
        // 实测 29.8 / 29.9 / 29.8
        let fps = h.fps.unwrap();
        assert!((fps - 29.8333).abs() < 1e-3, "实得 {fps}");
        // 锁 30 帧, 帧间隔全是 33.3 ms —— 均值与 p95 都该落在 33.3
        assert!((h.frame_time_mean_ms.unwrap() - 33.3).abs() < 1e-9);
        assert!((h.fps_p95_ms.unwrap() - 33.3).abs() < 1e-9);
        assert!((h.gpu_temp_c.unwrap() - 45.7).abs() < 1e-9);
        assert!((h.battery_temp_c.unwrap() - 40.7).abs() < 1e-9);
    }

    #[test]
    fn harmony_rejects_the_charging_power_reading_instead_of_reporting_it() {
        let h = summarize_harmony(&parse_sp_csv(SP_CSV));
        assert_eq!(h.power_watt, None, "充电态下的三千瓦读数绝不能进契约");
        let why = h.unavailable.iter().find(|u| u.field == "power_watt").expect("要说明为什么");
        assert!(why.reason.contains("充电"), "{}", why.reason);
    }

    #[test]
    fn harmony_reports_soc_temp_as_null_when_the_sensor_is_absent() {
        // 真实夹具里 soc_thermal / shell_frame 两列是空的
        let h = summarize_harmony(&parse_sp_csv(SP_CSV));
        assert_eq!(h.soc_temp_c, None);
        assert!(h.unavailable.iter().any(|u| u.field == "soc_temp_c"));
    }

    #[test]
    fn android_summary_keeps_hwcond_semantics() {
        // 放电态可用读数直接透传
        let ok = summarize_android(
            &[PowerSample { volt_uv: 5_127_000, curr_ua: 144_000, power_uw: None }],
            PowerRail::Usb,
        );
        assert!((ok.power_watt.unwrap() - 5.127 * 0.144).abs() < 1e-6);
        assert_eq!(ok.sample_count, 1);

        // 充电中电池轨恒 0 → null + 原因, 不是 0.000 W
        let charging = summarize_android(
            &[PowerSample { volt_uv: 4_411_000, curr_ua: 0, power_uw: Some(0) }],
            PowerRail::Battery,
        );
        assert_eq!(charging.power_watt, None);
        assert!(charging
            .unavailable
            .iter()
            .any(|u| u.field == "power_watt" && u.reason.contains("充电")));
    }

    #[test]
    fn android_frame_time_is_null_because_it_comes_from_another_channel() {
        let a = summarize_android(&[], PowerRail::Usb);
        assert_eq!(a.fps_p95_ms, None);
        let why = a.unavailable.iter().find(|u| u.field == "fps_p95_ms").unwrap();
        assert!(why.reason.contains("ftrace"), "{}", why.reason);
    }

    #[test]
    fn source_selection_follows_the_device_backend() {
        let a = parse_perf_args(&[]).unwrap();
        let oh = Device::new(Some("hdc:5ce1227d".into()), "/tmp".into());
        assert_eq!(for_device(&oh, &a).id(), SRC_HARMONY_SMARTPERF);
        let android = Device::new(Some("emulator-5554".into()), "/tmp".into());
        assert_eq!(for_device(&android, &a).id(), SRC_ANDROID_SYSFS);
        let default = Device::new(None, "/tmp".into());
        assert_eq!(for_device(&default, &a).id(), SRC_ANDROID_SYSFS);
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn parses_perf_args() {
        let a = parse_perf_args(&argv("--serial hdc:5ce1227d --app com.example.demo --rounds 30 --power-rail battery --json"))
            .unwrap();
        assert_eq!(a.serial.as_deref(), Some("hdc:5ce1227d"));
        assert_eq!(a.pkg.as_deref(), Some("com.example.demo"));
        assert_eq!(a.rounds, 30);
        assert_eq!(a.rail, PowerRail::Battery);
        assert!(a.json);

        // 缺省: 10 个采集点、USB 轨、文本输出
        let d = parse_perf_args(&[]).unwrap();
        assert_eq!((d.rounds, d.rail, d.json), (10, PowerRail::Usb, false));
        assert_eq!(d.from_csv, None);

        let off = parse_perf_args(&argv("--from-csv /tmp/data.csv --json")).unwrap();
        assert_eq!(off.from_csv.as_deref(), Some("/tmp/data.csv"));
    }

    #[test]
    fn bad_perf_args_are_rejected_with_a_readable_reason() {
        assert!(parse_perf_args(&argv("--rounds abc")).is_err());
        assert!(parse_perf_args(&argv("--rounds 0")).is_err());
        assert!(parse_perf_args(&argv("--power-rail wall")).is_err());
        assert!(parse_perf_args(&argv("--nonesuch")).is_err());
        // 漏了值的参数不能把下一个开关吞掉 —— `--app --json` 曾会把包名设成 "--json"、
        // 顺手关掉 JSON 输出, 还把 `-PKG --json` 发给设备
        for bad in ["--app --json", "--serial --json", "--rounds --json", "--from-csv --json",
                    "--power-rail --json", "--xpower-out --json", "--app"] {
            assert!(parse_perf_args(&argv(bad)).is_err(), "{bad} 应当报错");
        }
    }

    #[test]
    fn text_render_says_not_measured_rather_than_zero() {
        let t = render_perf_text(&summarize_harmony(&[]));
        assert!(t.contains("未测到"));
        assert!(!t.contains("0.000 W"), "缺失的功耗绝不能渲染成 0.000 W:\n{t}");
    }

    #[test]
    fn a_zero_or_nan_temperature_is_absence_not_a_reading() {
        use crate::smartperf::SpSample;
        // 传感器不支持时设备照样回 0 (这台机器的 gpu_max_freq,0 就是活例)
        let zero = SpSample { gpu_temp_c: Some(0.0), battery_temp_c: Some(f64::NAN), ..Default::default() };
        let h = summarize_harmony(&[zero]);
        assert_eq!(h.gpu_temp_c, None, "0 摄氏度不是一个测量结果");
        assert_eq!(h.battery_temp_c, None, "NaN 不能被当成 null 悄悄发出去");
        for f in ["gpu_temp_c", "battery_temp_c"] {
            assert!(h.unavailable.iter().any(|u| u.field == f), "{f} 缺了要有原因");
        }
    }

    #[test]
    fn android_snapshot_records_which_power_rail_it_measured() {
        for rail in [PowerRail::Usb, PowerRail::Battery] {
            let s = summarize_android(&[], rail);
            assert_eq!(
                s.meta.as_ref().and_then(|m| m.get("power_rail")).map(String::as_str),
                Some(rail.as_str()),
                "两条轨量的不是一回事, 不记下来下游会拿它们互相对比"
            );
        }
    }

    #[test]
    fn exit_code_separates_unusable_pipeline_from_zero_samples() {
        let ok = summarize_harmony(&parse_sp_csv(SP_CSV));
        assert_eq!(exit_code_of(&ok), 0);

        let nothing = summarize_harmony(&[]);
        assert_eq!(exit_code_of(&nothing), 1, "设备在、这轮没采到");

        let mut broken = PerfSnapshot::empty(SRC_HARMONY_SMARTPERF);
        broken.miss(FIELD_PIPELINE, "设备端没有可用的 SP_daemon");
        assert_eq!(exit_code_of(&broken), 2, "整条通路不可用");
    }

    #[test]
    fn version_probe_rejects_a_missing_sp_daemon() {
        assert!(version_looks_present("1.0.3"));
        assert!(version_looks_present("SP_daemon version: 1.0.3"));
        assert!(!version_looks_present(""));
        assert!(!version_looks_present("   "));
        assert!(!version_looks_present("sh: SP_daemon: not found"));
        assert!(!version_looks_present("/system/bin/sh: SP_daemon: inaccessible or not found"));
        assert!(!version_looks_present("Permission denied"));
        // 没有任何数字的回包不算版本号
        assert!(!version_looks_present("unknown command"));
    }

    #[test]
    fn offline_csv_mode_parses_without_touching_a_device() {
        let dir = std::env::temp_dir().join(format!("pf-perfsrc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("data.csv");
        std::fs::write(&csv, SP_CSV).unwrap();
        std::fs::write(dir.join("general.csv"), include_str!("testdata/hismartperf_general.csv")).unwrap();

        let snap = snapshot_from_csv_file(&csv).unwrap();
        assert_eq!(snap.sample_count, 3);
        assert!((snap.fps_p95_ms.unwrap() - 33.3).abs() < 1e-9);
        // 同目录的 general.csv 被当成本轮采集元数据读了进来
        let meta = snap.meta.as_ref().expect("有 general.csv 就该有 meta");
        assert_eq!(meta.get("testDuration").map(String::as_str), Some("119"));
        assert_eq!(meta.get("target_fps").map(String::as_str), Some("60"));

        // 同一份盘上数据重算两次必须逐字节一致
        assert_eq!(snap, snapshot_from_csv_file(&csv).unwrap());

        assert!(snapshot_from_csv_file(&dir.join("nope.csv")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let h = summarize_harmony(&parse_sp_csv(SP_CSV));
        let s = serde_json::to_string(&h).unwrap();
        let back: PerfSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(h, back);
    }
}
