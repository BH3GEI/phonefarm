//! gpu-op: Compute Shader 算子的真机标尺与 A/B 裁决。
//!
//! 职责边界: 收 eval_request → 本地预检 SPIR-V → 等冷 → 锁频 → A/B/A/B 交替跑测
//! → 采功耗与时延 → Welch t 检验 → 出 eval_report。
//!
//! 与 `bench` 的关系: 两者是并列的标尺, 互不依赖。`bench` 量 TFLite 模型,
//! 本模块量 Vulkan Compute Shader。共用的设备条件化 (等冷/锁频/还原) 与功耗遥测
//! 提在 `hwcond`, 统计提在 `gpustat`。
//!
//! 契约: `game_opt_loop/contracts/eval_request.schema.json` 与 `eval_report.schema.json`。
//! 上游 (game_opt_loop) 负责变异与防爆门禁, 本通路只负责物理采样与统计裁决,
//! **不做任何算法判断** —— 快慢好坏由数字说话。
//!
//! 纪律:
//!   - 不侵入任何游戏进程, 不在设备上留常驻服务或文件残留;
//!   - 所有 sysfs 写入退出前恢复, 无论中途成功失败;
//!   - 判定规则由 eval_request 在看到数据之前带入并冻结, 事后不得调整;
//!   - 量不到的指标如实留空, 绝不用默认值填补。

use crate::gpustat::{self, WelchResult};
use crate::hwcond::{self, PowerRail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 设备侧产物目录。跑完即删, 不留残留。
const REMOTE_DIR: &str = "/data/local/tmp/phonefarm_gpuop";
/// 设备侧 Vulkan 算子 runner 的默认本地路径。
const DEFAULT_RUNNER: &str = "tools/vkop/android_aarch64_vkop_runner";

fn progress(msg: &str) {
    hwcond::progress("gpu-op", msg);
}

// ══════════════ 契约类型 (与 game_opt_loop/contracts 对齐) ══════════════

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct EvalProtocol {
    pub cool_c: f32,
    pub replay_seconds: u32,
    pub rounds: u32,
    pub alternating: bool,
    pub p_threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRequest {
    pub version: u32,
    pub candidate_id: String,
    pub track: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    pub package: String,
    pub replay_script: String,
    pub shader_path: String,
    pub spirv_path: String,
    pub budget_ms: f32,
    pub quality_baseline_db: f32,
    pub protocol: EvalProtocol,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Metrics {
    pub operator_latency_ms: f32,
    /// `None` = 本次没有量到, 不是 0。
    ///
    /// headless 算子标尺量的是算子自身的 GPU 耗时, 帧时 p95 需要真实渲染上下文。
    /// 2026-09-22 真机回的第一份报告这一项就是 null, 而两边当时都声明成 f32,
    /// 上游直接解析失败 —— 契约必须容得下「未测量」这个状态。
    pub fps_p95_ms: Option<f32>,
    pub power_watt: f32,
    pub psnr_db: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Verdict {
    pub is_pareto_improvement: bool,
    pub p_value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Pass,
    Slow,
    PoorQuality,
    Crash,
    Reverted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub candidate_id: String,
    pub status: Status,
    pub metrics: Metrics,
    pub verdict: Verdict,
    /// 恒为 "phonefarm": 本通路只产出真机实测数据。
    pub source: String,
}

/// 校验 eval_request 的必填项与取值范围。
///
/// 上游契约写死了防噪声协议 (冷机 <32C / 60s 回放 / A/B 交替 / p<0.01),
/// 这里只检查它**自洽且可执行**, 不替上游改判据。
pub fn validate_request(r: &EvalRequest) -> Result<(), String> {
    if r.version != 1 {
        return Err(format!("不认识的 eval_request 版本 {}", r.version));
    }
    if r.candidate_id.trim().is_empty() {
        return Err("candidate_id 为空".into());
    }
    if !matches!(r.track.as_str(), "sr" | "frame_gen") {
        return Err(format!("未知赛道 {}", r.track));
    }
    if !(r.budget_ms > 0.0) {
        return Err(format!("budget_ms 必须为正, 收到 {}", r.budget_ms));
    }
    let p = &r.protocol;
    if p.rounds < 2 {
        return Err(format!(
            "protocol.rounds = {} 不足以做两样本检验, 每臂至少要 2 轮",
            p.rounds
        ));
    }
    if !(p.p_threshold > 0.0 && p.p_threshold < 1.0) {
        return Err(format!("p_threshold 必须落在 (0,1), 收到 {}", p.p_threshold));
    }
    if !(p.cool_c > 0.0 && p.cool_c < 100.0) {
        return Err(format!("cool_c 不合理: {}", p.cool_c));
    }
    if p.replay_seconds == 0 {
        return Err("replay_seconds 为 0".into());
    }
    Ok(())
}

// ══════════════ SPIR-V 本地预检 ══════════════

#[derive(Debug, Clone, PartialEq)]
pub struct SpirvInfo {
    pub version_major: u8,
    pub version_minor: u8,
    pub generator: u32,
    pub id_bound: u32,
    pub entry_points: Vec<String>,
    /// OpExecutionMode LocalSize 声明的工作组尺寸
    pub local_size: Option<[u32; 3]>,
    pub bytes: usize,
}

const SPIRV_MAGIC: u32 = 0x0723_0203;
const OP_ENTRY_POINT: u16 = 15;
const OP_EXECUTION_MODE: u16 = 16;
const EXEC_MODEL_GLCOMPUTE: u32 = 5;
const EXEC_MODE_LOCAL_SIZE: u32 = 17;

/// 解析并校验 SPIR-V 字节码。
///
/// 上游 game_opt_loop 已经用 naga 做过防爆门禁, 但**本通路不能假设上游一定跑过**:
/// 一份坏字节码推上真机, 轻则驱动报错, 重则 GPU 挂起要重启设备。
/// 这里只做结构性校验 (魔数/版本/入口点/工作组), 不重复上游的语义分析。
pub fn parse_spirv(bytes: &[u8]) -> Result<SpirvInfo, String> {
    if bytes.len() < 20 {
        return Err(format!("SPIR-V 太短 ({} 字节), 连文件头都不够", bytes.len()));
    }
    if bytes.len() % 4 != 0 {
        return Err(format!("SPIR-V 长度 {} 不是 4 的倍数", bytes.len()));
    }

    let le = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let be = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let little = if le == SPIRV_MAGIC {
        true
    } else if be == SPIRV_MAGIC {
        false
    } else {
        return Err(format!(
            "SPIR-V 魔数不对: 期望 0x{SPIRV_MAGIC:08X}, 实际 0x{le:08X}"
        ));
    };

    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| {
            let a = [c[0], c[1], c[2], c[3]];
            if little {
                u32::from_le_bytes(a)
            } else {
                u32::from_be_bytes(a)
            }
        })
        .collect();

    let version = words[1];
    let version_major = ((version >> 16) & 0xFF) as u8;
    let version_minor = ((version >> 8) & 0xFF) as u8;
    if version_major != 1 {
        return Err(format!("不支持的 SPIR-V 主版本 {version_major}"));
    }

    let mut info = SpirvInfo {
        version_major,
        version_minor,
        generator: words[2],
        id_bound: words[3],
        entry_points: Vec::new(),
        local_size: None,
        bytes: bytes.len(),
    };

    // 指令流从第 5 个 word 开始
    let mut i = 5usize;
    while i < words.len() {
        let word_count = (words[i] >> 16) as usize;
        let opcode = (words[i] & 0xFFFF) as u16;
        if word_count == 0 {
            return Err(format!("SPIR-V 指令流在 word {i} 处 wordCount=0, 已损坏"));
        }
        if i + word_count > words.len() {
            return Err(format!(
                "SPIR-V 指令流在 word {i} 处越界 (声明 {word_count} words, 只剩 {})",
                words.len() - i
            ));
        }

        match opcode {
            OP_ENTRY_POINT if word_count >= 4 => {
                if words[i + 1] == EXEC_MODEL_GLCOMPUTE {
                    // 入口名是从第 4 个 word 起的 UTF-8 字面量, 以 NUL 结尾
                    let name_words = &words[i + 3..i + word_count];
                    let mut raw = Vec::with_capacity(name_words.len() * 4);
                    for w in name_words {
                        raw.extend_from_slice(&w.to_le_bytes());
                    }
                    let name = raw
                        .split(|b| *b == 0)
                        .next()
                        .map(|s| String::from_utf8_lossy(s).into_owned())
                        .unwrap_or_default();
                    info.entry_points.push(name);
                }
            }
            OP_EXECUTION_MODE if word_count >= 6 => {
                if words[i + 2] == EXEC_MODE_LOCAL_SIZE {
                    info.local_size = Some([words[i + 3], words[i + 4], words[i + 5]]);
                }
            }
            _ => {}
        }
        i += word_count;
    }

    if info.entry_points.is_empty() {
        return Err("SPIR-V 里没有 GLCompute 入口点, 这不是一个 compute shader".into());
    }
    if info.entry_points.len() > 1 {
        return Err(format!(
            "SPIR-V 有 {} 个 compute 入口点 {:?}, 算子插槽只挂载单入口",
            info.entry_points.len(),
            info.entry_points
        ));
    }

    // 工作组总调用数上限: Vulkan 规范下限 128, Adreno/Mali 实际 1024。
    if let Some([x, y, z]) = info.local_size {
        let total = x.saturating_mul(y).saturating_mul(z);
        if total == 0 {
            return Err(format!("工作组尺寸含 0 维: {:?}", info.local_size));
        }
        if total > 1024 {
            return Err(format!(
                "工作组 {:?} = {total} 个调用, 超过硬件上限 1024",
                info.local_size
            ));
        }
    }

    Ok(info)
}

// ══════════════ A/B/A/B 调度 ══════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// A: 基线算子 (当前线上/上一代优胜)
    Baseline,
    /// B: 待测候选算子
    Candidate,
}

impl Arm {
    pub fn as_str(&self) -> &'static str {
        match self {
            Arm::Baseline => "A",
            Arm::Candidate => "B",
        }
    }
}

/// 生成跑测顺序。
///
/// `rounds` 是**每臂**的轮数, 总跑测次数 = rounds * 2。
///
/// 交替 (A/B/A/B) 是防热漂移的关键: 设备跑久了会升温降频, 若先把 A 跑满再跑 B,
/// B 全程都在更热的机器上跑, 温度差会被整包算进算子差异里。交替之后两臂
/// 均匀分布在整段时间上, 热漂移对两臂的影响一阶抵消。
pub fn ab_schedule(rounds: u32, alternating: bool) -> Vec<Arm> {
    let mut out = Vec::with_capacity(rounds as usize * 2);
    if alternating {
        for _ in 0..rounds {
            out.push(Arm::Baseline);
            out.push(Arm::Candidate);
        }
    } else {
        for _ in 0..rounds {
            out.push(Arm::Baseline);
        }
        for _ in 0..rounds {
            out.push(Arm::Candidate);
        }
    }
    out
}

// ══════════════ 设备侧 runner 回包契约 ══════════════

/// 设备侧 Vulkan runner 的一次跑测结果。
///
/// runner 是一个推到 `/data/local/tmp` 的原生可执行文件 (与 `bench` 用
/// `benchmark_model` 同一套路), 它负责: 建 Vulkan 设备 → 分配常驻显存的输入/输出
/// image → 跑 N 次 dispatch, 用 VkQueryPool 时间戳量 GPU 侧耗时 → 回读一次算 PSNR
/// → 往 stdout 打一个 JSON 对象。
///
/// 画面全程不出显存: 这是上游 game_opt_loop 零拷贝红线在设备侧的落地点。
#[derive(Debug, Clone, PartialEq)]
pub struct RunnerReport {
    pub ok: bool,
    /// 每次 dispatch 的 GPU 侧耗时 (微秒)
    pub timing_us: Vec<f64>,
    pub psnr_db: Option<f64>,
    pub device_name: Option<String>,
    pub error: Option<String>,
}

impl RunnerReport {
    /// 中位耗时 (毫秒)。
    pub fn median_ms(&self) -> Option<f64> {
        if self.timing_us.is_empty() {
            return None;
        }
        Some(gpustat::percentile(&self.timing_us, 0.5) / 1000.0)
    }
}

/// 解析 runner 的 stdout。契约要求 `--json` 时 stdout 只有一个 JSON 对象,
/// 但实现难免混进进度行, 故从后往前找第一个能解析的对象。
pub fn parse_runner_output(stdout: &str) -> Result<RunnerReport, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err("runner 回包为空".into());
    }

    // 从**最前面**的 `{` 往后找第一个"长得像回包"的对象。
    // 反过来从尾部找是错的: 嵌套对象 (比如 timing_us 里那个) 自己也是合法 JSON,
    // 用无类型 Value 解析时会先命中它, 于是 ok/samples 全都读空,
    // 一次成功的跑测被读成"没有样本"。
    let looks_like_report =
        |v: &Value| v.get("ok").is_some() || v.get("timing_us").is_some();

    let mut parsed: Option<Value> = None;
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if looks_like_report(&v) {
            parsed = Some(v);
        }
    }
    if parsed.is_none() {
        let bytes = trimmed.as_bytes();
        for start in (0..bytes.len()).filter(|&i| bytes[i] == b'{') {
            let mut stream =
                serde_json::Deserializer::from_str(&trimmed[start..]).into_iter::<Value>();
            if let Some(Ok(v)) = stream.next() {
                if looks_like_report(&v) {
                    parsed = Some(v);
                    break;
                }
            }
        }
    }
    let v = parsed.ok_or_else(|| {
        format!(
            "runner 回包不是合法 JSON: {}",
            trimmed.chars().take(200).collect::<String>()
        )
    })?;

    let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let error = v
        .get("error")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    let timing_us: Vec<f64> = v
        .pointer("/timing_us/samples")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();

    if ok && timing_us.is_empty() {
        return Err("runner 报告成功却没有任何时延样本".into());
    }

    Ok(RunnerReport {
        ok,
        timing_us,
        psnr_db: v.get("psnr_db").and_then(Value::as_f64),
        device_name: v
            .pointer("/device/name")
            .and_then(Value::as_str)
            .map(|s| s.to_string()),
        error,
    })
}

// ══════════════ 冻结判定规则 ══════════════

/// 判定所需的全部输入。构造之后不可变 —— 判据在看到数据之前就定死了。
#[derive(Debug, Clone, Copy)]
pub struct FrozenRules {
    pub budget_ms: f32,
    pub quality_baseline_db: f32,
    pub p_threshold: f64,
}

impl FrozenRules {
    pub fn from_request(r: &EvalRequest) -> Self {
        Self {
            budget_ms: r.budget_ms,
            quality_baseline_db: r.quality_baseline_db,
            p_threshold: r.protocol.p_threshold,
        }
    }
}

/// 由实测数据与冻结规则给出结论。
///
/// 判定次序是有讲究的, 不能换:
///   1. **崩了** → CRASH。没有数据可谈。
///   2. **超预算** → SLOW。算子自己就超了单帧硬预算, 后面的都不用看。
///   3. **画质掉基线** → POOR_QUALITY。§红线: 画质是约束不是目标,
///      严禁用模糊换帧率。这一条排在显著性前面 —— 哪怕"显著地更快",
///      掉了画质也是不合格品。
///   4. **过** → PASS。
///
/// `is_pareto_improvement` 只在「统计显著 且 确实更快」时为真。
/// 不显著就是不显著: 快了 0.3% 但 p=0.4, 那是噪声, 不是改进。
pub fn decide(
    rules: &FrozenRules,
    candidate: &Metrics,
    baseline: Option<&Metrics>,
    welch: Option<&WelchResult>,
    crashed: bool,
) -> (Status, Verdict) {
    // p 值取不到时记 1.0 —— 「未检验」, 不伪造一个漂亮的小数字。
    let p_value = welch.map(|w| w.p_value).unwrap_or(1.0);

    if crashed {
        return (
            Status::Crash,
            Verdict {
                is_pareto_improvement: false,
                p_value,
            },
        );
    }

    if candidate.operator_latency_ms > rules.budget_ms {
        return (
            Status::Slow,
            Verdict {
                is_pareto_improvement: false,
                p_value,
            },
        );
    }

    if candidate.psnr_db < rules.quality_baseline_db {
        return (
            Status::PoorQuality,
            Verdict {
                is_pareto_improvement: false,
                p_value,
            },
        );
    }

    // 显著性只有在有基线对照时才谈得上。
    let significant = welch
        .map(|w| w.significant(rules.p_threshold))
        .unwrap_or(false);
    let faster = baseline
        .map(|b| candidate.operator_latency_ms < b.operator_latency_ms)
        .unwrap_or(false);

    (
        Status::Pass,
        Verdict {
            is_pareto_improvement: significant && faster,
            p_value,
        },
    )
}

// ══════════════ 参数 ══════════════

#[derive(Debug, Clone)]
pub struct GpuOpArgs {
    pub request_path: Option<String>,
    pub serial: Option<String>,
    pub as_json: bool,
    pub runner_bin: Option<String>,
    pub power_rail: PowerRail,
    pub out_dir: Option<String>,
    pub cool_timeout_s: u64,
    pub gpu_level: Option<u32>,
    pub unlock_only: bool,
}

const USAGE: &str = "用法: phonefarm gpu-op --request <eval_request.json> [--serial S] [--json]\n\
\x20                    [--runner <本地 runner 路径>] [--power-rail usb|battery]\n\
\x20                    [--out 目录] [--cool-timeout-s N] [--gpu-level N]\n\
\x20      phonefarm gpu-op --serial S --unlock        回滚遗留的锁频态\n\
\n\
Compute Shader 算子的真机标尺: 等冷 → 锁频 → A/B/A/B 交替跑测 → Welch t 检验 → eval_report。\n\
契约见 game_opt_loop/contracts/{eval_request,eval_report}.schema.json。";

pub fn parse_args(args: &[String]) -> Result<GpuOpArgs, String> {
    let mut a = GpuOpArgs {
        request_path: None,
        serial: None,
        as_json: false,
        runner_bin: None,
        power_rail: PowerRail::Usb,
        out_dir: None,
        cool_timeout_s: 600,
        gpu_level: None,
        unlock_only: false,
    };
    let mut it = args.iter();
    while let Some(k) = it.next() {
        match k.as_str() {
            "--request" => a.request_path = it.next().cloned(),
            "--serial" => a.serial = it.next().cloned(),
            "--json" => a.as_json = true,
            "--runner" => a.runner_bin = it.next().cloned(),
            "--out" => a.out_dir = it.next().cloned(),
            "--unlock" => a.unlock_only = true,
            "--cool-timeout-s" => {
                a.cool_timeout_s = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--cool-timeout-s 需要一个正整数")?
            }
            "--gpu-level" => {
                a.gpu_level = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--gpu-level 需要一个档位索引")?,
                )
            }
            "--power-rail" => {
                a.power_rail = match it.next().map(|s| s.as_str()) {
                    Some("usb") => PowerRail::Usb,
                    Some("battery") => PowerRail::Battery,
                    other => return Err(format!("--power-rail 只能是 usb 或 battery, 收到 {other:?}")),
                }
            }
            other => return Err(format!("不认识的参数 {other}\n\n{USAGE}")),
        }
    }
    if !a.unlock_only && a.request_path.is_none() {
        return Err(format!("缺少 --request\n\n{USAGE}"));
    }
    Ok(a)
}

/// 找设备侧 runner 二进制。
fn locate_runner(a: &GpuOpArgs) -> Result<String, String> {
    let cands: Vec<String> = a
        .runner_bin
        .iter()
        .cloned()
        .chain(std::env::var("PF_VKOP_RUNNER").ok())
        .chain(std::iter::once(DEFAULT_RUNNER.to_string()))
        .collect();
    for c in &cands {
        if std::fs::metadata(c).map(|m| m.is_file()).unwrap_or(false) {
            return Ok(c.clone());
        }
    }
    Err(format!(
        "找不到设备侧 Vulkan 算子 runner (试过 {cands:?})。\n\
         该二进制负责在真机上建 Vulkan 设备、分配常驻显存的输入/输出 image、\n\
         用 VkQueryPool 时间戳量 dispatch 的 GPU 侧耗时并算 PSNR。\n\
         用 NDK (aarch64-linux-androidNN-clang++) 编出 arm64-v8a 二进制后, \n\
         放到 tools/vkop/ 或用 --runner / PF_VKOP_RUNNER 指路。\n\
         回包契约见 src/gpuop.rs 的 RunnerReport 与 docs/SPEC_GPU_OP.md §6。"
    ))
}

// ══════════════ 主流程 ══════════════

pub fn run_gpu_op(args: &[String]) -> i32 {
    let a = match parse_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };

    let out_dir = a
        .out_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("phonefarm_gpuop").to_string_lossy().into_owned());
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("建不了输出目录 {out_dir}: {e}");
        return 2;
    }
    let phone = crate::device::Device::new(a.serial.clone(), out_dir.clone());

    let state_path = hwcond::lock_state_path("gpuop", &a.serial);

    if a.unlock_only {
        return match hwcond::recover_stale_lock("gpu-op", &phone, &state_path) {
            Some(true) => {
                progress("遗留锁频态已回滚");
                0
            }
            Some(false) => {
                eprintln!("回滚失败, 设备可能仍处于锁频态");
                2
            }
            None => {
                progress("没有遗留的锁频态");
                0
            }
        };
    }

    // ---- 读契约 ----
    let path = a.request_path.clone().unwrap();
    let raw = match std::fs::read_to_string(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("读不了 {path}: {e}");
            return 2;
        }
    };
    let req: EvalRequest = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{path} 不符合 eval_request 契约: {e}");
            return 2;
        }
    };
    if let Err(e) = validate_request(&req) {
        eprintln!("eval_request 校验失败: {e}");
        return 2;
    }
    progress(&format!(
        "候选 {} | 赛道 {} | 预算 {:.2} ms | 画质基线 {:.3} dB | 每臂 {} 轮 | p<{}",
        req.candidate_id,
        req.track,
        req.budget_ms,
        req.quality_baseline_db,
        req.protocol.rounds,
        req.protocol.p_threshold
    ));

    // ---- 本地预检 SPIR-V: 坏字节码绝不推上真机 ----
    let spv = match std::fs::read(&req.spirv_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("读不了 SPIR-V {}: {e}", req.spirv_path);
            return 2;
        }
    };
    let info = match parse_spirv(&spv) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("SPIR-V 预检未通过, 拒绝下发真机: {e}");
            return 2;
        }
    };
    progress(&format!(
        "SPIR-V 预检通过: v{}.{} | {} 字节 | 入口 {:?} | 工作组 {:?}",
        info.version_major, info.version_minor, info.bytes, info.entry_points, info.local_size
    ));

    // ---- root 与 runner ----
    if !hwcond::root_ok(&phone) {
        eprintln!("设备无 root: 等冷/锁频/功耗遥测都需要 root");
        return 2;
    }
    let runner = match locate_runner(&a) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    progress(&format!("设备侧 runner: {runner}"));

    // ---- 功耗轨可用性先探一次: 量不了就当场说, 不要跑完 10 分钟再报 0 W ----
    let probe: Vec<_> = (0..3)
        .filter_map(|_| {
            let s = hwcond::read_power(&phone, a.power_rail);
            std::thread::sleep(std::time::Duration::from_millis(200));
            s
        })
        .collect();
    if let Err(e) = hwcond::power_usable(a.power_rail, &probe) {
        eprintln!("功耗采样不可用: {e}");
        return 2;
    }

    // ---- 部署: runner + 候选/基线 SPIR-V ----
    if let Err(e) = deploy(&phone, &runner, &req) {
        eprintln!("部署失败: {e}");
        return 2;
    }

    // ---- 回滚上次异常退出遗留的锁态 ----
    let recovered = hwcond::recover_stale_lock("gpu-op", &phone, &state_path);
    if recovered == Some(false) {
        eprintln!("上次遗留的锁频态回滚失败, 拒绝在未知频率状态下跑测");
        cleanup(&phone);
        return 2;
    }

    // ---- A/B/A/B 交替跑测 ----
    let schedule = ab_schedule(req.protocol.rounds, req.protocol.alternating);
    let mut lat_a: Vec<f64> = Vec::new();
    let mut lat_b: Vec<f64> = Vec::new();
    let mut pwr_a: Vec<f64> = Vec::new();
    let mut pwr_b: Vec<f64> = Vec::new();
    let mut psnr_b: Vec<f64> = Vec::new();
    let mut crashed = false;
    let mut fatal: Option<String> = None;

    for (i, arm) in schedule.iter().enumerate() {
        progress(&format!(
            "第 {}/{} 轮 [{}] 等冷 < {:.1}C ...",
            i + 1,
            schedule.len(),
            arm.as_str(),
            req.protocol.cool_c
        ));

        // 每轮都重新等冷: 上一轮把机器跑热了, 不等就把热漂移算进算子差异。
        if let Err(e) = hwcond::wait_cool(
            "gpu-op",
            &phone,
            req.protocol.cool_c as f64,
            a.cool_timeout_s,
        ) {
            fatal = Some(format!("等冷失败: {e}"));
            break;
        }

        // 锁频只包住跑测那几秒 (bench 的实测教训: 先锁再等冷永远等不下来)
        let snap = hwcond::read_snapshot(&phone);
        let level = a
            .gpu_level
            .or(snap.gpu_max_level)
            .unwrap_or(0);
        let lock = match hwcond::Lock::apply(&phone, &state_path, snap.clone(), level) {
            Ok(l) => l,
            Err(e) => {
                fatal = Some(format!("锁频失败: {e}"));
                break;
            }
        };

        let (out, power) = run_one(&phone, *arm, &req, a.power_rail);
        let restored = lock.restore(&phone);
        if !restored {
            fatal = Some("锁频态回读与锁前不一致, 设备状态未能 100% 还原".into());
            break;
        }

        match parse_runner_output(&out) {
            Ok(r) if r.ok => {
                let Some(ms) = r.median_ms() else {
                    fatal = Some("runner 未给出时延".into());
                    break;
                };
                let w = hwcond::power_stats(&power).map(|(mean, _, _, _)| mean);
                match arm {
                    Arm::Baseline => {
                        lat_a.push(ms);
                        if let Some(w) = w { pwr_a.push(w); }
                    }
                    Arm::Candidate => {
                        lat_b.push(ms);
                        if let Some(w) = w { pwr_b.push(w); }
                        if let Some(q) = r.psnr_db { psnr_b.push(q); }
                    }
                }
                progress(&format!(
                    "  [{}] {:.3} ms{}",
                    arm.as_str(),
                    ms,
                    w.map(|w| format!(" | {w:.2} W")).unwrap_or_default()
                ));
            }
            Ok(r) => {
                // 候选把 GPU 跑崩了是一个结论, 不是一个错误。
                progress(&format!("  [{}] runner 失败: {:?}", arm.as_str(), r.error));
                if *arm == Arm::Candidate {
                    crashed = true;
                } else {
                    fatal = Some(format!("基线臂都跑不起来: {:?}", r.error));
                    break;
                }
            }
            Err(e) => {
                fatal = Some(format!("runner 回包不可解析: {e}"));
                break;
            }
        }
    }

    cleanup(&phone);

    if let Some(e) = fatal {
        eprintln!("{e}");
        return 2;
    }

    // ---- 统计裁决 ----
    let welch = gpustat::welch_t_test(&lat_a, &lat_b);
    let rules = FrozenRules::from_request(&req);

    let cand_lat = if lat_b.is_empty() {
        f32::NAN
    } else {
        gpustat::mean(&lat_b) as f32
    };
    let base = if lat_a.is_empty() {
        None
    } else {
        Some(Metrics {
            operator_latency_ms: gpustat::mean(&lat_a) as f32,
            fps_p95_ms: None,
            power_watt: gpustat::mean(&pwr_a) as f32,
            psnr_db: f32::NAN,
        })
    };
    let candidate = Metrics {
        operator_latency_ms: cand_lat,
        // 帧时 p95 需要在真实渲染上下文里量, headless 算子标尺给不出来。
        // 留 None 而不是填 0 —— 量不到的指标不许用默认值填补。
        fps_p95_ms: None,
        power_watt: if pwr_b.is_empty() { f32::NAN } else { gpustat::mean(&pwr_b) as f32 },
        psnr_db: if psnr_b.is_empty() { f32::NAN } else { gpustat::mean(&psnr_b) as f32 },
    };

    let (status, verdict) = decide(&rules, &candidate, base.as_ref(), welch.as_ref(), crashed);

    let report = EvalReport {
        candidate_id: req.candidate_id.clone(),
        status,
        metrics: candidate,
        verdict,
        source: "phonefarm".into(),
    };

    if a.as_json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    } else {
        println!("{}", render_text(&report, base.as_ref(), welch.as_ref()));
    }

    match status {
        Status::Pass => 0,
        _ => 1,
    }
}

/// 把 runner 与两份 SPIR-V 推到设备。
fn deploy(
    phone: &crate::device::Device,
    runner_local: &str,
    req: &EvalRequest,
) -> Result<(), String> {
    phone.shell(&format!("mkdir -p {REMOTE_DIR}"), 10_000);
    let remote_runner = format!("{REMOTE_DIR}/vkop_runner");
    if !phone.push_file(runner_local, &remote_runner) {
        return Err(format!("推送 {runner_local} 到 {remote_runner} 失败"));
    }
    phone.shell(&format!("chmod 755 {remote_runner}"), 10_000);

    let remote_cand = format!("{REMOTE_DIR}/candidate.spv");
    if !phone.push_file(&req.spirv_path, &remote_cand) {
        return Err(format!("推送候选 SPIR-V 到 {remote_cand} 失败"));
    }
    // 基线取赛道模板编译产物; 上游把它与候选放在同一目录下。
    let baseline = std::path::Path::new(&req.spirv_path)
        .parent()
        .map(|d| d.join("baseline.spv"))
        .ok_or("推不出基线 SPIR-V 路径")?;
    if !baseline.is_file() {
        return Err(format!(
            "找不到基线 SPIR-V {} —— A/B 对照必须有 A 臂",
            baseline.display()
        ));
    }
    let remote_base = format!("{REMOTE_DIR}/baseline.spv");
    if !phone.push_file(&baseline.to_string_lossy(), &remote_base) {
        return Err(format!("推送基线 SPIR-V 到 {remote_base} 失败"));
    }
    Ok(())
}

/// 设备上不留任何残留。
fn cleanup(phone: &crate::device::Device) {
    phone.shell(&format!("rm -rf {REMOTE_DIR}"), 10_000);
}

/// 跑一个臂, 同时在设备侧采功耗。
fn run_one(
    phone: &crate::device::Device,
    arm: Arm,
    req: &EvalRequest,
    rail: PowerRail,
) -> (String, Vec<hwcond::PowerSample>) {
    let spv = match arm {
        Arm::Baseline => "baseline.spv",
        Arm::Candidate => "candidate.spv",
    };

    // 采样上限按回放时长定; adb 通道断开后远端循环也会自行到头, 不留常驻进程。
    let ticks = (req.protocol.replay_seconds as u64 * 4).max(4);
    let inner = hwcond::power_sample_cmd(rail)
        .trim_start_matches("su -c '")
        .trim_end_matches('\'')
        .to_string();
    let sampler_cmd =
        format!("su -c 'i=0; while [ $i -lt {ticks} ]; do {inner}; sleep 0.25; i=$((i+1)); done'");
    let sampler = phone.stream_shell(&sampler_cmd).ok();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let cmd = format!(
        "cd {REMOTE_DIR} && ./vkop_runner --shader {spv} --seconds {} --json 2>&1",
        req.protocol.replay_seconds
    );
    let out = phone.shell(&cmd, (req.protocol.replay_seconds as u64 + 60) * 1000);

    let mut power = Vec::new();
    if let Some(mut child) = sampler {
        let _ = child.kill();
        if let Ok(o) = child.wait_with_output() {
            power = hwcond::parse_power_samples(&String::from_utf8_lossy(&o.stdout));
        }
    }
    (out, power)
}

fn render_text(
    r: &EvalReport,
    base: Option<&Metrics>,
    welch: Option<&WelchResult>,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("候选 {}: {:?}\n", r.candidate_id, r.status));
    s.push_str(&format!(
        "  算子耗时 {:.3} ms",
        r.metrics.operator_latency_ms
    ));
    if let Some(b) = base {
        s.push_str(&format!(" (基线 {:.3} ms)", b.operator_latency_ms));
    }
    s.push('\n');
    if r.metrics.psnr_db.is_finite() {
        s.push_str(&format!("  画质 {:.3} dB\n", r.metrics.psnr_db));
    }
    if r.metrics.power_watt.is_finite() {
        s.push_str(&format!("  整机功耗 {:.3} W\n", r.metrics.power_watt));
    }
    if let Some(w) = welch {
        s.push_str(&format!(
            "  Welch t 检验: t={:.3} df={:.1} p={:.5} (n={}+{})\n",
            w.t, w.df, w.p_value, w.n_a, w.n_b
        ));
    }
    s.push_str(&format!(
        "  Pareto 改进: {}\n",
        if r.verdict.is_pareto_improvement { "是" } else { "否" }
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 契约 ----------

    fn a_request() -> EvalRequest {
        EvalRequest {
            version: 1,
            candidate_id: "gen2_llm1".into(),
            track: "sr".into(),
            serial: Some("91253241019A".into()),
            package: "com.example.target".into(),
            replay_script: "sr_replay_60s".into(),
            shader_path: "runs/evo/candidates/gen2_llm1.comp".into(),
            spirv_path: "runs/evo/candidates/gen2_llm1.spv".into(),
            budget_ms: 1.5,
            quality_baseline_db: 37.286,
            protocol: EvalProtocol {
                cool_c: 32.0,
                replay_seconds: 60,
                rounds: 4,
                alternating: true,
                p_threshold: 0.01,
            },
        }
    }

    /// 上游 game_opt_loop 写出来的那份 JSON 必须能被原样读进来。
    #[test]
    fn deserialises_the_upstream_request_shape() {
        let raw = r#"{
            "version": 1, "candidate_id": "gen2_c3", "track": "sr",
            "serial": "R5CT30ABCDE", "package": "com.miHoYo.Yuanshen",
            "replay_script": "sr_replay_60s",
            "shader_path": "runs/evo/candidates/gen2_c3.comp",
            "spirv_path": "runs/evo/candidates/gen2_c3.spv",
            "budget_ms": 1.5, "quality_baseline_db": 37.286,
            "protocol": {"cool_c": 32.0, "replay_seconds": 60, "rounds": 4,
                         "alternating": true, "p_threshold": 0.01}
        }"#;
        let r: EvalRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(r.candidate_id, "gen2_c3");
        assert_eq!(r.protocol.cool_c, 32.0);
        assert!(r.protocol.alternating);
        assert!(validate_request(&r).is_ok());
    }

    /// serial 是可选字段, 缺了也要能读 (上游省略即"由 phonefarm 自己挑设备")。
    #[test]
    fn serial_is_optional() {
        let raw = r#"{"version":1,"candidate_id":"c","track":"frame_gen",
            "package":"p","replay_script":"s","shader_path":"a","spirv_path":"b",
            "budget_ms":2.5,"quality_baseline_db":34.0,
            "protocol":{"cool_c":32.0,"replay_seconds":60,"rounds":4,
                        "alternating":true,"p_threshold":0.01}}"#;
        let r: EvalRequest = serde_json::from_str(raw).unwrap();
        assert!(r.serial.is_none());
        assert!(validate_request(&r).is_ok());
    }

    #[test]
    fn rejects_malformed_requests() {
        let mut r = a_request();
        r.version = 2;
        assert!(validate_request(&r).is_err());

        let mut r = a_request();
        r.track = "raytracing".into();
        assert!(validate_request(&r).is_err());

        let mut r = a_request();
        r.budget_ms = 0.0;
        assert!(validate_request(&r).is_err());

        // 每臂只跑 1 轮做不了两样本检验
        let mut r = a_request();
        r.protocol.rounds = 1;
        assert!(validate_request(&r).unwrap_err().contains("两样本"));

        let mut r = a_request();
        r.protocol.p_threshold = 1.5;
        assert!(validate_request(&r).is_err());
    }

    /// 报告序列化出来的枚举必须是上游契约里的那几个字面量。
    #[test]
    fn report_serialises_to_the_contract_vocabulary() {
        let rep = EvalReport {
            candidate_id: "c".into(),
            status: Status::PoorQuality,
            metrics: Metrics {
                operator_latency_ms: 1.1,
                fps_p95_ms: Some(16.9),
                power_watt: 4.7,
                psnr_db: 36.0,
            },
            verdict: Verdict {
                is_pareto_improvement: false,
                p_value: 0.004,
            },
            source: "phonefarm".into(),
        };
        let v = serde_json::to_value(&rep).unwrap();
        assert_eq!(v["status"], "POOR_QUALITY");
        assert_eq!(v["source"], "phonefarm");
        for s in [Status::Pass, Status::Slow, Status::Crash, Status::Reverted] {
            let j = serde_json::to_value(s).unwrap();
            let t = j.as_str().unwrap();
            assert!(
                ["PASS", "SLOW", "CRASH", "REVERTED"].contains(&t),
                "非法 status 字面量 {t}"
            );
        }
    }

    // ---------- SPIR-V 预检 ----------

    /// 造一个最小但结构合法的 compute SPIR-V (只含 header + OpEntryPoint + OpExecutionMode)。
    fn minimal_spirv(local: [u32; 3], entry: &str, model: u32) -> Vec<u8> {
        let mut w: Vec<u32> = vec![SPIRV_MAGIC, 0x0001_0300, 0, 10, 0];

        // OpEntryPoint: model, id, name...
        let mut name: Vec<u8> = entry.as_bytes().to_vec();
        name.push(0);
        while name.len() % 4 != 0 {
            name.push(0);
        }
        let name_words: Vec<u32> = name
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let wc = 3 + name_words.len();
        w.push(((wc as u32) << 16) | OP_ENTRY_POINT as u32);
        w.push(model);
        w.push(1);
        w.extend_from_slice(&name_words);

        // OpExecutionMode: id, LocalSize, x, y, z
        w.push((6u32 << 16) | OP_EXECUTION_MODE as u32);
        w.push(1);
        w.push(EXEC_MODE_LOCAL_SIZE);
        w.extend_from_slice(&local);

        w.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn accepts_a_well_formed_compute_module() {
        let b = minimal_spirv([16, 16, 1], "main", EXEC_MODEL_GLCOMPUTE);
        let i = parse_spirv(&b).unwrap();
        assert_eq!(i.version_major, 1);
        assert_eq!(i.version_minor, 3);
        assert_eq!(i.entry_points, vec!["main".to_string()]);
        assert_eq!(i.local_size, Some([16, 16, 1]));
        assert_eq!(i.bytes, b.len());
    }

    #[test]
    fn rejects_non_spirv_bytes() {
        assert!(parse_spirv(b"not spirv at all, really").is_err());
        assert!(parse_spirv(&[0u8; 8]).is_err());
        // 长度非 4 倍数
        let mut b = minimal_spirv([16, 16, 1], "main", EXEC_MODEL_GLCOMPUTE);
        b.push(0);
        assert!(parse_spirv(&b).is_err());
    }

    /// 顶点/片元着色器不是算子, 挂上去合成层会直接崩。
    #[test]
    fn rejects_a_module_with_no_compute_entry_point() {
        let b = minimal_spirv([1, 1, 1], "main", 0); // 0 = Vertex
        let e = parse_spirv(&b).unwrap_err();
        assert!(e.contains("GLCompute"), "{e}");
    }

    #[test]
    fn rejects_an_oversized_workgroup() {
        let b = minimal_spirv([32, 32, 2], "main", EXEC_MODEL_GLCOMPUTE); // 2048
        let e = parse_spirv(&b).unwrap_err();
        assert!(e.contains("1024"), "{e}");
    }

    #[test]
    fn rejects_a_zero_dimension_workgroup() {
        let b = minimal_spirv([16, 0, 1], "main", EXEC_MODEL_GLCOMPUTE);
        assert!(parse_spirv(&b).is_err());
    }

    /// 截断的字节码必须在越界前被发现, 不能跑飞。
    #[test]
    fn rejects_a_truncated_instruction_stream() {
        let mut b = minimal_spirv([16, 16, 1], "main", EXEC_MODEL_GLCOMPUTE);
        b.truncate(b.len() - 8);
        let e = parse_spirv(&b).unwrap_err();
        assert!(e.contains("越界") || e.contains("入口点"), "{e}");
    }

    #[test]
    fn reads_big_endian_modules() {
        let le = minimal_spirv([8, 8, 1], "main", EXEC_MODEL_GLCOMPUTE);
        let be: Vec<u8> = le
            .chunks_exact(4)
            .flat_map(|c| [c[3], c[2], c[1], c[0]])
            .collect();
        let i = parse_spirv(&be).unwrap();
        assert_eq!(i.local_size, Some([8, 8, 1]));
    }

    // ---------- A/B/A/B 调度 ----------

    #[test]
    fn alternating_schedule_interleaves_the_arms() {
        let s = ab_schedule(3, true);
        assert_eq!(s.len(), 6);
        assert_eq!(
            s,
            vec![
                Arm::Baseline,
                Arm::Candidate,
                Arm::Baseline,
                Arm::Candidate,
                Arm::Baseline,
                Arm::Candidate
            ]
        );
    }

    #[test]
    fn non_alternating_schedule_runs_each_arm_in_a_block() {
        let s = ab_schedule(2, false);
        assert_eq!(
            s,
            vec![Arm::Baseline, Arm::Baseline, Arm::Candidate, Arm::Candidate]
        );
    }

    /// 两臂轮数必须相等 —— 不等会让热漂移的一阶抵消失效。
    #[test]
    fn both_arms_always_get_the_same_number_of_rounds() {
        for rounds in 2..=8 {
            for alt in [true, false] {
                let s = ab_schedule(rounds, alt);
                let a = s.iter().filter(|x| **x == Arm::Baseline).count();
                let b = s.iter().filter(|x| **x == Arm::Candidate).count();
                assert_eq!(a, b, "rounds={rounds} alt={alt}");
                assert_eq!(a, rounds as usize);
            }
        }
    }

    // ---------- runner 回包 ----------

    #[test]
    fn parses_a_successful_runner_report() {
        let out = r#"{"v":1,"ok":true,
            "timing_us":{"samples":[1102.0,1098.5,1105.2],"median":1102.0},
            "psnr_db":38.42,"device":{"name":"Adreno (TM) 840"},"error":null}"#;
        let r = parse_runner_output(out).unwrap();
        assert!(r.ok);
        assert_eq!(r.timing_us.len(), 3);
        assert_eq!(r.psnr_db, Some(38.42));
        assert_eq!(r.device_name.as_deref(), Some("Adreno (TM) 840"));
        assert!((r.median_ms().unwrap() - 1.102).abs() < 1e-9);
    }

    #[test]
    fn picks_the_report_out_of_noisy_stdout() {
        let out = "creating vulkan device\nallocating images\n{\"ok\":true,\"timing_us\":{\"samples\":[900.0,910.0]}}\n";
        let r = parse_runner_output(out).unwrap();
        assert!(r.ok);
        assert_eq!(r.timing_us.len(), 2);
    }

    #[test]
    fn a_failed_runner_report_carries_its_error() {
        let out = r#"{"ok":false,"timing_us":{"samples":[]},"error":"VK_ERROR_DEVICE_LOST"}"#;
        let r = parse_runner_output(out).unwrap();
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("VK_ERROR_DEVICE_LOST"));
        assert!(r.median_ms().is_none());
    }

    /// 自称成功却没有样本 = 回包有问题, 不能当成 0 延迟。
    #[test]
    fn a_success_with_no_samples_is_rejected() {
        let out = r#"{"ok":true,"timing_us":{"samples":[]}}"#;
        assert!(parse_runner_output(out).is_err());
    }

    #[test]
    fn rejects_empty_or_unparseable_runner_output() {
        assert!(parse_runner_output("").is_err());
        assert!(parse_runner_output("Segmentation fault").is_err());
    }

    // ---------- 冻结判定 ----------

    fn rules() -> FrozenRules {
        FrozenRules {
            budget_ms: 1.5,
            quality_baseline_db: 37.286,
            p_threshold: 0.01,
        }
    }

    fn metrics(lat: f32, psnr: f32) -> Metrics {
        Metrics {
            operator_latency_ms: lat,
            fps_p95_ms: Some(16.9),
            power_watt: 4.7,
            psnr_db: psnr,
        }
    }

    fn welch(p: f64) -> WelchResult {
        WelchResult {
            n_a: 4,
            n_b: 4,
            mean_a: 1.2,
            mean_b: 1.0,
            delta: -0.2,
            t: -5.0,
            df: 6.0,
            p_value: p,
        }
    }

    #[test]
    fn over_budget_is_slow_regardless_of_quality() {
        let (s, v) = decide(&rules(), &metrics(1.9, 40.0), None, Some(&welch(0.001)), false);
        assert_eq!(s, Status::Slow);
        assert!(!v.is_pareto_improvement);
    }

    /// 画质低于基线 → 不合格品。哪怕"显著地更快"也不行。
    #[test]
    fn below_the_quality_floor_is_rejected_even_when_significantly_faster() {
        let base = metrics(1.20, 38.0);
        let cand = metrics(0.80, 36.0);
        let (s, v) = decide(&rules(), &cand, Some(&base), Some(&welch(0.0001)), false);
        assert_eq!(s, Status::PoorQuality);
        assert!(
            !v.is_pareto_improvement,
            "掉画质的算子不能被记成 Pareto 改进"
        );
    }

    #[test]
    fn a_crash_short_circuits_everything() {
        let (s, _) = decide(&rules(), &metrics(0.5, 40.0), None, None, true);
        assert_eq!(s, Status::Crash);
    }

    /// 更快但不显著 = 噪声, 不算改进。这是防硬件噪声欺骗的核心一条。
    #[test]
    fn faster_but_not_significant_is_not_an_improvement() {
        let base = metrics(1.02, 38.0);
        let cand = metrics(1.00, 38.1);
        let (s, v) = decide(&rules(), &cand, Some(&base), Some(&welch(0.42)), false);
        assert_eq!(s, Status::Pass);
        assert!(!v.is_pareto_improvement);
        assert!((v.p_value - 0.42).abs() < 1e-12);
    }

    #[test]
    fn significantly_faster_and_within_quality_is_an_improvement() {
        let base = metrics(1.20, 38.0);
        let cand = metrics(1.00, 38.2);
        let (s, v) = decide(&rules(), &cand, Some(&base), Some(&welch(0.001)), false);
        assert_eq!(s, Status::Pass);
        assert!(v.is_pareto_improvement);
    }

    /// 显著但更慢也不是改进 —— 显著性只说明差异真实, 不说明方向对。
    #[test]
    fn significantly_slower_is_not_an_improvement() {
        let base = metrics(1.00, 38.0);
        let cand = metrics(1.30, 38.1);
        let (s, v) = decide(&rules(), &cand, Some(&base), Some(&welch(0.0005)), false);
        assert_eq!(s, Status::Pass);
        assert!(!v.is_pareto_improvement);
    }

    /// 没做检验时 p 记 1.0 哨兵, 不伪造小 p 值。
    #[test]
    fn a_missing_test_reports_p_as_one() {
        let (_, v) = decide(&rules(), &metrics(1.0, 38.0), None, None, false);
        assert_eq!(v.p_value, 1.0);
        assert!(!v.is_pareto_improvement);
    }

    // ---------- CLI ----------

    #[test]
    fn parses_the_usual_invocation() {
        let a = parse_args(&[
            "--request".into(),
            "r.json".into(),
            "--serial".into(),
            "91253241019A".into(),
            "--json".into(),
            "--power-rail".into(),
            "battery".into(),
        ])
        .unwrap();
        assert_eq!(a.request_path.as_deref(), Some("r.json"));
        assert_eq!(a.serial.as_deref(), Some("91253241019A"));
        assert!(a.as_json);
        assert_eq!(a.power_rail, PowerRail::Battery);
    }

    #[test]
    fn unlock_needs_no_request() {
        let a = parse_args(&["--serial".into(), "S".into(), "--unlock".into()]).unwrap();
        assert!(a.unlock_only);
        assert!(a.request_path.is_none());
    }

    #[test]
    fn rejects_missing_request_and_bad_flags() {
        assert!(parse_args(&["--json".into()]).is_err());
        assert!(parse_args(&["--request".into(), "r.json".into(), "--power-rail".into(), "solar".into()]).is_err());
        assert!(parse_args(&["--nope".into()]).is_err());
    }

    #[test]
    fn power_rail_defaults_to_the_read_only_usb_rail() {
        let a = parse_args(&["--request".into(), "r.json".into()]).unwrap();
        assert_eq!(a.power_rail, PowerRail::Usb);
    }
}
