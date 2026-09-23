//! hwcond: 硬件条件化与功耗遥测 —— 被多条上层通路共享的通用能力。
//!
//! 为什么单独成模块: `bench`(TFLite 标尺) 与 `gpu-op`(Compute Shader 标尺) 都需要
//! 「等冷 → 锁频 → 测 → 立刻解锁 → 回读校验」这同一套设备条件化流程。
//! 金科玉律「上层各通路互不依赖」禁止 gpu-op 去 import bench 的私有函数;
//! 同一段逻辑抄两份又必然随时间漂移, 违反「遥测口径全局统一」。
//! 故按规则把它提为共享能力: 谁都能用, 谁都不拥有。
//!
//! 本模块只做两件事, 都是设备状态的读与受控写:
//!   1. **条件化**: 热区读取 / 冷机门禁 / CPU 调速器与 kgsl 档位与总线下限的锁定与还原;
//!   2. **功耗遥测**: 从 power_supply 电源轨读瞬时功率。
//!
//! 纪律 (与 bench 原有口径完全一致, 代码由 bench.rs 原样迁出):
//!   - 所有 sysfs 写入在退出前恢复原值, 无论中途成功失败;
//!   - 锁前先把快照落到本机状态文件, 进程被杀也能由下次启动回滚;
//!   - 解析全是纯函数, 同一份输入每次重算逐字节一致。

use std::time::{Duration, Instant};

pub const KGSL: &str = "/sys/class/kgsl/kgsl-3d0";
pub const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
/// 内存总线 DCVS (Qualcomm bus_dcvs): DDR 与 LLCC 的用户态下限 boost_freq 在跑测期间钉到 hw_max_freq
/// (hw_min_freq 是内核私有只读节点, root 也写不进)。
/// 反例 (2026-09-09): DDR 待机 547MHz 随负载爬升, resize 这类访存算子轮间离散 5.3%; 钉住后消除。
pub const BUS_DCVS: &str = "/sys/devices/system/cpu/bus_dcvs";
pub const BUS_NODES: [&str; 2] = ["DDR", "LLCC"];
/// 热区前缀: SoC 结温 (CPU 各核 / CPU LLC / GPU 子系统); 皮肤温等外壳传感器不参与冷机判定
pub const SOC_ZONE_PREFIXES: [&str; 3] = ["cpu-", "cpullc", "gpuss"];

/// 进度一律走 stderr: `--json` 时 stdout 只有 JSON, 而卡在哪一步必须能被看见
/// (等冷死循环的教训)。tag 由调用方给, 便于分辨是哪条通路在等。
pub fn progress(tag: &str, msg: &str) {
    eprintln!("[{tag}] {msg}");
}

/// 锁频前的设备快照落到本机状态文件 (按通路与 serial 区分);
/// 进程被杀留下的锁态由下次启动或 `--unlock` 回滚。
pub fn lock_state_path(tag: &str, serial: &Option<String>) -> std::path::PathBuf {
    let s: String = serial
        .as_deref()
        .unwrap_or("default")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    std::env::temp_dir().join(format!("phonefarm-{tag}-lock-{s}.txt"))
}

// ══════════════ 设备快照 ══════════════

#[derive(Debug, Clone, PartialEq)]
pub struct CpuPolicy {
    pub path: String,
    pub governor: String,
    pub min_khz: u64,
    pub max_khz: u64,
    pub cur_khz: u64,
    pub cpus: String,
}

#[derive(Debug, Clone, PartialEq)]
/// floor_khz = boost_freq (用户态下限), max_khz = hw_max_freq, cur_khz = cur_freq
pub struct BusNode {
    pub name: String,
    pub floor_khz: u64,
    pub max_khz: u64,
    pub cur_khz: u64,
}

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

/// 快照命令 (root): 一次 su 往返把 CPU 各簇 / kgsl 档位 / 频率表全采回
pub fn snapshot_cmd() -> String {
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
            Some("GPUFREQS") => {
                s.gpu_freqs = cols[1..].iter().filter_map(|v| v.parse().ok()).collect()
            }
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
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let ty = it.next()?;
            let temp: i64 = it.next()?.parse().ok()?;
            Some((ty.to_string(), temp))
        })
        .collect()
}

/// SoC 结温最高的热区 (只看 cpu-/cpullc/gpuss 前缀; 0 或 >=100C 的哨兵值跳过)
pub fn soc_max_c(zones: &[(String, i64)]) -> Option<(String, f64)> {
    zones
        .iter()
        .filter(|(ty, t)| {
            SOC_ZONE_PREFIXES.iter().any(|p| ty.starts_with(p)) && *t > 0 && *t < 100_000
        })
        .max_by_key(|(_, t)| *t)
        .map(|(ty, t)| (ty.clone(), *t as f64 / 1000.0))
}

/// 目标 MHz → 频率表里最接近的档位索引
pub fn nearest_level(freqs: &[u64], mhz: u64) -> Option<u32> {
    let target = mhz * 1_000_000;
    freqs
        .iter()
        .enumerate()
        .min_by_key(|(_, f)| (**f as i64 - target as i64).abs())
        .map(|(i, _)| i as u32)
}

/// (Max-Min)/Median, 百分比; 样本不足 2 个记 0
pub fn dispersion_pct(vals: &[f64]) -> (f64, f64, f64, f64) {
    let mut v: Vec<f64> = vals.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let median = if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    };
    let (min, max) = (v[0], v[n - 1]);
    let disp = if median > 0.0 && n >= 2 {
        (max - min) / median * 100.0
    } else {
        0.0
    };
    (median, min, max, disp)
}

/// 采样器输出 "gpuclk cpu0 cpu6 ..." 行 → 各列的 (众数, 最小, 最大)
pub fn parse_samples(text: &str) -> Vec<(u64, u64, u64)> {
    let rows: Vec<Vec<u64>> = text
        .lines()
        .map(|l| {
            l.split_whitespace()
                .filter_map(|v| v.parse().ok())
                .collect::<Vec<u64>>()
        })
        .filter(|r| !r.is_empty())
        .collect();
    let Some(width) = rows.iter().map(|r| r.len()).min() else {
        return vec![];
    };
    (0..width)
        .map(|c| {
            let col: Vec<u64> = rows.iter().map(|r| r[c]).collect();
            let mut counts: std::collections::HashMap<u64, usize> = Default::default();
            for v in &col {
                *counts.entry(*v).or_default() += 1;
            }
            let mode = counts
                .iter()
                .max_by_key(|(v, n)| (**n, **v))
                .map(|(v, _)| *v)
                .unwrap_or(0);
            (mode, *col.iter().min().unwrap(), *col.iter().max().unwrap())
        })
        .collect()
}

// ══════════════ 功耗遥测 ══════════════

/// 功率采样口径。两条轨各有各的适用条件, 不可混用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerRail {
    /// USB 输入轨 (`power_supply/usb`): 只读, 不动任何充电状态。
    ///
    /// 量的是**墙上抽走的功率**, 含充电与转换损耗。电池充满且停充时它最接近整机功耗,
    /// 正在充电时会把充电功率一起算进去, 此时不可用于对比。
    Usb,
    /// 电池轨 (`power_supply/battery/power_now`): 量的是设备从电池真实抽走的功率。
    ///
    /// 只有设备**处于放电态**时才有非零读数; 插着 USB 充电时恒为 0。
    Battery,
}

impl PowerRail {
    pub fn as_str(&self) -> &'static str {
        match self {
            PowerRail::Usb => "usb",
            PowerRail::Battery => "battery",
        }
    }
}

/// 一次功率采样。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerSample {
    pub volt_uv: i64,
    pub curr_ua: i64,
    /// 设备直接给出的瞬时功率 (微瓦); 没有该节点时为 None
    pub power_uw: Option<i64>,
}

impl PowerSample {
    /// 瓦特。优先用设备直报的 power_now, 否则 V x I 折算。
    ///
    /// 取绝对值: 放电电流在不同内核里有正有负 (有的用负数表示放电),
    /// 功率的**大小**才是我们要的量。
    pub fn watt(&self) -> f64 {
        if let Some(uw) = self.power_uw {
            if uw != 0 {
                return (uw.abs() as f64) / 1.0e6;
            }
        }
        (self.volt_uv as f64 / 1.0e6) * (self.curr_ua as f64 / 1.0e6).abs()
    }
}

/// 采样命令: 一次 su 往返读一条轨的电压/电流/功率。
pub fn power_sample_cmd(rail: PowerRail) -> String {
    let p = format!("/sys/class/power_supply/{}", rail.as_str());
    format!(
        "su -c 'echo PWR $(cat {p}/voltage_now 2>/dev/null || echo 0) \
$(cat {p}/current_now 2>/dev/null || echo 0) \
$(cat {p}/power_now 2>/dev/null || echo NA)'"
    )
}

/// 解析 `PWR <voltage_uv> <current_ua> <power_uw|NA>` 行。
pub fn parse_power_samples(text: &str) -> Vec<PowerSample> {
    text.lines()
        .filter_map(|l| {
            let c: Vec<&str> = l.split_whitespace().collect();
            if c.first().copied() != Some("PWR") || c.len() < 3 {
                return None;
            }
            Some(PowerSample {
                volt_uv: c[1].parse().ok()?,
                curr_ua: c[2].parse().ok()?,
                power_uw: c.get(3).and_then(|v| v.parse::<i64>().ok()),
            })
        })
        .collect()
}

/// 一组采样的功率统计 (瓦)。返回 (均值, 中位数, 最小, 最大)。
pub fn power_stats(samples: &[PowerSample]) -> Option<(f64, f64, f64, f64)> {
    if samples.is_empty() {
        return None;
    }
    let mut w: Vec<f64> = samples.iter().map(|s| s.watt()).collect();
    let mean = w.iter().sum::<f64>() / w.len() as f64;
    w.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = w.len();
    let median = if n % 2 == 1 {
        w[n / 2]
    } else {
        (w[n / 2 - 1] + w[n / 2]) / 2.0
    };
    Some((mean, median, w[0], w[n - 1]))
}

/// 功率读数可不可信。
///
/// 这层判断是必需的: 插着 USB 充电时电池轨恒为 0, 一个 0.000 W 的"实测功耗"
/// 看起来像个数字, 其实是「这条轨此刻量不了」。不做这层, 报告就会拿 0 当结论。
pub fn power_usable(rail: PowerRail, samples: &[PowerSample]) -> Result<(), String> {
    let Some((mean, _, _, _)) = power_stats(samples) else {
        return Err("没有取到任何功率采样".into());
    };
    if mean <= 0.0 {
        return Err(match rail {
            PowerRail::Battery => "电池轨功率恒为 0: 设备正插着 USB 充电。\
                 要量整机功耗需让设备处于放电态 (拔掉充电或停充), 或改用 --power-rail usb"
                .into(),
            PowerRail::Usb => "USB 轨功率恒为 0: 设备未通过 USB 供电 (可能走 Wi-Fi adb)。\
                 改用 --power-rail battery"
                .into(),
        });
    }
    Ok(())
}

// ══════════════ 设备操作 ══════════════

pub fn root_ok(phone: &crate::device::Device) -> bool {
    phone.shell("su -c id", 8000).contains("uid=0(")
}

pub fn read_snapshot(phone: &crate::device::Device) -> Snapshot {
    parse_snapshot(&phone.shell(&snapshot_cmd(), 15000))
}

pub fn read_thermal(phone: &crate::device::Device) -> Vec<(String, i64)> {
    parse_thermal(&phone.shell(
        "for z in /sys/class/thermal/thermal_zone*; do echo \"$(cat $z/type) $(cat $z/temp)\"; done 2>/dev/null",
        15000,
    ))
}

pub fn read_power(phone: &crate::device::Device, rail: PowerRail) -> Option<PowerSample> {
    parse_power_samples(&phone.shell(&power_sample_cmd(rail), 8000))
        .into_iter()
        .next()
}

/// 冷机门禁: SoC 结温最高热区 < cool_c 才放行; 超时即报错(安全阀, 不是时间预设)
pub fn wait_cool(
    tag: &str,
    phone: &crate::device::Device,
    cool_c: f64,
    timeout_s: u64,
) -> Result<(String, f64, u64), String> {
    let t0 = Instant::now();
    loop {
        let (zone, c) =
            soc_max_c(&read_thermal(phone)).ok_or("读不到任何 cpu-/gpuss 热区, 无法做冷机判定")?;
        if c < cool_c {
            return Ok((zone, c, t0.elapsed().as_secs()));
        }
        if t0.elapsed() > Duration::from_secs(timeout_s) {
            return Err(format!("等冷超时 {timeout_s}s: {zone}={c:.1}C 仍 >= {cool_c}C"));
        }
        progress(
            tag,
            &format!(
                "等冷: {zone}={c:.1}C >= {cool_c}C, 3s 后重测 (已等 {}s)",
                t0.elapsed().as_secs()
            ),
        );
        std::thread::sleep(Duration::from_secs(3));
    }
}

pub struct Lock {
    before: Snapshot,
    state_path: std::path::PathBuf,
    applied: bool,
}

/// 快照 → 状态文件文本 (与 snapshot_cmd 输出同形, parse_snapshot 可直接回读)
pub fn snapshot_text(s: &Snapshot) -> String {
    let mut t = String::new();
    for p in &s.cpu {
        t.push_str(&format!(
            "CPU {} {} {} {} {} {}\n",
            p.path, p.governor, p.min_khz, p.max_khz, p.cur_khz, p.cpus
        ));
    }
    if let (Some(mx), Some(mn)) = (s.gpu_max_level, s.gpu_min_level) {
        t.push_str(&format!(
            "GPU {} {} {} {} {}\n",
            mx,
            mn,
            s.gpu_thermal_level.unwrap_or(0),
            s.gpuclk_hz.unwrap_or(0),
            s.gpu_model.clone().unwrap_or_default()
        ));
    }
    if !s.gpu_freqs.is_empty() {
        t.push_str(&format!(
            "GPUFREQS {}\n",
            s.gpu_freqs
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    for b in &s.bus {
        t.push_str(&format!(
            "BUS {} {} {} {}\n",
            b.name, b.floor_khz, b.max_khz, b.cur_khz
        ));
    }
    t
}

/// 把快照里的调速器与 kgsl 档位写回设备; 返回回读是否一致
pub fn restore_snapshot(phone: &crate::device::Device, before: &Snapshot) -> bool {
    let mut cmd = String::from("su -c '");
    for p in &before.cpu {
        cmd.push_str(&format!("echo {} > {}/scaling_governor; ", p.governor, p.path));
    }
    // 恢复顺序: 先 min(数值大) 再 max, 与 kgsl 钳位规则一致
    if let (Some(mx), Some(mn)) = (before.gpu_max_level, before.gpu_min_level) {
        cmd.push_str(&format!(
            "K={KGSL}; echo {mn} > $K/min_pwrlevel; echo {mx} > $K/max_pwrlevel; "
        ));
    }
    for b in &before.bus {
        cmd.push_str(&format!(
            "echo {} > {BUS_DCVS}/{}/boost_freq; ",
            b.floor_khz, b.name
        ));
    }
    cmd.push_str("echo RESTORED'");
    let out = phone.shell(&cmd, 15000);
    if !out.contains("RESTORED") {
        return false;
    }
    let after = read_snapshot(phone);
    let cpu_ok = after.cpu.len() == before.cpu.len()
        && after
            .cpu
            .iter()
            .zip(&before.cpu)
            .all(|(a, b)| a.governor == b.governor);
    let bus_ok = after.bus.len() == before.bus.len()
        && after
            .bus
            .iter()
            .zip(&before.bus)
            .all(|(a, b)| a.floor_khz == b.floor_khz);
    cpu_ok
        && bus_ok
        && after.gpu_max_level == before.gpu_max_level
        && after.gpu_min_level == before.gpu_min_level
}

/// 上次异常退出遗留的锁态: 状态文件在就按它回滚。返回 Some(是否回滚成功); 无遗留 None
pub fn recover_stale_lock(
    tag: &str,
    phone: &crate::device::Device,
    state_path: &std::path::Path,
) -> Option<bool> {
    let text = std::fs::read_to_string(state_path).ok()?;
    let before = parse_snapshot(&text);
    if before.cpu.is_empty() && before.gpu_max_level.is_none() {
        let _ = std::fs::remove_file(state_path);
        return None;
    }
    progress(
        tag,
        &format!("发现上次遗留的锁频状态 {}, 先回滚", state_path.display()),
    );
    let ok = restore_snapshot(phone, &before);
    if ok {
        let _ = std::fs::remove_file(state_path);
    }
    Some(ok)
}

impl Lock {
    pub fn apply(
        phone: &crate::device::Device,
        state_path: &std::path::Path,
        before: Snapshot,
        level: u32,
    ) -> Result<Lock, String> {
        // 先落状态文件再动设备: 中途被杀也能回滚
        std::fs::write(state_path, snapshot_text(&before))
            .map_err(|e| format!("写不了锁频状态文件 {}: {e}", state_path.display()))?;
        let mut cmd = String::from("su -c '");
        for p in &before.cpu {
            cmd.push_str(&format!("echo performance > {}/scaling_governor; ", p.path));
        }
        // 先 max 再 min 再 max: kgsl 的 store 互相钳位 (max<=min, min>=max), 这个顺序对任意目标档都成立
        cmd.push_str(&format!(
            "K={KGSL}; echo {level} > $K/max_pwrlevel; echo {level} > $K/min_pwrlevel; echo {level} > $K/max_pwrlevel; "
        ));
        // 总线钉在 hw_max_freq: 访存算子的时延不再随 DDR/LLCC DCVS 漂移
        for b in &before.bus {
            cmd.push_str(&format!(
                "echo {} > {BUS_DCVS}/{}/boost_freq; ",
                b.max_khz, b.name
            ));
        }
        cmd.push_str("echo LOCKED'");
        let out = phone.shell(&cmd, 15000);
        let lock = Lock {
            before,
            state_path: state_path.to_path_buf(),
            applied: true,
        };
        if !out.contains("LOCKED") {
            lock.restore(phone);
            return Err(format!("锁频命令未执行完: {}; 已回滚", out.trim()));
        }
        let after = read_snapshot(phone);
        let cpu_ok = !after.cpu.is_empty() && after.cpu.iter().all(|p| p.governor == "performance");
        let gpu_ok = after.gpu_max_level == Some(level) && after.gpu_min_level == Some(level);
        let bus_ok = after.bus.len() == lock.before.bus.len()
            && after.bus.iter().all(|b| b.floor_khz == b.max_khz);
        if !cpu_ok || !gpu_ok || !bus_ok {
            lock.restore(phone);
            return Err(format!(
                "锁频回读不符: cpu_governor={:?} gpu(max,min)=({:?},{:?}) 目标档 {level} bus={:?}; 已回滚",
                after.cpu.iter().map(|p| p.governor.as_str()).collect::<Vec<_>>(),
                after.gpu_max_level,
                after.gpu_min_level,
                after.bus.iter().map(|b| format!("{}:{}/{}", b.name, b.floor_khz, b.max_khz)).collect::<Vec<_>>()
            ));
        }
        Ok(lock)
    }

    /// 恢复原值并清掉状态文件; 返回回读是否与锁前一致
    pub fn restore(&self, phone: &crate::device::Device) -> bool {
        if !self.applied {
            return true;
        }
        let ok = restore_snapshot(phone, &self.before);
        if ok {
            let _ = std::fs::remove_file(&self.state_path);
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 快照与热区 (口径由 bench 迁入, 行为必须逐字不变) ----------

    #[test]
    fn snapshot_thermal_and_levels() {
        let snap = parse_snapshot(
            "CPU /sys/devices/system/cpu/cpufreq/policy0 schedutil 300000 2400000 1200000 0,1,2\n\
             GPU 3 17 3 902000000 Adreno840\n\
             GPUFREQS 1200000000 1100000000 1000000000 902000000\n\
             BUS DDR 547000 5333000 547000\n\
             BUS LLCC 300000 1400000 300000\n",
        );
        assert_eq!(snap.cpu.len(), 1);
        assert_eq!(snap.cpu[0].governor, "schedutil");
        assert_eq!(snap.gpu_max_level, Some(3));
        assert_eq!(snap.gpu_min_level, Some(17));
        assert_eq!(snap.gpu_model.as_deref(), Some("Adreno840"));
        assert_eq!(snap.bus.len(), 2);
        assert_eq!(snap.bus[0].max_khz, 5333000);

        // 快照往返: 文本 → 结构 → 文本 → 结构, 必须等价
        let back = parse_snapshot(&snapshot_text(&snap));
        assert_eq!(back.cpu, snap.cpu);
        assert_eq!(back.bus, snap.bus);
        assert_eq!(back.gpu_max_level, snap.gpu_max_level);

        // 热区筛选: 只认 SoC 结温前缀, 跳过 0 与 >=100C 哨兵
        let zones = parse_thermal(
            "cpu-0-0 45000\nskin-therm 38000\ngpuss-0 52000\ncpu-1-0 0\nbattery 99000\nsoc-dummy 150000\n",
        );
        let (zone, c) = soc_max_c(&zones).unwrap();
        assert_eq!(zone, "gpuss-0");
        assert!((c - 52.0).abs() < 1e-9);

        assert_eq!(nearest_level(&snap.gpu_freqs, 902), Some(3));
        assert_eq!(nearest_level(&snap.gpu_freqs, 1190), Some(0));
    }

    #[test]
    fn dispersion_and_samples() {
        let (median, min, max, disp) = dispersion_pct(&[100.0, 104.0, 102.0]);
        assert!((median - 102.0).abs() < 1e-9);
        assert!((min - 100.0).abs() < 1e-9);
        assert!((max - 104.0).abs() < 1e-9);
        assert!((disp - (4.0 / 102.0 * 100.0)).abs() < 1e-9);
        assert_eq!(dispersion_pct(&[]), (0.0, 0.0, 0.0, 0.0));

        let s = parse_samples("900 111 222\n900 111 333\n800 111 222\n");
        assert_eq!(s[0], (900, 800, 900));
        assert_eq!(s[1], (111, 111, 111));
    }

    #[test]
    fn snapshot_cmd_is_single_quoted_root_payload() {
        let c = snapshot_cmd();
        assert!(c.starts_with("su -c '") && c.ends_with('\''));
        assert!(c.contains("max_pwrlevel") && c.contains("gpu_available_frequencies"));
        assert!(c.contains("DDR") && c.contains("LLCC"));
    }

    #[test]
    fn lock_state_path_separates_paths_and_serials() {
        let a = lock_state_path("bench", &Some("R5CT30ABCDE".into()));
        let b = lock_state_path("gpuop", &Some("R5CT30ABCDE".into()));
        let c = lock_state_path("bench", &Some("other".into()));
        assert_ne!(a, b, "不同通路的锁态文件不能互相覆盖");
        assert_ne!(a, c, "不同设备的锁态文件不能互相覆盖");
        // 序列号里的非字母数字必须被规整掉, 免得拼出非法路径
        let d = lock_state_path("bench", &Some("127.0.0.1:5555".into()));
        assert!(d.to_string_lossy().contains("127_0_0_1_5555"));
    }

    // ---------- 功耗遥测 ----------

    #[test]
    fn power_prefers_the_device_reported_wattage() {
        let s = PowerSample {
            volt_uv: 4_400_000,
            curr_ua: 1_000_000,
            power_uw: Some(3_900_000),
        };
        // 有 power_now 就用它, 不去乘 V x I
        assert!((s.watt() - 3.9).abs() < 1e-9);
    }

    #[test]
    fn power_falls_back_to_volts_times_amps() {
        let s = PowerSample {
            volt_uv: 5_127_000,
            curr_ua: 144_000,
            power_uw: None,
        };
        assert!((s.watt() - 5.127 * 0.144).abs() < 1e-6);
    }

    /// 放电电流在不同内核上有正有负, 功率取大小。
    #[test]
    fn power_is_sign_agnostic() {
        let pos = PowerSample { volt_uv: 4_000_000, curr_ua: 500_000, power_uw: None };
        let neg = PowerSample { volt_uv: 4_000_000, curr_ua: -500_000, power_uw: None };
        assert!((pos.watt() - neg.watt()).abs() < 1e-12);
        assert!((pos.watt() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn parses_power_lines_and_tolerates_missing_power_now() {
        let v = parse_power_samples("PWR 5127000 144000 NA\nPWR 4411000 0 0\nnoise\nPWR 4400000 -900000 3960000\n");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].power_uw, None);
        assert_eq!(v[1].power_uw, Some(0));
        assert_eq!(v[2].power_uw, Some(3_960_000));
    }

    #[test]
    fn power_stats_reports_mean_median_and_range() {
        let s = vec![
            PowerSample { volt_uv: 1_000_000, curr_ua: 1_000_000, power_uw: None }, // 1 W
            PowerSample { volt_uv: 1_000_000, curr_ua: 3_000_000, power_uw: None }, // 3 W
            PowerSample { volt_uv: 1_000_000, curr_ua: 2_000_000, power_uw: None }, // 2 W
        ];
        let (mean, median, min, max) = power_stats(&s).unwrap();
        assert!((mean - 2.0).abs() < 1e-9);
        assert!((median - 2.0).abs() < 1e-9);
        assert!((min - 1.0).abs() < 1e-9);
        assert!((max - 3.0).abs() < 1e-9);
        assert!(power_stats(&[]).is_none());
    }

    /// 这条是防「拿 0 当结论」的闸: 充电中电池轨恒为 0,
    /// 那不是「功耗为零」, 而是「这条轨此刻量不了」。
    #[test]
    fn an_all_zero_rail_is_reported_as_unmeasurable_not_as_zero_watts() {
        let charging = vec![
            PowerSample { volt_uv: 4_411_000, curr_ua: 0, power_uw: Some(0) },
            PowerSample { volt_uv: 4_411_000, curr_ua: 0, power_uw: Some(0) },
        ];
        let err = power_usable(PowerRail::Battery, &charging).unwrap_err();
        assert!(err.contains("充电"), "{err}");

        let usb_off = vec![PowerSample { volt_uv: 0, curr_ua: 0, power_uw: None }];
        assert!(power_usable(PowerRail::Usb, &usb_off).is_err());

        let real = vec![PowerSample { volt_uv: 5_127_000, curr_ua: 144_000, power_uw: None }];
        assert!(power_usable(PowerRail::Usb, &real).is_ok());
        assert!(power_usable(PowerRail::Battery, &[]).is_err());
    }

    #[test]
    fn power_sample_cmd_targets_the_right_rail() {
        assert!(power_sample_cmd(PowerRail::Usb).contains("power_supply/usb/voltage_now"));
        assert!(power_sample_cmd(PowerRail::Battery).contains("power_supply/battery/power_now"));
        assert!(power_sample_cmd(PowerRail::Usb).starts_with("su -c '"));
    }
}
