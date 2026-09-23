//! gpdaemon: 安卓侧的 HiSmartPerf 通路 —— 设备端 `GamePerfToolCollector` 的推送、握手、
//! 命令构造与实时样本解析。
//!
//! **安卓侧的 HiSmartPerf 不是 `SP_daemon`。** 鸿蒙侧是 `hdc shell SP_daemon -N …` 跑完落盘
//! `data.csv` 再拉回来 (见 `smartperf`); 安卓侧完全不同 —— HiSmartPerf-Editor 往手机
//! `/data/local/tmp` 推一个 native 采集器 `GamePerfToolCollector`, 以 `-authorize` 常驻,
//! 它在设备上监听 TCP, 主机 `adb forward` 过去后用一套文本协议实时拉样本。
//! 两条通路除了「都叫 HiSmartPerf」以外没有任何共用代码, 故各占一个模块。
//!
//! **口径的来源** (全部逐条取自本机 HiSmartPerf-Editor 5.1.0.47, 没有一条是凭空构造的):
//!
//! | 东西 | 出处 |
//! |---|---|
//! | `adb -s <S> shell "pidof GamePerfToolCollector"` → `kill -9` | `app.asar` `dist/electron.js` `startCollector` |
//! | `adb -s <S> shell "LD_LIBRARY_PATH=/data/local/tmp/ /data/local/tmp/GamePerfToolCollector -version"` | 同上 (版本比对, 不一致才重推) |
//! | `adb -s <S> shell getprop ro.product.cpu.abi` → 选 `plugins/Device/Android/<abi>/` | 同上 |
//! | `adb -s <S> push <abi>/{GamePerfToolCollector,libQProfilerInterface.so} /data/local/tmp/` | 同上 |
//! | `adb -s <S> shell chmod 777 /data/local/tmp/GamePerfToolCollector` | 同上 |
//! | `adb -s <S> shell "LD_LIBRARY_PATH=/data/local/tmp/ /data/local/tmp/GamePerfToolCollector -authorize"` | 同上 (常驻, 不返回) |
//! | `adb -s <S> forward tcp:<本地> tcp:<20100..20102>` | `dist/umi.js` `DevSocket` 探测 |
//! | 握手 `0012\|authorize:0;` + `cmd=getVersion;end;` | 同上 |
//! | `cmd=getDeviceInfo;end;` / `cmd=selectPid;para=<pid>;end;` | 同上 |
//! | `cmd=startCollect;para=itemMask:<掩码>,package:<包名>,pid:<pid>,realtimeEnable:1,;end;` | 同上 |
//! | `cmd=stopCollect;para=pcTmp;end;` | 同上 |
//! | 样本 `{fps:%d;refresh:%d;gpuFreq:%d;…}` 的字段名与顺序 | `GamePerfToolCollector` 二进制里的格式串 |
//!
//! **设备端到底读的是什么** (`GamePerfToolCollector` 二进制里的路径字面量):
//! 功耗 `/sys/class/power_supply/{battery,Battery}/{current_now,voltage_now}`,
//! 温度 `/sys/class/thermal` 与 `/sys/devices/virtual/thermal`,
//! GPU `/sys/class/kgsl/kgsl-3d0/{gpubusy,gpuclk,devfreq/cur_freq}` (高通) 或
//! `/sys/class/devfreq/gpufreq/*` (ARM/Maleoon), 帧率 `dumpsys SurfaceFlinger --latency "<图层>"`。
//! **这与 `hwcond` 读的是同一批节点** —— 所以安卓两条通路的功耗/温度本该对得上,
//! 对不上就是哪边算错了, 这正是可以交叉验证的地方。
//!
//! **单位口径**: HiSmartPerf 自己把安卓与鸿蒙的字段映到同一组 UI series
//! (`dist/umi.js` 的映射表: `current`↔`currentNow`, `voltage`↔`voltageNow`,
//! `gpuFreq`↔`gpuFrequency`, 温度 `[shellFrame,shellBack,soc,system,cpuTemp,gpuTemp,npuTemp,batTemp]`
//! ↔ `[shell_frame,shell_back,soc_thermal,system_h,cluster0,gpu,npu_thermal,Battery]`),
//! 既然进同一条曲线, 单位必须一致, 故沿用鸿蒙侧官方文档的口径: `current` **mA**、
//! `voltage` **μV**、温度 **摄氏度**。这套折算在真机上与 `hwcond` 直读 sysfs 做过交叉比对
//! (见 `docs/SPEC_PERF_SOURCE.md` 第 7 节), 不是只照文档抄的。
//!
//! **这条通路拿不到逐帧间隔**: 实时样本里只有每秒一个整数 `fps`, 没有鸿蒙侧的
//! `fpsJitters` (逐帧绘制间隔)。故本模块给得出帧率与「每秒帧时」, 给不出**逐帧** p95 ——
//! 那个只能走 ftrace/Vulkan 时间戳。不拿每秒口径的 p95 冒充逐帧 p95, 见 `perfsrc`。

use std::collections::BTreeMap;

// ══════════════ 设备侧路径与常量 ══════════════

/// 采集器在设备上的落脚点 (HiSmartPerf 写死的目录)。
pub const REMOTE_DIR: &str = "/data/local/tmp";
/// 设备端采集器可执行文件名。
pub const COLLECTOR_BIN: &str = "GamePerfToolCollector";
/// 采集器依赖的 GPU 计数器库, 必须和它一起推。
pub const COLLECTOR_LIB: &str = "libQProfilerInterface.so";

/// 控制/实时通道的设备端端口候选。HiSmartPerf 按 20100→20102 逐个探,
/// 第一个应答握手的就是它。(另一组 20103..20105 是第二路 socket, 本模块不用。)
pub const PORT_FIRST: u16 = 20100;
pub const PORT_LAST: u16 = 20102;

/// 握手第一帧。`0012` 是长度前缀, `authorize:0` 是免鉴权档位 —— 原样照发,
/// 这四个字符不是我们能自己算的, 是 HiSmartPerf 写死的。
pub const HANDSHAKE: &str = "0012|authorize:0;";

/// 协议的帧尾。样本正文里本身带 `;`, 所以**只能按 `;end;` 切帧**, 不能按 `;` 切。
pub const FRAME_END: &str = ";end;";

// ══════════════ 采集项掩码 ══════════════

/// `startCollect` 的 `itemMask` 位。取自 `dist/umi.js` 里逐位相加的那段,
/// 位序不可自创 —— 设备端按位判开关。
///
/// 整张表都留着 (哪怕本模块只用其中四位): 少一位就没法判断「这个掩码里有没有混进
/// 会落盘的采集项」, 而 `DEFAULT_MASK` 的单测正是靠 `CPU_TRACE` / `ENGINE` 这些位来钉的。
#[allow(dead_code)]
pub mod item {
    pub const FPS: u32 = 1;
    pub const CPU: u32 = 2;
    pub const GPU: u32 = 4;
    pub const MEMORY: u32 = 8;
    pub const TEMP: u32 = 16;
    pub const POWER: u32 = 32;
    pub const NET: u32 = 64;
    pub const CPU_TRACE: u32 = 128;
    pub const CHIP: u32 = 256;
    pub const ENGINE: u32 = 512;
    pub const CPU_FLAME_GRAPH: u32 = 2048;
}

/// 本模块默认采的四类: 帧率、GPU、温度、功耗 —— 正好对齐 `PerfSnapshot` 要填的字段。
///
/// 不开 `CPU_TRACE` / `CPU_FLAME_GRAPH` / `ENGINE`: 前两个会在设备上拉 perfetto/simpleperf
/// 落盘 (有副作用, 且要 python/ndk), 后一个要游戏自己链 Unity profiler —— 都不是只读采集。
pub const DEFAULT_MASK: u32 = item::FPS | item::GPU | item::TEMP | item::POWER;

// ══════════════ 命令构造 (纯函数) ══════════════

/// `pidof` 掉残留采集器 —— 上一轮没退干净的进程会占着端口, 新的起不来。
pub fn cmd_pidof() -> String {
    format!("pidof {COLLECTOR_BIN}")
}

/// 版本探测。HiSmartPerf 用它和内置版本比对, 不一致才重推。
pub fn cmd_version() -> String {
    format!("LD_LIBRARY_PATH={REMOTE_DIR}/ {REMOTE_DIR}/{COLLECTOR_BIN} -version")
}

/// 常驻采集器。**这条命令不会返回** —— 它在前台跑到被 kill 为止, 必须当后台子进程起。
pub fn cmd_authorize() -> String {
    format!("LD_LIBRARY_PATH={REMOTE_DIR}/ {REMOTE_DIR}/{COLLECTOR_BIN} -authorize")
}

/// 推完要给执行位。HiSmartPerf 用的就是 777。
pub fn cmd_chmod() -> String {
    format!("chmod 777 {REMOTE_DIR}/{COLLECTOR_BIN}")
}

pub fn cmd_get_version() -> String {
    "cmd=getVersion;end;".into()
}

pub fn cmd_get_device_info() -> String {
    "cmd=getDeviceInfo;end;".into()
}

pub fn cmd_select_pid(pid: i64) -> String {
    format!("cmd=selectPid;para={pid};end;")
}

/// `startCollect`。参数顺序与结尾那个多余的逗号都照 HiSmartPerf 原样 ——
/// 设备端是按 `,` 切词元再找 `key:value` 的, 少一个逗号就少一个词元。
pub fn cmd_start_collect(mask: u32, pkg: &str, pid: Option<i64>, realtime: bool) -> String {
    let mut s = format!("cmd=startCollect;para=itemMask:{mask},package:{pkg},");
    if let Some(p) = pid {
        if p >= 0 {
            s.push_str(&format!("pid:{p},"));
        }
    }
    if realtime {
        s.push_str("realtimeEnable:1,");
    }
    s.push_str(FRAME_END);
    s
}

/// `stopCollect`。`pcTmp` 是 HiSmartPerf 写死的落盘目录名 (设备端 `/data/local/tmp/GamePerfReport/pcTmp`)。
pub fn cmd_stop_collect() -> String {
    format!("cmd=stopCollect;para=pcTmp{FRAME_END}")
}

// ══════════════ 应答与样本解析 (纯函数) ══════════════

/// 一条实时样本。字段名原样保留设备端的拼写, **不在这里换算单位** ——
/// 换算与可信度判断在 `perfsrc`, 这里只做「字节 → 数字」。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpSample {
    pub fields: BTreeMap<String, i64>,
}

impl GpSample {
    pub fn get(&self, k: &str) -> Option<i64> {
        self.fields.get(k).copied()
    }
}

/// 按 `;end;` 把收到的字节流切成完整帧, 返回 (帧列表, 尚未收全的残余)。
///
/// 必须留残余: TCP 不保证一次 read 正好是一帧, 半帧直接丢掉就会稳定漏采样。
pub fn split_frames(buf: &str) -> (Vec<String>, String) {
    let mut frames = Vec::new();
    let mut rest = buf;
    while let Some(i) = rest.find(FRAME_END) {
        frames.push(rest[..i].to_string());
        rest = &rest[i + FRAME_END.len()..];
    }
    (frames, rest.to_string())
}

/// 一帧的载荷: `value=<正文>` → `Some(正文)`; `ret=<码>…` → `None` (那是命令回执, 不是数据)。
pub fn frame_payload(frame: &str) -> Option<&str> {
    frame.trim_start().strip_prefix("value=")
}

/// 实时样本正文 `{k:v;k:v;…}` → 数字表。
///
/// 只收**能解析成整数**的键: 设备端在同一条正文里还塞了 `gpuType:arm` 这种字符串值,
/// 把它当 0 收进来就会凭空多出一个「测量值」。解析不了的键直接丢, 不猜。
pub fn parse_sample(payload: &str) -> Option<GpSample> {
    let body = payload.trim();
    let body = body.strip_prefix('{').unwrap_or(body);
    let body = body.strip_suffix('}').unwrap_or(body);
    let mut s = GpSample::default();
    for tok in body.split(';') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let Some((k, v)) = tok.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() {
            continue;
        }
        if let Ok(n) = v.parse::<i64>() {
            s.fields.insert(k.to_string(), n);
        }
    }
    if s.fields.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 字节流 → 样本列表 + 残余。非样本帧 (版本号、命令回执) 静默跳过。
pub fn parse_stream(buf: &str) -> (Vec<GpSample>, String) {
    let (frames, rest) = split_frames(buf);
    let samples = frames
        .iter()
        .filter_map(|f| frame_payload(f))
        .filter(|p| p.trim_start().starts_with('{'))
        .filter_map(parse_sample)
        .collect();
    (samples, rest)
}

/// `getVersion` 的应答里取版本号: `value=v1.28` → `v1.28`。
pub fn parse_version(buf: &str) -> Option<String> {
    let (frames, _) = split_frames(buf);
    frames.iter().find_map(|f| {
        let p = frame_payload(f)?.trim();
        // 设备端偶尔在同一条 read 里先吐半截裸版本号 (日志里见过 `1.26\0`),
        // 只认带 value= 前缀且不是 `{…}` 样本的那帧。
        if p.starts_with('{') || p.is_empty() {
            return None;
        }
        Some(p.trim_end_matches('\0').to_string())
    })
}

/// `getDeviceInfo` 的应答 → 键值表。设备端的格式与样本一致 (`k:v;k:v;`),
/// 但值多是字符串, 故这里按字符串收, 原样进 `meta`, 不解释不换算。
pub fn parse_device_info(payload: &str) -> BTreeMap<String, String> {
    let body = payload.trim();
    let body = body.strip_prefix('{').unwrap_or(body);
    let body = body.strip_suffix('}').unwrap_or(body);
    let mut m = BTreeMap::new();
    for tok in body.split(';') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        if let Some((k, v)) = tok.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && !v.is_empty() {
                m.insert(k.to_string(), v.to_string());
            }
        }
    }
    m
}

// ══════════════ 单位口径 ══════════════

/// 设备端 `current` 的单位是 **mA** (与鸿蒙 `currentNow` 同一条 UI series)。
pub fn current_ma_to_ua(ma: i64) -> i64 {
    ma.saturating_mul(1000)
}

/// 设备端 `voltage` 的单位是 **μV** (与鸿蒙 `voltageNow` 同一条 UI series)。
pub fn voltage_uv(uv: i64) -> i64 {
    uv
}

/// 一条样本 → 瓦特。取绝对值 (放电电流在不同内核里有正有负)。
///
/// 采不到任一半边就是 `None`, **不拿 0 当功率** —— 一个 0.000 W 看着像数字,
/// 其实是「这条轨此刻量不了」。
pub fn sample_watt(s: &GpSample) -> Option<f64> {
    let (c, v) = (s.get("current")?, s.get("voltage")?);
    if c == 0 || v == 0 {
        return None;
    }
    Some((current_ma_to_ua(c) as f64 / 1.0e6).abs() * (voltage_uv(v) as f64 / 1.0e6).abs())
}

/// 温度字段名 → 统一契约里的热区。顺序/对应关系取自 `dist/umi.js` 的安卓↔鸿蒙映射表。
pub const TEMP_SOC: &str = "soc";
pub const TEMP_GPU: &str = "gpuTemp";
pub const TEMP_BATTERY: &str = "batTemp";

/// 一个温度读数是不是「测量结果」。
///
/// `0` 不是 0 摄氏度, 是「这台机器没有这个传感器」—— 鸿蒙侧 `gpu_max_freq,0` 是同一回事。
/// 设备端对缺失的热区照样回 0, 当成摄氏度发出去比不给更糟。
pub fn temp_c(s: &GpSample, key: &str) -> Option<f64> {
    match s.get(key) {
        Some(0) | None => None,
        Some(v) => Some(v as f64),
    }
}

// ══════════════ 设备侧会话 ══════════════

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::device::Device;

/// 本机转发端口的起点。HiSmartPerf 自己用 12348/12349, 这里避开它 ——
/// 万一 HiSmartPerf-Editor 正开着, 抢同一个端口两边都采不成。
pub const LOCAL_PORT_FIRST: u16 = 12448;
/// 最多往后试几个本机端口 (上一轮没撤干净的转发会占住端口)。
pub const LOCAL_PORT_TRIES: u16 = 8;

/// 采集器起来到能应答握手之间的等待上限。
pub const DAEMON_READY_MS: u64 = 12_000;

/// 设备端采集器所在的 abi 目录 → 本机 HiSmartPerf-Editor 里的插件路径。
pub const PLUGIN_DIR: &str =
    "/Applications/HiSmartPerf-Editor.app/Contents/Resources/plugins/Device/Android";

/// 一次采集会话: 推采集器 → 常驻 → 转发 → 握手 → 拉样本 → 收摊。
///
/// **收摊是 `Drop` 里做的**, 不是「记得调 close」: 中途任何一步出错都必须撤掉端口转发、
/// 杀掉设备上的常驻进程。漏一次, 下一轮就会撞上一个占着端口的僵尸采集器,
/// 而症状是「握手超时」, 跟真正的故障长得一模一样。
pub struct Session<'a> {
    phone: &'a Device,
    sock: TcpStream,
    local_port: u16,
    daemon: Option<std::process::Child>,
    /// 设备端采集器版本 (`getVersion` 的应答), 进 `meta`
    pub version: Option<String>,
    /// 未消费完的半帧
    buf: String,
}

/// 设备上有没有这台机器 abi 对应的采集器文件, 没有就从本机 HiSmartPerf-Editor 推一份。
///
/// 与 HiSmartPerf 同一口径: 先比版本, 一致就不动设备; 不一致 (或根本没有) 才推。
/// 不无条件重推 —— 一次 push 是几百 KB 的写盘, 每轮采集都来一遍纯属糟蹋设备。
pub fn ensure_collector(phone: &Device, plugin_dir: &str) -> Result<String, String> {
    let have = phone.shell(&cmd_version(), 10_000).trim().to_string();
    if have.starts_with('v') {
        return Ok(have);
    }
    let abi = phone.shell("getprop ro.product.cpu.abi", 8_000).trim().to_string();
    if abi.is_empty() {
        return Err("读不到 ro.product.cpu.abi, 判不出该推哪个 abi 的采集器".into());
    }
    let dir = std::path::Path::new(plugin_dir).join(&abi);
    let bin = dir.join(COLLECTOR_BIN);
    let lib = dir.join(COLLECTOR_LIB);
    if !bin.exists() || !lib.exists() {
        return Err(format!(
            "本机 HiSmartPerf-Editor 里没有 {abi} 的采集器 ({} / {}); \
安卓侧 HiSmartPerf 通路要靠它推到设备上, 装一个 HiSmartPerf-Editor 再来",
            bin.display(),
            lib.display()
        ));
    }
    if !phone.push_file(&bin.to_string_lossy(), &format!("{REMOTE_DIR}/{COLLECTOR_BIN}")) {
        return Err(format!("推 {COLLECTOR_BIN} 失败"));
    }
    if !phone.push_file(&lib.to_string_lossy(), &format!("{REMOTE_DIR}/{COLLECTOR_LIB}")) {
        return Err(format!("推 {COLLECTOR_LIB} 失败"));
    }
    phone.shell(&cmd_chmod(), 8_000);
    let v = phone.shell(&cmd_version(), 10_000).trim().to_string();
    if v.starts_with('v') {
        Ok(v)
    } else {
        Err(format!("采集器推上去了但跑不起来 (-version 回: {v:?})"))
    }
}

/// 杀掉设备上还活着的采集器。开新会话前必做 —— 旧进程占着端口, 新的起来也没人应答。
pub fn kill_stale(phone: &Device) {
    let pids = phone.shell(&cmd_pidof(), 8_000);
    for pid in pids.split_whitespace() {
        if pid.chars().all(|c| c.is_ascii_digit()) {
            phone.shell(&format!("kill -9 {pid}"), 5_000);
        }
    }
}

impl<'a> Session<'a> {
    /// 开一次会话。`plugin_dir` 单独传是为了让单测能指一个假目录, 不写死本机路径。
    pub fn open(phone: &'a Device, plugin_dir: &str) -> Result<Self, String> {
        let pushed = ensure_collector(phone, plugin_dir)?;
        kill_stale(phone);

        // -authorize 不返回, 必须当后台子进程起; 它的 stdout/stderr 我们不消费,
        // 但管子得留着 —— 直接丢给 null, adb 那头写满了会把采集器卡住。
        let daemon = phone
            .stream_shell(&cmd_authorize())
            .map_err(|e| format!("起不了设备端采集器: {e}"))?;

        // 设备端端口是探出来的 (20100..20102), 本机端口也可能被上一轮的残留转发占着。
        let t0 = Instant::now();
        let mut last = String::from("没有试过任何端口");
        while t0.elapsed() < Duration::from_millis(DAEMON_READY_MS) {
            for remote in PORT_FIRST..=PORT_LAST {
                for off in 0..LOCAL_PORT_TRIES {
                    let local = LOCAL_PORT_FIRST + off;
                    if let Err(e) = phone.forward(local, remote) {
                        last = format!("forward tcp:{local}→tcp:{remote}: {e}");
                        continue;
                    }
                    match Self::handshake(phone, local) {
                        Ok((sock, version)) => {
                            return Ok(Session {
                                phone,
                                sock,
                                local_port: local,
                                daemon: Some(daemon),
                                version: version.or(Some(pushed)),
                                buf: String::new(),
                            })
                        }
                        Err(e) => {
                            phone.forward_remove(local);
                            last = format!("tcp:{remote} 握手: {e}");
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        // 探不通也要把刚起的常驻进程收掉, 否则它会一直挂在设备上
        let mut d = daemon;
        let _ = d.kill();
        let _ = d.wait();
        kill_stale(phone);
        Err(format!("设备端采集器没在 {PORT_FIRST}..={PORT_LAST} 上应答 (最后一次: {last})"))
    }

    fn handshake(_phone: &Device, local: u16) -> Result<(TcpStream, Option<String>), String> {
        let sock = TcpStream::connect(("127.0.0.1", local)).map_err(|e| e.to_string())?;
        sock.set_read_timeout(Some(Duration::from_millis(2_000))).ok();
        sock.set_write_timeout(Some(Duration::from_millis(2_000))).ok();
        let mut s = sock;
        // 顺序照 HiSmartPerf: 先鉴权帧, 紧跟着问版本。少了鉴权帧设备端直接闭嘴。
        s.write_all(HANDSHAKE.as_bytes()).map_err(|e| e.to_string())?;
        s.write_all(cmd_get_version().as_bytes()).map_err(|e| e.to_string())?;
        let mut raw = [0u8; 4096];
        let n = s.read(&mut raw).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("设备端把连接关了".into());
        }
        let text = String::from_utf8_lossy(&raw[..n]).to_string();
        let v = parse_version(&text);
        if v.is_none() && !text.contains("value=") {
            return Err(format!("应答不是这个协议: {text:?}"));
        }
        Ok((s, v))
    }

    fn send(&mut self, cmd: &str) -> Result<(), String> {
        self.sock.write_all(cmd.as_bytes()).map_err(|e| format!("发 {cmd:?} 失败: {e}"))
    }

    /// 设备信息 (进 `meta`, 原样透传)。
    pub fn device_info(&mut self) -> BTreeMap<String, String> {
        if self.send(&cmd_get_device_info()).is_err() {
            return BTreeMap::new();
        }
        let mut raw = [0u8; 8192];
        let Ok(n) = self.sock.read(&mut raw) else { return BTreeMap::new() };
        let text = String::from_utf8_lossy(&raw[..n]);
        let (frames, _) = split_frames(&text);
        frames
            .iter()
            .filter_map(|f| frame_payload(f))
            .map(parse_device_info)
            .find(|m| !m.is_empty())
            .unwrap_or_default()
    }

    /// 开采并收满 `want` 条实时样本 (设备端约每秒一条), 超时就返回已收到的。
    ///
    /// 收不满也照样返回: 上层按 `sample_count` 判, 一条都没有才算这轮没采到。
    /// 这里**不补齐、不插值** —— 没采到的那一秒就是没采到。
    pub fn collect(
        &mut self,
        mask: u32,
        pkg: &str,
        pid: Option<i64>,
        want: usize,
        timeout_ms: u64,
    ) -> Result<Vec<GpSample>, String> {
        if let Some(p) = pid {
            self.send(&cmd_select_pid(p))?;
        }
        self.send(&cmd_start_collect(mask, pkg, pid, true))?;
        let mut out = Vec::new();
        let t0 = Instant::now();
        let mut raw = [0u8; 16384];
        while out.len() < want && t0.elapsed() < Duration::from_millis(timeout_ms) {
            match self.sock.read(&mut raw) {
                Ok(0) => break,
                Ok(n) => {
                    self.buf.push_str(&String::from_utf8_lossy(&raw[..n]));
                    let (mut got, rest) = parse_stream(&self.buf);
                    self.buf = rest;
                    out.append(&mut got);
                }
                // 读超时不是故障: 设备端一秒才吐一条, 空读很正常, 继续等到总超时为止
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) => return Err(format!("读实时样本失败: {e}")),
            }
        }
        let _ = self.send(&cmd_stop_collect());
        out.truncate(want);
        Ok(out)
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
        self.phone.forward_remove(self.local_port);
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        // adb shell 的子进程被杀不代表设备那头也死了 —— 必须显式再杀一次设备侧进程
        kill_stale(self.phone);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 命令构造 ──

    #[test]
    fn start_collect_matches_hismartperf_wire_format() {
        // 逐字对齐 dist/umi.js 拼出来的串 (含结尾那个多余逗号)
        assert_eq!(
            cmd_start_collect(53, "com.miHoYo.Yuanshen", Some(1234), true),
            "cmd=startCollect;para=itemMask:53,package:com.miHoYo.Yuanshen,pid:1234,realtimeEnable:1,;end;"
        );
    }

    #[test]
    fn start_collect_without_pid_omits_the_pid_token() {
        // pid 未知时不能发 `pid:-1` —— 设备端会把它当一个真的 pid 去找
        let s = cmd_start_collect(DEFAULT_MASK, "com.x", None, false);
        assert!(!s.contains("pid:"), "{s}");
        assert!(!s.contains("realtimeEnable"), "{s}");
        assert!(s.ends_with(";end;"), "{s}");
    }

    #[test]
    fn negative_pid_is_dropped_not_sent() {
        let s = cmd_start_collect(1, "com.x", Some(-1), true);
        assert!(!s.contains("pid:"), "{s}");
    }

    #[test]
    fn default_mask_is_fps_gpu_temp_power() {
        assert_eq!(DEFAULT_MASK, 1 + 4 + 16 + 32);
        // 不能顺手开上这三个: 它们都会在设备上落盘或要外部工具链, 不是只读采集
        assert_eq!(DEFAULT_MASK & item::CPU_TRACE, 0);
        assert_eq!(DEFAULT_MASK & item::CPU_FLAME_GRAPH, 0);
        assert_eq!(DEFAULT_MASK & item::ENGINE, 0);
    }

    #[test]
    fn other_commands_are_verbatim() {
        assert_eq!(cmd_get_version(), "cmd=getVersion;end;");
        assert_eq!(cmd_get_device_info(), "cmd=getDeviceInfo;end;");
        assert_eq!(cmd_select_pid(42), "cmd=selectPid;para=42;end;");
        assert_eq!(cmd_stop_collect(), "cmd=stopCollect;para=pcTmp;end;");
        assert_eq!(HANDSHAKE, "0012|authorize:0;");
    }

    // ── 切帧 ──

    #[test]
    fn frames_split_on_end_marker_not_on_semicolon() {
        // 样本正文里全是分号; 按 `;` 切就会把一条样本碎成十几片
        let (f, rest) = split_frames("value={fps:30;refresh:60;};end;value=v1.28;end;");
        assert_eq!(f, vec!["value={fps:30;refresh:60;}", "value=v1.28"]);
        assert_eq!(rest, "");
    }

    #[test]
    fn half_a_frame_is_kept_as_remainder() {
        // TCP 不保证一次 read 正好一帧; 半帧丢掉就是稳定漏采样
        let (f, rest) = split_frames("value={fps:30;};end;value={fps:2");
        assert_eq!(f.len(), 1);
        assert_eq!(rest, "value={fps:2");
        // 续上下一段后应当补齐, 且不丢那一帧
        let (f2, rest2) = split_frames(&format!("{rest}9;}};end;"));
        assert_eq!(f2, vec!["value={fps:29;}"]);
        assert_eq!(rest2, "");
    }

    // ── 样本解析 ──

    fn real_sample() -> &'static str {
        // 字段名与顺序取自 GamePerfToolCollector 二进制里的格式串
        "value={fps:30;refresh:120;gpuFreq:670000000;gpuUsage:74;ddrFreq:2092000;\
current:-1256;voltage:4212000;netTx:0;netRx:0;appCpu:212;cpu0usage:31;cluster0freq:1804800;\
shellFrame:0;shellBack:38;soc:52;system:41;cpuTemp:56;gpuTemp:49;npuTemp:0;batTemp:40;\
gpuType:qualcomm;};end;"
    }

    #[test]
    fn realtime_sample_parses_into_numbers() {
        let (s, rest) = parse_stream(real_sample());
        assert_eq!(rest, "");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].get("fps"), Some(30));
        assert_eq!(s[0].get("refresh"), Some(120));
        assert_eq!(s[0].get("current"), Some(-1256));
        assert_eq!(s[0].get("voltage"), Some(4_212_000));
        assert_eq!(s[0].get("batTemp"), Some(40));
    }

    #[test]
    fn non_numeric_values_are_dropped_not_zeroed() {
        // gpuType:qualcomm 收成 0 就会凭空多出一个「测量值」
        let (s, _) = parse_stream(real_sample());
        assert_eq!(s[0].get("gpuType"), None);
        assert!(!s[0].fields.contains_key("gpuType"));
    }

    #[test]
    fn command_receipts_are_not_mistaken_for_samples() {
        // ret= 是命令回执, 不是数据; 收进来就会虚报 sample_count
        let (s, _) = parse_stream("ret=0;end;ret=-10001;end;value=v1.28;end;");
        assert!(s.is_empty());
    }

    #[test]
    fn version_frame_is_recognised() {
        assert_eq!(parse_version("value=v1.28;end;").as_deref(), Some("v1.28"));
        // 设备端偶尔先吐半截裸版本号 (HiSmartPerf 日志里见过 `1.26\0`), 不能当版本帧
        assert_eq!(parse_version("value=v1.267\0;end;").as_deref(), Some("v1.267"));
        assert_eq!(parse_version("value={fps:30;};end;"), None);
    }

    #[test]
    fn device_info_keeps_string_values_verbatim() {
        let m = parse_device_info("{brand:NUBIA;model:NX809J;cpuCoreNum:8;}");
        assert_eq!(m.get("brand").map(String::as_str), Some("NUBIA"));
        assert_eq!(m.get("cpuCoreNum").map(String::as_str), Some("8"));
    }

    // ── 单位与「0 不是测量值」 ──

    #[test]
    fn watt_uses_ma_times_uv_and_takes_absolute_value() {
        // 放电电流是负的; 要的是功率的大小
        let (s, _) = parse_stream(real_sample());
        let w = sample_watt(&s[0]).unwrap();
        // 1256 mA x 4.212 V = 5.290 W
        assert!((w - 5.290_272).abs() < 1e-6, "{w}");
    }

    #[test]
    fn zero_current_or_voltage_is_not_zero_watts() {
        let mut s = GpSample::default();
        s.fields.insert("current".into(), 0);
        s.fields.insert("voltage".into(), 4_212_000);
        assert_eq!(sample_watt(&s), None);
        s.fields.insert("current".into(), -1000);
        s.fields.insert("voltage".into(), 0);
        assert_eq!(sample_watt(&s), None);
    }

    #[test]
    fn missing_power_fields_give_none_not_zero() {
        assert_eq!(sample_watt(&GpSample::default()), None);
    }

    #[test]
    fn a_zero_thermal_zone_is_a_missing_sensor_not_zero_celsius() {
        let (s, _) = parse_stream(real_sample());
        // 这台机器 npuTemp / shellFrame 回 0 = 没有这个传感器
        assert_eq!(temp_c(&s[0], "npuTemp"), None);
        assert_eq!(temp_c(&s[0], "shellFrame"), None);
        assert_eq!(temp_c(&s[0], TEMP_SOC), Some(52.0));
        assert_eq!(temp_c(&s[0], TEMP_GPU), Some(49.0));
        assert_eq!(temp_c(&s[0], TEMP_BATTERY), Some(40.0));
    }

    #[test]
    fn empty_record_is_not_a_sample() {
        assert_eq!(parse_sample("{}"), None);
        assert_eq!(parse_sample("{;;;}"), None);
    }
}
