//! CTS 自动化测试 Harness (CTS Harness Spec v1.0): 面向 A2OH 桥接环境的轻量化
//! instrumentation 批量测试与诊断支持。纯增量模块——不触碰 run/benchmark/script/serve
//! 的任何既有路径；设备层只新增 stream_shell/push_file 两个方法，看门狗、遥测切片与
//! 崩溃打包全部作为执行器外层的包装，不进入 Device 基础通道。
//!
//! 与 A2OH 团队《API 桥接的 CTS 快速测试流程》(CTS_FAST_TESTING.md) 的对齐契约:
//!   ① 用例粒度原生支持 Class#method 按需切片(-e class com.x.C#m),而非整包盲跑;
//!   ② 判定枚举严格采用 PASS / ASSERTION_FAIL / ENV_BLOCKED / TIMEOUT / NOT_RUN,
//!      跳过、零用例、运行器错误一律不得计为 PASS;
//!   ③ 掉线纪律: 停设备写操作→留恢复记录→重连后核对 boot_id/包/进程再继续,
//!      不重用旧 PID,不因观察超时直接重启测试;
//!   ④ 证据目录固定四件: stdout.log / hilog_slice.log / telemetry.json / 批次报告
//!      (junit.xml + summary.json),含阶段耗时与恢复状态 VERIFIED|PENDING。
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ══════════════ 判定枚举(同事流程对齐,字面量即契约) ══════════════

/// 单用例判定。序列化值为同事流程约定的字面值,跨工具可直接对账。
/// 变体名刻意保持 SCREAMING(与同事文档字面值 1:1),禁 camelCase/acronym 风格警告。
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseVerdict {
    /// 断言全过
    PASS,
    /// 断言失败(业务语义上的测试失败)
    ASSERTION_FAIL,
    /// 环境阻塞: 运行器缺失、设备掉线、桥接进程崩溃、权限/依赖不满足
    ENV_BLOCKED,
    /// 看门狗超时(全局或静默)
    TIMEOUT,
    /// 未运行: 用例被列出但没有产生结果(跳过/忽略/轮次中断)
    NOT_RUN,
}

impl CaseVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            CaseVerdict::PASS => "PASS",
            CaseVerdict::ASSERTION_FAIL => "ASSERTION_FAIL",
            CaseVerdict::ENV_BLOCKED => "ENV_BLOCKED",
            CaseVerdict::TIMEOUT => "TIMEOUT",
            CaseVerdict::NOT_RUN => "NOT_RUN",
        }
    }
}

/// 单用例结果(含现场材料)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    #[serde(rename = "class")]
    pub class_name: String,
    pub method: String,
    pub verdict: CaseVerdict,
    /// Java 异常调用栈(失败/错误时)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stack: String,
    /// 运行器 stream 文本(附加输出)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stream: String,
    /// 该用例窗口内的原始输出行(stdout.log 的材料)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_lines: Vec<String>,
    pub duration_ms: u64,
}

impl CaseResult {
    pub fn id(&self) -> String {
        format!("{}#{}", self.class_name, self.method)
    }
}

// ══════════════ instrument 原语 ══════════════

/// instrument 动作规格: 原生支持 Class#method 级切片
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentSpec {
    /// 被测包名(target package)
    pub package: String,
    /// Instrumentation runner(类名,可含包前缀或相对写法)
    pub runner: String,
    /// 可选 Class 或 Class#method;缺省=整个 runner 全量
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_or_method: Option<String>,
    /// 全局超时(毫秒),0=不设
    #[serde(default = "d_total_ms")]
    pub timeout_ms: u64,
    /// 静默超时(毫秒): 管道长时间无字符流判定锁死
    #[serde(default = "d_idle_ms")]
    pub idle_timeout_ms: u64,
    /// 透传 -e key value 运行器参数
    #[serde(default)]
    pub env_args: Vec<(String, String)>,
}
fn d_total_ms() -> u64 {
    600_000
}
fn d_idle_ms() -> u64 {
    90_000
}

/// 组装 am instrument 命令(纯函数供单测)。
/// runner 已含包名(含'.')时原样使用,相对写法(".FooRunner")补包名前缀。
pub fn instrument_command(spec: &InstrumentSpec) -> String {
    let mut c = String::from("am instrument -r -w");
    for (k, v) in &spec.env_args {
        c.push_str(&format!(" -e {} {}", k, v));
    }
    if let Some(cm) = &spec.class_or_method {
        if !cm.is_empty() {
            c.push_str(&format!(" -e class {}", cm));
        }
    }
    let runner = if spec.runner.contains('.') {
        spec.runner.clone()
    } else {
        format!("{}.{}", spec.package, spec.runner)
    };
    c.push_str(&format!(" {}/{}", spec.package, runner));
    c
}

// ══════════════ 流式输出解析状态机 ══════════════

/// 解析器产出的事件: 用例开始/用例结束/整轮结束
#[derive(Debug, Clone, PartialEq)]
pub enum ParseEvent {
    CaseStarted { class_name: String, method: String },
    CaseFinished(CaseResult),
    RunFinished { code: i32, tail: Vec<String> },
}

/// `am instrument -r` 输出流状态机。
/// 协议要点(实测 AndroidJUnitRunner):
///   INSTRUMENTATION_STATUS: <key>=<value>   — 字段行;stream/stack 的值可延续到
///                                             后续不带 INSTRUMENTATION_ 前缀的行
///   INSTRUMENTATION_STATUS_CODE: <n>        — 状态码: 1=开始 0=PASS -2=断言失败
///                                             -3=忽略(跳过) -4=假设失败(亦按跳过)
///   INSTRUMENTATION_RESULT: ...             — 整轮结果段
///   INSTRUMENTATION_CODE: <n>               — 整轮结束(-1)
/// 不等整轮结束才出结果: 每个 STATUS_CODE 到达即产出一条 CaseFinished。
pub struct InstrumentParser {
    fields: HashMap<String, String>,
    /// 多行值的归属键(stream/stack);新 INSTRUMENTATION_ 行到达时结束续行
    cont_key: Option<String>,
    cur_class: String,
    cur_method: String,
    cur_lines: Vec<String>,
    cur_start: Option<Instant>,
    /// 整轮尾部原文(OK (N tests) / FAILURES!!! 等),供报告引用
    tail: Vec<String>,
    crashed: bool,
}

impl Default for InstrumentParser {
    fn default() -> Self {
        Self::new()
    }
}

impl InstrumentParser {
    pub fn new() -> Self {
        InstrumentParser {
            fields: HashMap::new(),
            cont_key: None,
            cur_class: String::new(),
            cur_method: String::new(),
            cur_lines: Vec::new(),
            cur_start: None,
            tail: Vec::new(),
            crashed: false,
        }
    }

    /// 喂入一行原始输出,返回本行触发的事件(可能为空)
    pub fn feed_line(&mut self, line: &str) -> Vec<ParseEvent> {
        let mut events = Vec::new();
        let l = line.trim_end();
        self.cur_lines.push(l.to_string());

        if l.contains("Process crashed") || l.contains("shortMsg=Process crashed") {
            self.crashed = true;
        }

        if let Some(rest) = l.strip_prefix("INSTRUMENTATION_STATUS: ") {
            self.cont_key = None;
            if let Some((k, v)) = rest.split_once('=') {
                let k = k.trim().to_string();
                let v = v.to_string();
                if k == "stream" || k == "stack" {
                    self.cont_key = Some(k.clone());
                }
                self.fields.insert(k, v);
            }
            return events;
        }
        if let Some(code_s) = l.strip_prefix("INSTRUMENTATION_STATUS_CODE: ") {
            self.cont_key = None;
            let code: i32 = code_s.trim().parse().unwrap_or(i32::MIN);
            let f = &self.fields;
            let class_name = f.get("class").cloned().unwrap_or_default();
            let method = f.get("test").cloned().unwrap_or_default();
            match code {
                1 => {
                    // 用例开始
                    self.cur_class = class_name.clone();
                    self.cur_method = method.clone();
                    self.cur_lines.clear();
                    self.cur_start = Some(Instant::now());
                    if !class_name.is_empty() {
                        events.push(ParseEvent::CaseStarted { class_name, method });
                    }
                }
                0 | -2 | -3 | -4 => {
                    let verdict = match code {
                        0 => CaseVerdict::PASS,
                        -2 => CaseVerdict::ASSERTION_FAIL,
                        // 跳过/假设失败不得计为通过(同事流程: 跳过不算 PASS)
                        _ => CaseVerdict::NOT_RUN,
                    };
                    let dur = self
                        .cur_start
                        .map(|t| t.elapsed().as_millis() as u64)
                        .unwrap_or(0);
                    let cn = if class_name.is_empty() {
                        self.cur_class.clone()
                    } else {
                        class_name
                    };
                    let m = if method.is_empty() {
                        self.cur_method.clone()
                    } else {
                        method
                    };
                    events.push(ParseEvent::CaseFinished(CaseResult {
                        class_name: cn,
                        method: m,
                        verdict,
                        stack: f.get("stack").cloned().unwrap_or_default(),
                        stream: f.get("stream").cloned().unwrap_or_default(),
                        raw_lines: std::mem::take(&mut self.cur_lines),
                        duration_ms: dur,
                    }));
                }
                _ => {}
            }
            self.fields.clear();
            return events;
        }
        if let Some(rest) = l.strip_prefix("INSTRUMENTATION_RESULT: ") {
            self.cont_key = None;
            if let Some((k, _v)) = rest.split_once('=') {
                if k.trim() == "stream" {
                    self.cont_key = Some("__tail__".into());
                }
                self.tail.push(l.to_string());
            }
            return events;
        }
        if let Some(code_s) = l.strip_prefix("INSTRUMENTATION_CODE: ") {
            self.cont_key = None;
            let code: i32 = code_s.trim().parse().unwrap_or(i32::MIN);
            events.push(ParseEvent::RunFinished {
                code,
                tail: self.tail.clone(),
            });
            return events;
        }

        // 非协议行: 多行值续行(stack/stream),或整轮尾部原文
        match self.cont_key.clone().as_deref() {
            Some(k @ ("stream" | "stack")) => {
                self.fields
                    .entry(k.to_string())
                    .and_modify(|v| {
                        v.push('\n');
                        v.push_str(l);
                    })
                    .or_insert_with(|| l.to_string());
            }
            Some("__tail__") => self.tail.push(l.to_string()),
            _ => {
                // runner 直出文本(如 "com.example.FooTest:" 用例头),仅留痕
                if !l.is_empty() && !self.tail.is_empty() {
                    self.tail.push(l.to_string());
                }
            }
        }
        events
    }

    /// 进程崩溃标记(桥接层 Native Crash 经由 runner 文本露出)
    pub fn saw_process_crash(&self) -> bool {
        self.crashed
    }
}

// ══════════════ 双重看门狗流式执行器 ══════════════

/// 一轮 instrument 执行的总判定
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunVerdict {
    /// 正常收官(用例成败看各自 verdict)
    Completed,
    /// 全局总超时触发级联清理
    TotalTimeout,
    /// 静默超时(管道长时间无字符流,判定锁死)触发级联清理
    IdleTimeout,
    /// 本地进程拉起失败
    SpawnFailed,
}

impl RunVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunVerdict::Completed => "Completed",
            RunVerdict::TotalTimeout => "TotalTimeout",
            RunVerdict::IdleTimeout => "IdleTimeout",
            RunVerdict::SpawnFailed => "SpawnFailed",
        }
    }
}

#[derive(Debug)]
pub struct InstrumentRun {
    pub cases: Vec<CaseResult>,
    pub verdict: RunVerdict,
    /// 全部原始输出行(stdout.log 落盘材料)
    pub stdout_lines: Vec<String>,
    /// 解析器是否看到桥接进程崩溃
    pub process_crashed: bool,
    pub wall_ms: u64,
}

/// 流式执行 instrument: 拉起管道→逐行解析→双重看门狗(total+idle)→超时级联清理。
/// 看门狗与清理是本函数对执行器的纯外层包装;log_sink 逐行接收原始输出(供落盘)。
pub fn run_instrument(
    phone: &crate::device::Device,
    spec: &InstrumentSpec,
    log_sink: &mut dyn FnMut(&str),
) -> InstrumentRun {
    let t0 = Instant::now();
    let cmd = instrument_command(spec);
    log_sink(&format!("$ {cmd}"));

    let spawned = phone.stream_shell(&cmd);
    let mut child: Child = match spawned {
        Ok(c) => c,
        Err(e) => {
            log_sink(&format!("!! spawn failed: {e}"));
            return InstrumentRun {
                cases: vec![],
                verdict: RunVerdict::SpawnFailed,
                stdout_lines: vec![format!("spawn failed: {e}")],
                process_crashed: false,
                wall_ms: t0.elapsed().as_millis() as u64,
            };
        }
    };

    // stdout/stderr 双 reader 线程 → 行通道
    let (tx, rx) = mpsc::channel::<String>();
    fn spawn_reader<R: std::io::Read + Send + 'static>(r: Option<R>, tx: mpsc::Sender<String>) {
        if let Some(s) = r {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(s);
                let mut buf = String::new();
                loop {
                    buf.clear();
                    match reader.read_line(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if tx.send(buf.trim_end().to_string()).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    }
    spawn_reader(child.stdout.take(), tx.clone());
    spawn_reader(child.stderr.take(), tx.clone());
    drop(tx);

    let mut parser = InstrumentParser::new();
    let mut cases: Vec<CaseResult> = Vec::new();
    let mut stdout_lines: Vec<String> = Vec::new();
    let mut last_output = Instant::now();
    let mut verdict = RunVerdict::Completed;

    loop {
        // 收干已到达的行
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                last_output = Instant::now();
                log_sink(&line);
                stdout_lines.push(line.clone());
                for ev in parser.feed_line(&line) {
                    match ev {
                        ParseEvent::CaseStarted { class_name, method } => {
                            println!("    ▶ {class_name}#{method}");
                        }
                        ParseEvent::CaseFinished(c) => {
                            println!(
                                "    {} {} ({:.1}s)",
                                match c.verdict {
                                    CaseVerdict::PASS => "✓",
                                    CaseVerdict::ASSERTION_FAIL => "✗",
                                    CaseVerdict::ENV_BLOCKED => "⊘",
                                    CaseVerdict::TIMEOUT => "⏱",
                                    CaseVerdict::NOT_RUN => "○",
                                },
                                c.id(),
                                c.duration_ms as f64 / 1000.0
                            );
                            cases.push(c);
                        }
                        ParseEvent::RunFinished { code, .. } => {
                            println!("    ── runner 收官 code={code}");
                        }
                    }
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // 管道全关: 子进程应已退出,确认后收官
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => { /* 子进程尚在(管道先于退出关闭的极端态),走超时判定 */
                    }
                    Err(_) => break,
                }
            }
        }

        // 子进程自然退出
        if let Ok(Some(_)) = child.try_wait() {
            // 再抽干剩余行(下一轮 recv 会 Disconnected 后退出)
            continue;
        }

        // 看门狗: 静默超时(管道锁死) / 全局总超时
        let idle = last_output.elapsed();
        let total = t0.elapsed();
        let hit_idle =
            spec.idle_timeout_ms > 0 && idle > Duration::from_millis(spec.idle_timeout_ms);
        let hit_total = spec.timeout_ms > 0 && total > Duration::from_millis(spec.timeout_ms);
        if hit_idle || hit_total {
            verdict = if hit_idle {
                RunVerdict::IdleTimeout
            } else {
                RunVerdict::TotalTimeout
            };
            let kind = verdict.as_str();
            log_sink(&format!(
                "!! watchdog {kind}: idle={:.1}s total={:.1}s — 级联清理开始",
                idle.as_secs_f64(),
                total.as_secs_f64()
            ));
            cascade_cleanup(phone, &mut child, &spec.package, log_sink);
            break;
        }
    }

    let wall_ms = t0.elapsed().as_millis() as u64;
    let crashed = parser.saw_process_crash();

    // 看门狗触发的轮次: 已开始但未收官的用例如实记 TIMEOUT/NOT_RUN
    if verdict != RunVerdict::Completed {
        // 本轮所有已产出的用例保留;若有"已开始未结束"的用例,补一条 TIMEOUT
        let finished_started = cases
            .iter()
            .any(|c| c.class_name == parser.cur_class && c.method == parser.cur_method);
        if !parser.cur_class.is_empty() && !finished_started {
            cases.push(CaseResult {
                class_name: parser.cur_class.clone(),
                method: parser.cur_method.clone(),
                verdict: CaseVerdict::TIMEOUT,
                stack: String::new(),
                stream: format!(
                    "watchdog {} after {:.1}s",
                    verdict.as_str(),
                    wall_ms as f64 / 1000.0
                ),
                raw_lines: parser.cur_lines.clone(),
                duration_ms: wall_ms,
            });
        }
    }

    InstrumentRun {
        cases,
        verdict,
        stdout_lines,
        process_crashed: crashed,
        wall_ms,
    }
}

/// 级联清理(看门狗外层包装): 终止本地管道 → 设备端强杀被测进程 → 残余清理。
/// 每步 best-effort,如实记录,不抛错。
fn cascade_cleanup(
    phone: &crate::device::Device,
    child: &mut Child,
    pkg: &str,
    log_sink: &mut dyn FnMut(&str),
) {
    log_sink("  [1/3] 终止本地管道(child.kill)");
    let _ = child.kill();
    let _ = child.wait();
    log_sink(&format!("  [2/3] 设备端强杀被测进程 {pkg}"));
    let out = phone.shell(&format!("am force-stop {pkg}"), 8000);
    if !out.trim().is_empty() {
        log_sink(&format!("    force-stop: {}", out.trim()));
    }
    // runner 宿主进程(与 target 同包名)若仍在,按 pidof 补刀
    let out2 = phone.shell(
        &format!("p=$(pidof {pkg}); [ -n \"$p\" ] && kill -9 $p; echo rc=$?"),
        6000,
    );
    log_sink(&format!("    pidof-kill: {}", out2.trim()));
    log_sink("  [3/3] 残余环境清理(回 HOME,释放输入焦点)");
    phone.home();
}

// ══════════════ 日志时间窗切片(hilog / logcat 同构) ══════════════

/// 设备侧日志 dump: OH 走 `hilog -x`,Android 走 `logcat -d`。
pub fn device_log_dump(phone: &crate::device::Device) -> String {
    match phone.backend_name() {
        "hdc" => phone.shell("hilog -x", 15000),
        _ => phone.shell("logcat -d", 15000),
    }
}

/// 设备当前时间锚点(月,日,当年秒数浮点): hilog/logcat 行首时间戳无年份,
/// 以设备 `date` 为锚,窗口比较全部在设备时间域内进行(主机时区/时钟偏差不入链)。
pub fn device_time_anchor(phone: &crate::device::Device) -> Option<(u32, u32, f64)> {
    let out = phone.shell("date '+%m %d %H %M %S'", 5000);
    let nums: Vec<f64> = out
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect();
    if nums.len() >= 5 {
        Some((
            nums[0] as u32,
            nums[1] as u32,
            nums[2] * 3600.0 + nums[3] * 60.0 + nums[4],
        ))
    } else {
        None
    }
}

/// 伪时间戳比较键: (年内日序*86400 + 当日秒数)。跨年/跨日边界按月日序保守比较——
/// 测试窗口量级是秒到分钟,年内日序已足够单调。
fn ts_key(month: u32, day: u32, secs: f64) -> f64 {
    let mut days = 0u32;
    for m in 1..month.min(13) {
        days += match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            _ => 28,
        };
    }
    (days + day) as f64 * 86400.0 + secs
}

/// 解析日志行首时间戳 "MM-DD HH:MM:SS.mmm" → ts_key(纯函数)。
pub fn parse_log_ts(line: &str) -> Option<f64> {
    let b = line.as_bytes();
    if b.len() < 18 || b[2] != b'-' || b[5] != b' ' {
        return None;
    }
    let month: u32 = line[0..2].parse().ok()?;
    let day: u32 = line[3..5].parse().ok()?;
    let hour: f64 = line[6..8].parse().ok()?;
    let min: f64 = line[9..11].parse().ok()?;
    let sec: f64 = line[12..14].parse().ok()?;
    let ms: f64 = line[15..18].parse().ok()?;
    Some(ts_key(
        month,
        day,
        hour * 3600.0 + min * 60.0 + sec + ms / 1000.0,
    ))
}

/// 按时间窗切片日志(前后带 buffer 秒)。无时间戳的行跟随上一行归属(续行语义)。
/// 若整段日志一个时间戳都解析不出,如实返回全文加说明头——不假装切了。
pub fn slice_log_by_window(
    dump: &str,
    start: (u32, u32, f64),
    end: (u32, u32, f64),
    buffer_secs: f64,
) -> String {
    let lo = ts_key(start.0, start.1, start.2) - buffer_secs;
    let hi = ts_key(end.0, end.1, end.2) + buffer_secs;
    let mut any_ts = false;
    let mut in_window = false;
    let mut kept: Vec<&str> = Vec::new();
    for line in dump.lines() {
        match parse_log_ts(line) {
            Some(k) => {
                any_ts = true;
                in_window = k >= lo && k <= hi;
                if in_window {
                    kept.push(line);
                }
            }
            None => {
                if in_window {
                    kept.push(line);
                }
            }
        }
    }
    if !any_ts {
        return format!("(警告: 日志行无标准时间戳,无法按窗切片,以下为全文)\n{dump}");
    }
    if kept.is_empty() {
        return format!("(窗口 [{lo:.0}, {hi:.0}] 内无日志行——可能设备日志缓冲区已滚动)\n");
    }
    kept.join("\n")
}

// ══════════════ 遥测前后差值 ══════════════

/// 用例前后遥测对(内存/FD/线程/crash/anr 计数),差值进 telemetry.json。
pub fn telemetry_snapshot(phone: &crate::device::Device, pkg: &str) -> Value {
    let t = phone.telemetry(false, pkg);
    json!({
        "pid": t.pid,
        "vm_rss_kb": t.vm_rss_kb,
        "threads": t.threads,
        "fd_count": t.fd_count,
        "socket_count": t.socket_count,
        "app_pss_kb": t.app_pss_kb,
        "crash_count": t.crash_count,
        "anr_count": t.anr_count,
        "mem_avail_kb": t.mem_avail_kb,
        "cpu_total_pct": t.cpu_total_pct,
    })
}

/// 前后快照差值(纯函数): 数值字段出 delta,缺测如实为 null
pub fn telemetry_diff(before: &Value, after: &Value) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("before".into(), before.clone());
    m.insert("after".into(), after.clone());
    let mut delta = serde_json::Map::new();
    for k in [
        "vm_rss_kb",
        "threads",
        "fd_count",
        "socket_count",
        "app_pss_kb",
        "crash_count",
        "anr_count",
        "mem_avail_kb",
    ] {
        let d = match (before[k].as_i64(), after[k].as_i64()) {
            (Some(b), Some(a)) => Value::from(a - b),
            _ => Value::Null,
        };
        delta.insert(k.to_string(), d);
    }
    m.insert("delta".into(), Value::Object(delta));
    Value::Object(m)
}

// ══════════════ Crash Bundle 打包 ══════════════

/// 文件系统安全的用例目录名(Class#method → Class_method)
pub fn sanitize_case_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 失败用例现场打包: artifacts/<module>/<test_case>/
///   stdout.log        该用例窗口的运行器原始输出
///   hilog_slice.log   前后带缓冲的设备日志切片
///   telemetry.json    前后内存/FD 遥测差值
///   meta.json         判定/耗时/时间锚点
#[allow(clippy::too_many_arguments)]
pub fn write_crash_bundle(
    out_root: &Path,
    module: &str,
    case: &CaseResult,
    stdout_lines: &[String],
    log_dump: &str,
    win_start: (u32, u32, f64),
    win_end: (u32, u32, f64),
    tele: &Value,
) -> Result<PathBuf, String> {
    let dir = out_root
        .join("artifacts")
        .join(sanitize_case_id(module))
        .join(sanitize_case_id(&case.id()));
    fs::create_dir_all(&dir).map_err(|e| format!("建目录失败 {}: {e}", dir.display()))?;

    let case_lines: Vec<&str> = if case.raw_lines.is_empty() {
        stdout_lines.iter().map(|s| s.as_str()).collect()
    } else {
        case.raw_lines.iter().map(|s| s.as_str()).collect()
    };
    fs::write(dir.join("stdout.log"), case_lines.join("\n")).map_err(|e| e.to_string())?;
    fs::write(
        dir.join("hilog_slice.log"),
        slice_log_by_window(log_dump, win_start, win_end, 5.0),
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        dir.join("telemetry.json"),
        serde_json::to_string_pretty(tele).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(&json!({
            "module": module,
            "case": case.id(),
            "verdict": case.verdict.as_str(),
            "duration_ms": case.duration_ms,
            "window": {"start": win_start, "end": win_end, "buffer_s": 5},
        }))
        .unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(dir)
}

// ══════════════ 报告: JUnit XML + summary.json ══════════════

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 单模块 JUnit XML(纯函数供单测)。跳过/未运行按 <skipped>,绝不计入通过。
pub fn junit_xml(module: &str, cases: &[CaseResult], wall_ms: u64) -> String {
    let tests = cases.len();
    let failures = cases
        .iter()
        .filter(|c| c.verdict == CaseVerdict::ASSERTION_FAIL)
        .count();
    let errors = cases
        .iter()
        .filter(|c| matches!(c.verdict, CaseVerdict::ENV_BLOCKED | CaseVerdict::TIMEOUT))
        .count();
    let skipped = cases
        .iter()
        .filter(|c| c.verdict == CaseVerdict::NOT_RUN)
        .count();
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str(&format!(
        "<testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{:.3}\">\n",
        xml_escape(module), tests, failures, errors, skipped, wall_ms as f64 / 1000.0));
    for c in cases {
        s.push_str(&format!(
            "  <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\" verdict=\"{}\">",
            xml_escape(&c.class_name),
            xml_escape(&c.method),
            c.duration_ms as f64 / 1000.0,
            c.verdict.as_str()
        ));
        match c.verdict {
            CaseVerdict::PASS => s.push_str("</testcase>\n"),
            CaseVerdict::ASSERTION_FAIL => {
                let msg = c.stack.lines().next().unwrap_or("assertion failed");
                s.push_str(&format!(
                    "\n    <failure message=\"{}\">{}</failure>\n  </testcase>\n",
                    xml_escape(msg),
                    xml_escape(&c.stack)
                ));
            }
            CaseVerdict::NOT_RUN => {
                s.push_str("\n    <skipped/>\n  </testcase>\n");
            }
            _ => {
                let msg = if c.stream.is_empty() {
                    c.verdict.as_str().to_string()
                } else {
                    c.stream.clone()
                };
                s.push_str(&format!(
                    "\n    <error message=\"{}\">{}</error>\n  </testcase>\n",
                    xml_escape(c.verdict.as_str()),
                    xml_escape(&msg)
                ));
            }
        }
    }
    s.push_str("</testsuite>\n");
    s
}

/// 批次汇总(summary.json 的骨架)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleReport {
    pub module: String,
    pub package: String,
    pub runner: String,
    pub run_verdict: String,
    pub cases: Vec<CaseResult>,
    pub wall_ms: u64,
    pub retries_used: u32,
    /// 恢复状态: VERIFIED / PENDING(同事流程: PENDING 时不得宣称整轮完成)
    pub recovery: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crash_bundles: Vec<String>,
}

pub fn summary_json(batch_id: &str, serial: &str, modules: &[ModuleReport], wall_ms: u64) -> Value {
    let mut counts = BTreeMap::new();
    for v in [
        CaseVerdict::PASS,
        CaseVerdict::ASSERTION_FAIL,
        CaseVerdict::ENV_BLOCKED,
        CaseVerdict::TIMEOUT,
        CaseVerdict::NOT_RUN,
    ] {
        counts.insert(v.as_str().to_string(), 0usize);
    }
    let mut total_cases = 0usize;
    for m in modules {
        for c in &m.cases {
            *counts.entry(c.verdict.as_str().to_string()).or_insert(0) += 1;
            total_cases += 1;
        }
    }
    let all_verified = modules.iter().all(|m| m.recovery == "VERIFIED");
    json!({
        "batch_id": batch_id,
        "serial": serial,
        "wall_ms": wall_ms,
        "modules": modules.len(),
        "total_cases": total_cases,
        "verdicts": counts,
        "recovery": if all_verified { "VERIFIED" } else { "PENDING" },
        "module_reports": modules,
    })
}

/// 批次是否"干净收官"(纯函数供单测): 恢复全 VERIFIED 且无失败/阻塞/超时
pub fn batch_is_clean(summary: &Value) -> bool {
    summary["recovery"] == "VERIFIED"
        && summary["verdicts"]["ASSERTION_FAIL"].as_u64() == Some(0)
        && summary["verdicts"]["ENV_BLOCKED"].as_u64() == Some(0)
        && summary["verdicts"]["TIMEOUT"].as_u64() == Some(0)
}

// ══════════════ 批次状态(断点续跑) ══════════════

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchState {
    /// 模块名 → 状态("done" | "failed")
    #[serde(default)]
    pub modules: BTreeMap<String, String>,
}

impl BatchState {
    pub fn load(out_dir: &Path) -> BatchState {
        fs::read_to_string(out_dir.join("batch_state.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, out_dir: &Path) {
        let _ = fs::write(
            out_dir.join("batch_state.json"),
            serde_json::to_string_pretty(self).unwrap_or_default(),
        );
    }
}

// ══════════════ 同事 profile / environment.json 兼容层 ══════════════

/// 同事的 API 测试配置(profile): 一个 API 对应原始 CTS 模块 + Class#method 列表。
/// 未知字段一律容忍(serde 默认),只消费认识的键——对齐而不绑架对方格式演进。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CtsProfile {
    /// API 名(如 getPackageInfo)
    #[serde(default, alias = "api", alias = "name")]
    pub api_name: String,
    /// 原始 CTS 模块名
    #[serde(default, alias = "cts_module")]
    pub module: String,
    /// 被测包名
    #[serde(default, alias = "target_package", alias = "pkg")]
    pub package: String,
    /// instrumentation runner
    #[serde(default)]
    pub runner: String,
    /// Class#method 列表(空=整模块)
    #[serde(default, alias = "methods", alias = "test_cases")]
    pub cases: Vec<String>,
    /// 预期用例数(对账用;缺省=cases.len())
    #[serde(default, alias = "expected_count")]
    pub expected_cases: Option<usize>,
    /// 测试 APK 路径(用于部署;可空=设备上已装)
    #[serde(default, alias = "apk")]
    pub apk_path: Option<String>,
}

/// 同事的环境配置(environment.json): 设备/版本/ABI/夹具。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CtsEnvironment {
    #[serde(default, alias = "device", alias = "serial")]
    pub device_serial: Option<String>,
    #[serde(default, alias = "runtime_version")]
    pub android_runtime: Option<String>,
    #[serde(default)]
    pub abi: Option<String>,
    #[serde(default, alias = "cts_version")]
    pub cts_or_apk_version: Option<String>,
}

pub fn load_profile(path: &Path) -> Result<CtsProfile, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("读不到 profile {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("profile 解析失败 {}: {e}", path.display()))
}

pub fn load_environment(path: &Path) -> Result<CtsEnvironment, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("读不到 environment {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("environment 解析失败 {}: {e}", path.display()))
}

// ══════════════ APK 发现与差量部署 ══════════════

/// `pm list instrumentation` 输出解析(纯函数供单测):
///   instrumentation:com.x.test/androidx.test.runner.AndroidJUnitRunner (target=com.x)
/// 返回 (test_pkg, runner, target_pkg) 三元组列表。
pub fn parse_instrumentation_list(s: &str) -> Vec<(String, String, String)> {
    let re = Regex::new(r"instrumentation:([^/\s]+)/(\S+)\s+\(target=([^)]+)\)").unwrap();
    re.captures_iter(s)
        .map(|c| (c[1].to_string(), c[2].to_string(), c[3].to_string()))
        .collect()
}

/// aapt dump badging 的包名解析(纯函数): `package: name='com.x' ...`
pub fn parse_aapt_package(s: &str) -> Option<String> {
    let re = Regex::new(r"package: name='([^']+)'").unwrap();
    re.captures(s).map(|c| c[1].to_string())
}

/// 文件 SHA256(差量传输的判据)
pub fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::Digest;
    let mut f = fs::File::open(path).map_err(|e| format!("打不开 {}: {e}", path.display()))?;
    let mut h = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(e) => return Err(format!("读文件失败: {e}")),
        }
    }
    Ok(format!("{:x}", h.finalize()))
}

/// 设备端持久缓存目录(差量部署的落点;同事流程目前每轮全量推独立暂存目录,
/// 本 harness 改为持久缓存 + 散列比对,跳过未变化的传输与安装)
pub const DEVICE_CACHE_DIR: &str = "/data/local/tmp/pf_cts_cache";

/// 差量部署: 本地散列 ↔ 设备缓存散列比对,未变化跳过 push 与安装。
/// 返回 (设备侧apk路径, 本轮是否新装)。install_cmd 模板(可空=adb 默认 install -r;
/// hdc 必须显式给,如 "bm install -p {apk}")——A2OH 桥的安装入口由桥定义,不臆造。
pub fn deploy_apk(
    phone: &crate::device::Device,
    apk: &Path,
    install_cmd: Option<&str>,
    log_sink: &mut dyn FnMut(&str),
) -> Result<(String, bool), String> {
    let name = apk
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("APK 路径无文件名: {}", apk.display()))?;
    let remote = format!("{DEVICE_CACHE_DIR}/{name}");
    let hash_file = format!("{remote}.sha256");
    let local_hash = file_sha256(apk)?;

    phone.shell(&format!("mkdir -p {DEVICE_CACHE_DIR}"), 5000);
    let remote_hash = phone
        .shell(&format!("cat {hash_file} 2>/dev/null"), 5000)
        .trim()
        .to_string();

    if remote_hash == local_hash {
        log_sink(&format!("  差量命中: {name} 散列一致,跳过传输与安装"));
        return Ok((remote, false));
    }

    log_sink(&format!("  推送 {name} → {remote}"));
    if !phone.push_file(&apk.to_string_lossy(), &remote) {
        return Err(format!("推送失败: {}", apk.display()));
    }
    match install_cmd {
        Some(tpl) => {
            let cmd = tpl.replace("{apk}", &remote);
            log_sink(&format!("  安装: {cmd}"));
            let out = phone.shell(&cmd, 180_000);
            if out.contains("Failure") || out.contains("error") || out.contains("fail") {
                return Err(format!("安装失败: {}", out.trim()));
            }
        }
        None => {
            if phone.backend_name() == "hdc" {
                return Err(
                    "hdc/A2OH 后端必须显式给 --install-cmd(桥安装入口由桥定义,不臆造)".into(),
                );
            }
            log_sink("  安装: adb install -r");
            let out = phone.shell(&format!("pm install -r {remote}"), 180_000);
            if !out.contains("Success") {
                return Err(format!("安装失败: {}", out.trim()));
            }
        }
    }
    phone.shell(&format!("echo {local_hash} > {hash_file}"), 5000);
    Ok((remote, true))
}

// ══════════════ 掉线恢复记录(不重用旧 PID 纪律) ══════════════

/// 设备 boot_id: 重连后核对——变了说明设备重启过,旧进程身份全部作废。
pub fn device_boot_id(phone: &crate::device::Device) -> String {
    phone
        .shell("cat /proc/sys/kernel/random/boot_id 2>/dev/null", 5000)
        .trim()
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryRecord {
    pub batch_id: String,
    pub boot_id: String,
    pub pending_module: String,
    pub note: String,
    pub ts_ms: u64,
}

pub fn save_recovery(out_dir: &Path, rec: &RecoveryRecord) {
    let _ = fs::write(
        out_dir.join("recovery_pending.json"),
        serde_json::to_string_pretty(rec).unwrap_or_default(),
    );
}

pub fn clear_recovery(out_dir: &Path) {
    let _ = fs::remove_file(out_dir.join("recovery_pending.json"));
}

/// 自愈 Hook: 桥接进程异常/设备失联时调外部恢复脚本,然后核对 boot_id。
/// 返回 (是否恢复到有心跳, 新boot_id)。脚本路径为空=不自愈。
pub fn heal_and_verify(
    phone: &crate::device::Device,
    heal_script: Option<&str>,
    log_sink: &mut dyn FnMut(&str),
) -> (bool, String) {
    if let Some(script) = heal_script {
        log_sink(&format!("  🔧 触发自愈脚本: {script}"));
        let st = std::process::Command::new("sh")
            .arg(script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match st {
            Ok(s) if s.success() => log_sink("  自愈脚本执行完成"),
            Ok(s) => log_sink(&format!("  ⚠ 自愈脚本退出码 {s}")),
            Err(e) => log_sink(&format!("  ⚠ 自愈脚本拉起失败: {e}")),
        }
    }
    // 新会话新进程探活(不重用任何旧 PID/旧管道)
    for i in 1..=10 {
        if phone.health_check(5000) {
            let bid = device_boot_id(phone);
            log_sink(&format!("  设备心跳恢复(第{i}次), boot_id={bid}"));
            return (true, bid);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    (false, String::new())
}

// ══════════════ 批量调度器 ══════════════

pub struct BatchCfg {
    pub serial: Option<String>,
    /// APK 目录扫描模式
    pub apk_dir: Option<String>,
    /// 显式模块列表(pkg/runner),与 apk_dir 二选一
    pub modules: Vec<String>,
    /// 同事 profile(JSON),从中展开 Class#method 切片
    pub profile: Option<String>,
    /// 同事 environment.json
    pub environment: Option<String>,
    pub include: Option<String>,
    pub exclude: Option<String>,
    pub resume: bool,
    pub retry: u32,
    pub timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub heal_script: Option<String>,
    pub install_cmd: Option<String>,
    pub out_dir: Option<String>,
}

/// 一个待跑模块: 包名 + runner + 该模块下的方法切片(空=整模块)
#[derive(Debug, Clone)]
pub struct ModulePlan {
    package: String,
    runner: String,
    methods: Vec<String>,
    apk: Option<PathBuf>,
}

impl ModulePlan {
    fn name(&self) -> String {
        format!("{}/{}", self.package, self.runner)
    }
}

/// 解析 --module 参数 "pkg/runner" 或 "pkg/runner#Class.method"…(纯函数)
pub fn parse_module_arg(s: &str) -> Option<(String, String)> {
    let (pkg, runner) = s.split_once('/')?;
    if pkg.is_empty() || runner.is_empty() {
        return None;
    }
    Some((pkg.to_string(), runner.to_string()))
}

/// profile → ModulePlan(纯函数): cases 列表原样作为 Class#method 切片
pub fn plan_from_profile(p: &CtsProfile) -> Result<ModulePlan, String> {
    if p.package.is_empty() || p.runner.is_empty() {
        return Err("profile 缺 package/runner 字段".into());
    }
    if let Some(exp) = p.expected_cases {
        if exp != p.cases.len() {
            println!(
                "  ⚠ profile 预期 {exp} 用例,实际列出 {} 条——以实列为准并对账",
                p.cases.len()
            );
        }
    }
    Ok(ModulePlan {
        package: p.package.clone(),
        runner: p.runner.clone(),
        methods: p.cases.clone(),
        apk: p.apk_path.as_ref().map(PathBuf::from),
    })
}

/// APK 目录扫描 → ModulePlan 列表: aapt 取包名(无 aapt 用文件名兜底并如实标注),
/// pm list instrumentation 对账 runner;对不上的模块如实 ENV_BLOCKED,不猜。
fn scan_apk_dir(
    phone: &crate::device::Device,
    dir: &Path,
    install_cmd: Option<&str>,
    log_sink: &mut dyn FnMut(&str),
) -> Result<Vec<ModulePlan>, String> {
    let mut apks: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("读不到 APK 目录 {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("apk"))
        .collect();
    apks.sort();
    if apks.is_empty() {
        return Err(format!("目录 {} 下没有 .apk", dir.display()));
    }

    let instr = parse_instrumentation_list(&phone.shell("pm list instrumentation", 15000));
    let mut plans = Vec::new();
    for apk in apks {
        // 包名: aapt 优先,文件名兜底
        let pkg_guess = std::process::Command::new("aapt")
            .args(["dump", "badging", &apk.to_string_lossy()])
            .output()
            .ok()
            .and_then(|o| parse_aapt_package(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_else(|| {
                let stem = apk
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                log_sink(&format!(
                    "  (无 aapt,{stem} 以文件名猜包名,runner 对账为准)"
                ));
                stem
            });

        let (_, installed) = deploy_apk(phone, &apk, install_cmd, log_sink)?;
        if installed {
            // 新装后 runner 清单可能变化,重查
            let instr2 = parse_instrumentation_list(&phone.shell("pm list instrumentation", 15000));
            for (tp, runner, target) in instr2 {
                if target == pkg_guess || tp == pkg_guess || target.starts_with(&pkg_guess) {
                    plans.push(ModulePlan {
                        package: target,
                        runner,
                        methods: vec![],
                        apk: Some(apk.clone()),
                    });
                }
            }
        } else {
            for (tp, runner, target) in &instr {
                if target == &pkg_guess || tp == &pkg_guess || target.starts_with(&pkg_guess) {
                    plans.push(ModulePlan {
                        package: target.clone(),
                        runner: runner.clone(),
                        methods: vec![],
                        apk: Some(apk.clone()),
                    });
                }
            }
        }
        if !plans
            .iter()
            .any(|p| p.apk.as_deref() == Some(apk.as_path()))
        {
            log_sink(&format!(
                "  ⚠ {} 装上了但 pm list instrumentation 对不出 runner——记 ENV_BLOCKED 待查",
                apk.display()
            ));
        }
    }
    Ok(plans)
}

/// 批次主入口。返回进程退出码(0=全部 VERIFIED 且无 ASSERTION_FAIL/ENV_BLOCKED/TIMEOUT)
pub fn run_batch(cfg: &BatchCfg) -> Result<i32, String> {
    let t0 = Instant::now();
    let batch_id = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let out_dir = PathBuf::from(
        cfg.out_dir
            .clone()
            .unwrap_or_else(|| format!("cts-batch-{batch_id}")),
    );
    fs::create_dir_all(&out_dir).map_err(|e| format!("建输出目录失败: {e}"))?;

    // 环境文件对齐(同事 environment.json)
    let mut serial = cfg.serial.clone();
    if let Some(env_path) = &cfg.environment {
        let env = load_environment(Path::new(env_path))?;
        if serial.is_none() {
            serial = env.device_serial.clone();
        }
        println!(
            "环境: serial={:?} runtime={:?} abi={:?} cts={:?}",
            serial, env.android_runtime, env.abi, env.cts_or_apk_version
        );
    }

    let tmp = std::env::temp_dir()
        .join(format!("phonefarm-cts-{}", std::process::id()))
        .to_string_lossy()
        .to_string();
    let _ = fs::create_dir_all(&tmp);
    let phone = crate::device::Device::new(serial.clone(), tmp);

    // 模块计划: profile > --module > --dir
    let mut plans: Vec<ModulePlan> = Vec::new();
    let mut sink = |line: &str| println!("{line}");
    if let Some(pf) = &cfg.profile {
        let profile = load_profile(Path::new(pf))?;
        println!(
            "profile: api={} module={} 切片={}条",
            profile.api_name,
            profile.module,
            profile.cases.len()
        );
        plans.push(plan_from_profile(&profile)?);
    }
    for m in &cfg.modules {
        let (pkg, runner) = parse_module_arg(m)
            .ok_or_else(|| format!("--module 格式应为 pkg/runner,收到 '{m}'"))?;
        plans.push(ModulePlan {
            package: pkg,
            runner,
            methods: vec![],
            apk: None,
        });
    }
    if let Some(dir) = &cfg.apk_dir {
        plans.extend(scan_apk_dir(
            &phone,
            Path::new(dir),
            cfg.install_cmd.as_deref(),
            &mut sink,
        )?);
    }
    if plans.is_empty() {
        return Err("没有可跑的模块: 给 --profile / --module / --dir 之一".into());
    }

    // 白/黑名单正则(模块名 pkg/runner)
    let inc = cfg
        .include
        .as_deref()
        .map(Regex::new)
        .transpose()
        .map_err(|e| format!("--include 正则无效: {e}"))?;
    let exc = cfg
        .exclude
        .as_deref()
        .map(Regex::new)
        .transpose()
        .map_err(|e| format!("--exclude 正则无效: {e}"))?;
    plans.retain(|p| {
        let n = p.name();
        inc.as_ref().is_none_or(|r| r.is_match(&n)) && exc.as_ref().is_none_or(|r| !r.is_match(&n))
    });
    if plans.is_empty() {
        return Err("白/黑名单过滤后没有剩余模块".into());
    }

    // 断点续跑
    let mut state = BatchState::load(&out_dir);
    if cfg.resume && !state.modules.is_empty() {
        println!(
            "断点续跑: 跳过已完成 {} 个模块",
            state.modules.values().filter(|s| *s == "done").count()
        );
    }

    // 开局核对: boot_id + 上次未完成的恢复(同事流程"检查"步)
    let boot_id0 = device_boot_id(&phone);
    if out_dir.join("recovery_pending.json").exists() {
        println!("⚠ 发现上次未完成的恢复记录(recovery_pending.json)——先核对设备状态再继续");
        println!("  当前 boot_id={boot_id0}");
    }

    println!("════ CTS 批次 {batch_id} ════");
    println!("输出目录: {}", out_dir.display());
    println!(
        "模块: {} 个 | 重试: {} | 总超时: {}ms | 静默超时: {}ms",
        plans.len(),
        cfg.retry,
        cfg.timeout_ms,
        cfg.idle_timeout_ms
    );

    let mut reports: Vec<ModuleReport> = Vec::new();
    for plan in &plans {
        let mname = plan.name();
        if cfg.resume && state.modules.get(&mname).map(|s| s.as_str()) == Some("done") {
            println!("── {mname}: 已完成,跳过");
            continue;
        }
        println!("── 模块 {mname} (切片 {} 条) ──", plan.methods.len());

        // profile 的 APK 尚未部署时(直接给 --profile 而非 --dir),补差量部署
        if let Some(apk) = &plan.apk {
            if apk.exists() {
                let _ = deploy_apk(&phone, apk, cfg.install_cmd.as_deref(), &mut sink)?;
            }
        }

        // 遥测通道(被测包)
        let tele_ok = phone.telemetry_setup(&plan.package);

        // 方法切片列表(空=整模块一次跑)
        let methods: Vec<Option<String>> = if plan.methods.is_empty() {
            vec![None]
        } else {
            plan.methods.iter().map(|m| Some(m.clone())).collect()
        };

        let mut module_cases: Vec<CaseResult> = Vec::new();
        let mut retries_used = 0u32;
        let mut run_verdict = RunVerdict::Completed;
        let mut recovery = "VERIFIED".to_string();
        let mut bundles: Vec<String> = Vec::new();
        let mod_t0 = Instant::now();
        let stdout_path = out_dir.join(format!("{}_stdout.log", sanitize_case_id(&mname)));
        let mut stdout_file = fs::File::create(&stdout_path).map_err(|e| e.to_string())?;
        let mut file_sink = |line: &str| {
            let _ = writeln!(stdout_file, "{line}");
        };

        for method in &methods {
            let spec = InstrumentSpec {
                package: plan.package.clone(),
                runner: plan.runner.clone(),
                class_or_method: method.clone(),
                timeout_ms: cfg.timeout_ms,
                idle_timeout_ms: cfg.idle_timeout_ms,
                env_args: vec![],
            };

            let win_start = device_time_anchor(&phone).unwrap_or((1, 1, 0.0));
            let tele_before = if tele_ok {
                telemetry_snapshot(&phone, &plan.package)
            } else {
                Value::Null
            };
            let run = run_instrument(&phone, &spec, &mut file_sink);
            let win_end = device_time_anchor(&phone).unwrap_or(win_start);
            let tele_after = if tele_ok {
                telemetry_snapshot(&phone, &plan.package)
            } else {
                Value::Null
            };

            if run.verdict != RunVerdict::Completed {
                run_verdict = run.verdict.clone();
            }

            // 桥接进程崩溃 / 看门狗触发 → 掉线纪律: 停写→留记录→自愈→核对 boot_id
            if run.process_crashed || run.verdict != RunVerdict::Completed {
                eprintln!(
                    "  ⚠ {} (crash={})——进入恢复流程",
                    run.verdict.as_str(),
                    run.process_crashed
                );
                save_recovery(
                    &out_dir,
                    &RecoveryRecord {
                        batch_id: batch_id.clone(),
                        boot_id: boot_id0.clone(),
                        pending_module: mname.clone(),
                        note: format!(
                            "verdict={} crashed={}",
                            run.verdict.as_str(),
                            run.process_crashed
                        ),
                        ts_ms: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0),
                    },
                );
                let (alive, new_boot) =
                    heal_and_verify(&phone, cfg.heal_script.as_deref(), &mut sink);
                if !alive {
                    recovery = "PENDING".into();
                    eprintln!("  ✗ 设备未恢复——本模块剩余切片记 ENV_BLOCKED,批次继续下一个模块");
                    break;
                }
                if !new_boot.is_empty() && new_boot != boot_id0 {
                    println!("  (boot_id 变化: 设备已重启,旧进程身份作废)");
                }
                clear_recovery(&out_dir);
            }

            // 用例归账 + 失败重试(--retry)
            let mut round_cases = run.cases;
            if method.is_some() && cfg.retry > 0 {
                let mut attempt = 0;
                while round_cases.iter().any(|c| {
                    matches!(
                        c.verdict,
                        CaseVerdict::ASSERTION_FAIL | CaseVerdict::TIMEOUT
                    )
                }) && attempt < cfg.retry
                {
                    attempt += 1;
                    retries_used += 1;
                    println!(
                        "    ↻ 重试 {}/{}: {}",
                        attempt,
                        cfg.retry,
                        method.as_deref().unwrap_or("")
                    );
                    let rerun = run_instrument(&phone, &spec, &mut file_sink);
                    if rerun.verdict == RunVerdict::Completed {
                        round_cases = rerun.cases;
                    }
                }
            }
            for c in &round_cases {
                if matches!(
                    c.verdict,
                    CaseVerdict::ASSERTION_FAIL | CaseVerdict::ENV_BLOCKED | CaseVerdict::TIMEOUT
                ) {
                    let dump = device_log_dump(&phone);
                    let tele = telemetry_diff(&tele_before, &tele_after);
                    match write_crash_bundle(
                        &out_dir,
                        &mname,
                        c,
                        &run.stdout_lines,
                        &dump,
                        win_start,
                        win_end,
                        &tele,
                    ) {
                        Ok(dir) => bundles.push(dir.display().to_string()),
                        Err(e) => eprintln!("  ⚠ crash bundle 打包失败: {e}"),
                    }
                }
            }
            module_cases.extend(round_cases);
        }

        // profile 期望用例对账: 缺测补 NOT_RUN(零用例/漏报不得计为通过)
        if !plan.methods.is_empty() {
            for m in &plan.methods {
                let seen = module_cases.iter().any(|c| &c.id() == m);
                if !seen {
                    module_cases.push(CaseResult {
                        class_name: m.split('#').next().unwrap_or("").to_string(),
                        method: m.split('#').nth(1).unwrap_or(m).to_string(),
                        verdict: CaseVerdict::NOT_RUN,
                        stack: String::new(),
                        stream: "profile 列出的用例未产生运行器结果".into(),
                        raw_lines: vec![],
                        duration_ms: 0,
                    });
                }
            }
        }

        // 零用例红线(同事流程: 零用例明确失败,不得静默通过)——整个模块一条结果
        // 都没产出,说明运行器/设备/安装有环境问题,如实记 ENV_BLOCKED
        if module_cases.is_empty() {
            eprintln!("  ✗ 模块 {mname} 零用例——运行器未产出任何结果,记 ENV_BLOCKED");
            module_cases.push(CaseResult {
                class_name: plan.package.clone(),
                method: "(zero-case run)".into(),
                verdict: CaseVerdict::ENV_BLOCKED,
                stack: String::new(),
                stream: "runner produced no case results (device offline / runner missing / install broken)"
                    .into(),
                raw_lines: vec![],
                duration_ms: 0,
            });
        }

        let wall = mod_t0.elapsed().as_millis() as u64;
        let failed = module_cases.iter().any(|c| c.verdict != CaseVerdict::PASS);
        state.modules.insert(
            mname.clone(),
            if failed {
                "failed".into()
            } else {
                "done".into()
            },
        );
        state.save(&out_dir);

        // 模块 JUnit
        let junit = junit_xml(&mname, &module_cases, wall);
        let junit_path = out_dir.join(format!("junit_{}.xml", sanitize_case_id(&mname)));
        let _ = fs::write(&junit_path, &junit);
        println!(
            "  模块收官: {} 用例 wall={:.1}s → {}",
            module_cases.len(),
            wall as f64 / 1000.0,
            junit_path.display()
        );

        reports.push(ModuleReport {
            module: mname,
            package: plan.package.clone(),
            runner: plan.runner.clone(),
            run_verdict: run_verdict.as_str().to_string(),
            cases: module_cases,
            wall_ms: wall,
            retries_used,
            recovery,
            crash_bundles: bundles,
        });
    }

    // 批次报告
    let wall = t0.elapsed().as_millis() as u64;
    let summary = summary_json(&batch_id, serial.as_deref().unwrap_or(""), &reports, wall);
    let summary_path = out_dir.join("summary.json");
    fs::write(
        &summary_path,
        serde_json::to_string_pretty(&summary).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;

    let v = &summary["verdicts"];
    println!("════ 批次 {batch_id} 收官 ════");
    println!(
        "  用例 {} | PASS {} | ASSERTION_FAIL {} | ENV_BLOCKED {} | TIMEOUT {} | NOT_RUN {}",
        summary["total_cases"],
        v["PASS"],
        v["ASSERTION_FAIL"],
        v["ENV_BLOCKED"],
        v["TIMEOUT"],
        v["NOT_RUN"]
    );
    println!(
        "  恢复状态: {} | 报告: {}",
        summary["recovery"].as_str().unwrap_or("?"),
        summary_path.display()
    );

    let clean = batch_is_clean(&summary);
    Ok(if clean { 0 } else { 1 })
}

// ══════════════ 单测 ══════════════

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RUN: &str = r"INSTRUMENTATION_STATUS: numtests=2
INSTRUMENTATION_STATUS: stream=
android.content.pm.cts.PackageManagerTest:
INSTRUMENTATION_STATUS: id=AndroidJUnitRunner
INSTRUMENTATION_STATUS: test=testGetPackageInfo
INSTRUMENTATION_STATUS: class=android.content.pm.cts.PackageManagerTest
INSTRUMENTATION_STATUS: current=1
INSTRUMENTATION_STATUS_CODE: 1
INSTRUMENTATION_STATUS: stream=.
INSTRUMENTATION_STATUS: id=AndroidJUnitRunner
INSTRUMENTATION_STATUS: test=testGetPackageInfo
INSTRUMENTATION_STATUS: class=android.content.pm.cts.PackageManagerTest
INSTRUMENTATION_STATUS: current=1
INSTRUMENTATION_STATUS_CODE: 0
INSTRUMENTATION_STATUS: numtests=2
INSTRUMENTATION_STATUS: stream=
INSTRUMENTATION_STATUS: id=AndroidJUnitRunner
INSTRUMENTATION_STATUS: test=testGetApplicationInfo
INSTRUMENTATION_STATUS: class=android.content.pm.cts.PackageManagerTest
INSTRUMENTATION_STATUS: current=2
INSTRUMENTATION_STATUS_CODE: 1
INSTRUMENTATION_STATUS: stack=junit.framework.AssertionFailedError: expected:<1> but was:<2>
	at junit.framework.Assert.fail(Assert.java:50)
	at android.content.pm.cts.PackageManagerTest.testGetApplicationInfo(PackageManagerTest.java:99)
INSTRUMENTATION_STATUS: id=AndroidJUnitRunner
INSTRUMENTATION_STATUS: test=testGetApplicationInfo
INSTRUMENTATION_STATUS: class=android.content.pm.cts.PackageManagerTest
INSTRUMENTATION_STATUS: current=2
INSTRUMENTATION_STATUS_CODE: -2
INSTRUMENTATION_RESULT: stream=

Time: 3.141
There was 1 failure:
FAILURES!!!
INSTRUMENTATION_CODE: -1
";

    #[test]
    fn parser_streams_case_results_before_run_end() {
        let mut p = InstrumentParser::new();
        let mut started = 0;
        let mut finished: Vec<CaseResult> = Vec::new();
        let mut run_end = false;
        for line in SAMPLE_RUN.lines() {
            for ev in p.feed_line(line) {
                match ev {
                    ParseEvent::CaseStarted { .. } => started += 1,
                    ParseEvent::CaseFinished(c) => finished.push(c),
                    ParseEvent::RunFinished { code, tail } => {
                        run_end = true;
                        assert_eq!(code, -1);
                        assert!(
                            tail.iter().any(|t| t.contains("FAILURES!!!")),
                            "整轮尾部要留住 FAILURES 原文"
                        );
                    }
                }
            }
            // 流式性: 第一个用例的 PASS 必须在 RunFinished 之前产出
            if !run_end && finished.len() == 1 {
                assert_eq!(finished[0].verdict, CaseVerdict::PASS);
            }
        }
        assert_eq!(started, 2);
        assert_eq!(finished.len(), 2);
        assert_eq!(
            finished[0].class_name,
            "android.content.pm.cts.PackageManagerTest"
        );
        assert_eq!(finished[0].method, "testGetPackageInfo");
        assert_eq!(finished[0].verdict, CaseVerdict::PASS);
        assert_eq!(finished[1].verdict, CaseVerdict::ASSERTION_FAIL);
        assert!(
            finished[1].stack.contains("AssertionFailedError"),
            "失败用例要带 Java 调用栈"
        );
        assert!(
            finished[1].stack.contains("PackageManagerTest.java:99"),
            "多行 stack 续行要拼全"
        );
        assert!(run_end);
    }

    #[test]
    fn parser_marks_skipped_as_not_run_never_pass() {
        // code -3 (ignored/skipped) → NOT_RUN,绝不计 PASS(同事流程: 跳过不算通过)
        let mut p = InstrumentParser::new();
        let mut out = Vec::new();
        for line in [
            "INSTRUMENTATION_STATUS: test=testSkipped",
            "INSTRUMENTATION_STATUS: class=com.x.FooTest",
            "INSTRUMENTATION_STATUS_CODE: 1",
            "INSTRUMENTATION_STATUS_CODE: -3",
        ] {
            out.extend(p.feed_line(line));
        }
        let fin = out.iter().find_map(|e| match e {
            ParseEvent::CaseFinished(c) => Some(c),
            _ => None,
        });
        assert_eq!(fin.map(|c| c.verdict), Some(CaseVerdict::NOT_RUN));
    }

    #[test]
    fn instrument_command_builds_method_slice() {
        let spec = InstrumentSpec {
            package: "android.content.cts".into(),
            runner: "androidx.test.runner.AndroidJUnitRunner".into(),
            class_or_method: Some(
                "android.content.pm.cts.PackageManagerTest#testGetPackageInfo".into(),
            ),
            timeout_ms: 1,
            idle_timeout_ms: 1,
            env_args: vec![("timeout_msec".into(), "30000".into())],
        };
        let cmd = instrument_command(&spec);
        assert!(cmd.starts_with("am instrument -r -w"));
        assert!(cmd.contains("-e timeout_msec 30000"));
        assert!(
            cmd.contains("-e class android.content.pm.cts.PackageManagerTest#testGetPackageInfo")
        );
        assert!(cmd.ends_with(" android.content.cts/androidx.test.runner.AndroidJUnitRunner"));

        // 相对 runner 补包名;无切片时无 -e class
        let spec2 = InstrumentSpec {
            package: "com.x".into(),
            runner: ".MyRunner".into(),
            class_or_method: None,
            timeout_ms: 0,
            idle_timeout_ms: 0,
            env_args: vec![],
        };
        let cmd2 = instrument_command(&spec2);
        assert!(cmd2.ends_with(" com.x/..MyRunner") || cmd2.ends_with(" com.x/.MyRunner"));
        assert!(!cmd2.contains("-e class"));
    }

    #[test]
    fn log_ts_and_window_slice() {
        let dump = "09-08 10:00:00.000  1000  1000 I early: before window\n\
                    09-08 10:00:05.000  1000  1000 E bridge: deadlock detected\n\
                        at com.bridge.Native.wait(Native method)\n\
                    09-08 10:00:20.000  1000  1000 I late: after window\n\
                    no timestamp header line\n";
        // 窗口 10:00:04 ~ 10:00:06, buffer 0 → 只留中间行与其续行
        let s = slice_log_by_window(
            dump,
            (9, 8, 10.0 * 3600.0 + 4.0),
            (9, 8, 10.0 * 3600.0 + 6.0),
            0.0,
        );
        assert!(s.contains("deadlock detected"));
        assert!(s.contains("Native method"), "无时间戳续行跟随归属");
        assert!(!s.contains("before window"));
        assert!(!s.contains("after window"));
        // 解析不出时间戳的全文 → 如实带警告头
        let s2 = slice_log_by_window("garbage\nlines\n", (1, 1, 0.0), (1, 1, 1.0), 0.0);
        assert!(s2.starts_with("(警告: 日志行无标准时间戳"));
        // 窗口内无行 → 如实说明,不空文件冒充
        let s3 = slice_log_by_window(dump, (9, 9, 0.0), (9, 9, 1.0), 0.0);
        assert!(s3.contains("无日志行"));
    }

    #[test]
    fn junit_maps_verdicts_and_never_counts_pass_wrong() {
        let cases = vec![
            CaseResult { class_name: "C".into(), method: "m1".into(), verdict: CaseVerdict::PASS,
                stack: String::new(), stream: String::new(), raw_lines: vec![], duration_ms: 100 },
            CaseResult { class_name: "C".into(), method: "m2".into(), verdict: CaseVerdict::ASSERTION_FAIL,
                stack: "junit.framework.AssertionFailedError: expected:<1> but was:<2>\n\tat C.m2(C.java:1)".into(),
                stream: String::new(), raw_lines: vec![], duration_ms: 200 },
            CaseResult { class_name: "C".into(), method: "m3".into(), verdict: CaseVerdict::TIMEOUT,
                stack: String::new(), stream: "watchdog".into(), raw_lines: vec![], duration_ms: 90000 },
            CaseResult { class_name: "C".into(), method: "m4".into(), verdict: CaseVerdict::NOT_RUN,
                stack: String::new(), stream: String::new(), raw_lines: vec![], duration_ms: 0 },
        ];
        let x = junit_xml("mod", &cases, 1000);
        assert!(x.contains("tests=\"4\""));
        assert!(x.contains("failures=\"1\""));
        assert!(
            x.contains("errors=\"1\""),
            "TIMEOUT 计入 errors 而非 failures"
        );
        assert!(x.contains("skipped=\"1\""), "NOT_RUN 计 skipped,绝不算通过");
        assert!(x.contains("<failure message=\"junit.framework.AssertionFailedError: expected:&lt;1&gt; but was:&lt;2&gt;\">"));
        assert!(x.contains("verdict=\"PASS\""));
        assert!(x.contains("&lt;"), "XML 转义");
    }

    #[test]
    fn summary_uses_colleague_verdict_literals() {
        let m = ModuleReport {
            module: "m".into(),
            package: "p".into(),
            runner: "r".into(),
            run_verdict: "Completed".into(),
            cases: vec![CaseResult {
                class_name: "C".into(),
                method: "m".into(),
                verdict: CaseVerdict::PASS,
                stack: String::new(),
                stream: String::new(),
                raw_lines: vec![],
                duration_ms: 1,
            }],
            wall_ms: 1,
            retries_used: 0,
            recovery: "VERIFIED".into(),
            crash_bundles: vec![],
        };
        let s = summary_json("b1", "ser", &[m], 10);
        for k in [
            "PASS",
            "ASSERTION_FAIL",
            "ENV_BLOCKED",
            "TIMEOUT",
            "NOT_RUN",
        ] {
            assert!(s["verdicts"].get(k).is_some(), "summary 缺判定键 {k}");
        }
        assert_eq!(s["verdicts"]["PASS"], 1);
        assert_eq!(s["recovery"], "VERIFIED");
        // 全 PASS + VERIFIED → 干净(退出码 0)
        assert!(batch_is_clean(&s));
        // 混入任一 ENV_BLOCKED → 不干净(退出码 1),零用例红线同理
        let m2 = ModuleReport {
            module: "m2".into(),
            package: "p".into(),
            runner: "r".into(),
            run_verdict: "Completed".into(),
            cases: vec![CaseResult {
                class_name: "C".into(),
                method: "x".into(),
                verdict: CaseVerdict::ENV_BLOCKED,
                stack: String::new(),
                stream: String::new(),
                raw_lines: vec![],
                duration_ms: 0,
            }],
            wall_ms: 1,
            retries_used: 0,
            recovery: "VERIFIED".into(),
            crash_bundles: vec![],
        };
        assert!(
            !batch_is_clean(&summary_json("b2", "ser", &[m2], 10)),
            "ENV_BLOCKED 必须让批次判不干净(退出码 1)"
        );
    }

    #[test]
    fn pm_instrumentation_and_aapt_parsing() {
        let s = "instrumentation:android.content.cts/androidx.test.runner.AndroidJUnitRunner (target=android.content.cts)\n\
                 instrumentation:com.x.test/com.x.MyRunner (target=com.x)\n";
        let v = parse_instrumentation_list(s);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].1, "androidx.test.runner.AndroidJUnitRunner");
        assert_eq!(v[1].2, "com.x");
        let aapt = "package: name='com.x.test' versionCode='1' versionName='1.0' platformBuildVersionName='16'";
        assert_eq!(parse_aapt_package(aapt).as_deref(), Some("com.x.test"));
    }

    #[test]
    fn module_arg_and_profile_plan() {
        assert_eq!(
            parse_module_arg("android.content.cts/androidx.test.runner.AndroidJUnitRunner"),
            Some((
                "android.content.cts".into(),
                "androidx.test.runner.AndroidJUnitRunner".into()
            ))
        );
        assert!(parse_module_arg("norunner").is_none());
        assert!(parse_module_arg("pkg/").is_none());

        let p: CtsProfile = serde_json::from_str(
            r#"{
            "api": "getPackageInfo",
            "module": "CtsContentTestCases",
            "package": "android.content.cts",
            "runner": "androidx.test.runner.AndroidJUnitRunner",
            "cases": ["android.content.pm.cts.PackageManagerTest#testGetPackageInfo"],
            "expected_count": 1,
            "unknown_future_field": {"nested": true}
        }"#,
        )
        .expect("同事 profile 的未知字段必须容忍");
        let plan = plan_from_profile(&p).unwrap();
        assert_eq!(plan.package, "android.content.cts");
        assert_eq!(plan.methods.len(), 1);
        assert!(plan.methods[0].contains('#'), "Class#method 切片原样保留");

        let env: CtsEnvironment = serde_json::from_str(
            r#"{
            "device": "FMR0223C13000649",
            "runtime_version": "12.0.0",
            "abi": "arm64-v8a",
            "cts_version": "16_r1",
            "host": {"note": "future"}
        }"#,
        )
        .expect("environment.json 未知字段必须容忍");
        assert_eq!(env.device_serial.as_deref(), Some("FMR0223C13000649"));
        assert_eq!(env.abi.as_deref(), Some("arm64-v8a"));
    }

    #[test]
    fn telemetry_diff_reports_deltas() {
        let before = json!({"vm_rss_kb": 1000, "fd_count": 50, "threads": 10});
        let after = json!({"vm_rss_kb": 1400, "fd_count": 57, "threads": null});
        let d = telemetry_diff(&before, &after);
        assert_eq!(d["delta"]["vm_rss_kb"], 400);
        assert_eq!(
            d["delta"]["fd_count"], 7,
            "FD 泄漏差值是 A2OH 死锁排查的关键指标"
        );
        assert!(d["delta"]["threads"].is_null(), "缺测如实 null");
    }

    #[test]
    fn batch_state_roundtrip_and_sanitize() {
        let dir = std::env::temp_dir().join(format!("pf_cts_state_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = BatchState::default();
        st.modules.insert("a/b".into(), "done".into());
        st.save(&dir);
        let loaded = BatchState::load(&dir);
        assert_eq!(loaded.modules.get("a/b").map(String::as_str), Some("done"));
        assert_eq!(sanitize_case_id("com.x.C#m()"), "com.x.C_m__");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
