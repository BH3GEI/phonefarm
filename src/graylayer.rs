//! 灰档 Vulkan 层的挂/摘/查 (knobs/gray/enable_layer.sh 的 Rust 移植)。
//!
//! 机制 (2026-09-23 在 NX809J / Android 16 / user 版 + Magisk root 上逐条实测):
//! 生效只需要两件事, 缺一不可:
//!   1. libVkLayer_refknobs.so 放进**目标应用自己的 nativeLibraryDir**
//!      —— 加载器对每个应用都默认搜这个目录, 所以不需要应用 debuggable
//!   2. `setprop debug.vulkan.layers <层名>`
//!
//! 实测**不需要**: settings put global enable_gpu_debug_layers 那一套
//! (框架侧对非 debuggable 应用直接跳过), 以及 ro.debuggable=1。
//!
//! ⚠ debug.vulkan.layers 是**全局**属性, 会误伤别的应用: 属性置上之后启动的、
//! 自己 lib 目录里没有这个 .so 的 Vulkan 应用 vkCreateInstance 直接失败
//! (实测 cn.nubia.gameassist RenderThread SIGABRT 每 ~2 秒崩溃重启一次)。
//! 所以 target/probe/loadop 挂完等目标进程真的加载上层, 然后**立刻清掉全局属性**
//! 收窄暴露窗口; 无论怎么用, 收尾都要跑 [`off`]。

use crate::eval::{adb, adb_ok, adb_path, resolve_path};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;

pub(crate) const LAYER_NAME: &str = "VK_LAYER_refknobs_readonly";
const LOADOP_PROP: &str = "debug.knobs.loadop"; // 1 = 层把 loadOp LOAD 改写成 DONT_CARE
const DUMP_PROP: &str = "debug.knobs.passdump"; // 1 = 层导出 render pass 形状表
const CP_PROP: &str = "debug.knobs.copyprobe"; // 1 = 1:1 拷贝探针 (隐含 passdump 追踪)
const UPOP_PROP: &str = "debug.knobs.upop"; // 1 = 真超分算子 (gen1_loc3)
const NPUPROBE_PROP: &str = "debug.knobs.npuprobe";
/// Android 非只读属性值上限 91 字节 (PROP_VALUE_MAX 92 含结尾 0)。超了 setprop 会失败。
pub(crate) const PROP_VALUE_MAX: usize = 91;
pub(crate) const FA_PROP: &str = "debug.knobs.frameaudit"; // 1 = 一帧步骤清单 (只读, 隐含 passdump)
pub(crate) const FA_TS_PROP: &str = "debug.knobs.frameaudit_ts"; // 1 = 步骤清单另插 GPU 时间戳
pub(crate) const FA_GO_PROP: &str = "debug.knobs.frameaudit_go"; // 改成新编号 = 抓一次
pub(crate) const STOREOP_PROP: &str = "debug.knobs.storeop"; // "<sig>:<i>,..." / "ds" / 0 (#91)
pub(crate) const STOREOP_LIVE_PROP: &str = "debug.knobs.storeop_live"; // 1 = 孪生 pass 现场切换
pub(crate) const STOREOP_ON_PROP: &str = "debug.knobs.storeop_on"; // 现场切换时的开/关
pub(crate) const SO: &str = "libVkLayer_refknobs.so";
pub(crate) const STATE: &str = "/data/local/tmp/knobs_layer_state"; // "把 .so 推进了哪个包"
pub(crate) const LAYER_PROPERTIES: [&str; 12] = [
    "debug.vulkan.layers", LOADOP_PROP, DUMP_PROP, CP_PROP, UPOP_PROP, NPUPROBE_PROP,
    FA_PROP, FA_TS_PROP, FA_GO_PROP, STOREOP_PROP, STOREOP_LIVE_PROP, STOREOP_ON_PROP,
];

const USAGE: &str = "用法: phonefarm gray-layer {probe|loadop [pkg]|passdump [pkg]|copyprobe [pkg]|upop [pkg]|frameaudit [pkg]|frameaudit-ts [pkg]|target <pkg>|status|off [pkg] [--json]} [--serial S] [--keep-prop] [--so 本机.so]";

const DEACTIVATION_SCOPE: &str = "known_target_layer_deactivated";

#[derive(Debug, Serialize)]
pub struct DeactivationCommand {
    pub command: String,
    /// Unmodified transport output, including its exit marker.
    pub output: String,
}

#[derive(Debug, Serialize)]
pub struct DeactivationEvidence {
    pub scope: &'static str,
    pub serial: String,
    pub package: String,
    pub recorded_installation: Option<String>,
    pub recorded_installation_absent: Option<bool>,
    pub state_absent: bool,
    pub processes_absent: bool,
    pub properties_disabled: bool,
    pub original_properties_restored: bool,
    pub unregistered_installations_verified: bool,
    pub game_state_restored: bool,
    pub job_recovery_complete: bool,
    pub commands: Vec<DeactivationCommand>,
}

#[derive(Debug, Serialize)]
pub struct DeactivationFailure {
    pub scope: &'static str,
    pub serial: String,
    pub package: Option<String>,
    pub error: String,
    pub game_state_restored: bool,
    pub job_recovery_complete: bool,
    pub commands: Vec<DeactivationCommand>,
}

impl std::fmt::Display for DeactivationEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: 已核验目标进程停止及灰层属性停用；{}。范围仅为指定目标摘层，不代表游戏状态或整个任务恢复。",
            self.package, if self.recorded_installation_absent == Some(true) {
                "登记的安装文件及 state 已缺席"
            } else { "没有安装登记，未验证未登记的安装文件" })
    }
}
impl std::fmt::Display for DeactivationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "目标灰层停用未获核验: {}", self.error)
    }
}

pub(crate) fn deactivation_json(result: &Result<DeactivationEvidence, DeactivationFailure>) -> Value {
    match result {
        Ok(evidence) => json!({"schema":"phonefarm/gray-layer-deactivation@1", "status":"verified", "evidence":evidence}),
        Err(error) => json!({"schema":"phonefarm/gray-layer-deactivation@1", "status":"failed", "error":error}),
    }
}

pub(crate) fn record_deactivation(
    dir: &Path,
    phase: &str,
    result: &Result<DeactivationEvidence, DeactivationFailure>,
) -> Result<Value, String> {
    if phase.is_empty() || !phase.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
        return Err("invalid deactivation evidence phase".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.canonicalize().map_err(|e| e.to_string())?.join(format!(
        "deactivation-{phase}-{}-{}.json", std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().ok_or("evidence timestamp unavailable")?));
    let mut value = deactivation_json(result);
    value["evidence_path"] = json!(path);
    crate::confirmation_transport::new_file(&path, &serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?)?;
    Ok(value)
}

/// 「去掉多余写回」的属性串: 每项 (pass 特征码前缀, 附件下标) → "<sig>:<i>,…"。
///
/// 特征码前缀要 ≥8 位十六进制 (层只认这么长的前缀, 短了容易撞上别的 pass), 全串不能超过
/// Android 属性值上限 —— 超了 setprop 静默失败, 层读到空串就当成关, 那一臂就成了对照臂。
pub fn storeop_spec(items: &[(String, u32)]) -> Result<String, String> {
    if items.is_empty() {
        return Ok("0".into());
    }
    let mut parts = Vec::new();
    for (sig, att) in items {
        let sig = sig.trim().to_ascii_lowercase();
        if sig.len() < 8 || sig.len() > 16 || !sig.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("pass 特征码前缀要 8–16 位十六进制, 收到 {sig:?}"));
        }
        if *att > 31 {
            return Err(format!("附件下标 {att} 不合理"));
        }
        parts.push(format!("{sig}:{att}"));
    }
    parts.sort();
    parts.dedup();
    let s = parts.join(",");
    if s.len() > PROP_VALUE_MAX {
        return Err(format!(
            "写回改写指定串 {} 字节, 超过属性值上限 {PROP_VALUE_MAX}: 少放几项, 或把特征码前缀截短到 8 位",
            s.len()
        ));
    }
    Ok(s)
}

pub struct GrayLayer {
    serial: String,
    /// 显式指定的层 .so (测未合并的层; 缺省用 knobs/gray/build/out/ 下的产物)
    so: Option<String>,
    /// storeop 改写规则与是否现场切换; None = 显式关掉 (每一档都写一遍, 不留上一轮的残值)
    storeop: Option<(String, bool)>,
    /// 启动目标时附加的 intent extras (例: Megacity 的 `--es scene city_static ...`)。
    /// 层只在挂层后**第一次**启动时载入 (随后全局属性就清了), 所以 extras 必须在这次启动里带上。
    launch_extras: Option<String>,
}

impl GrayLayer {
    pub fn new(serial: &str) -> Self {
        Self {
            serial: serial.to_string(),
            so: None,
            storeop: None,
            launch_extras: None,
        }
    }

    pub fn with_so(mut self, so: Option<String>) -> Self {
        self.so = so;
        self
    }

    pub fn with_launch_extras(mut self, extras: Option<String>) -> Self {
        self.launch_extras = extras.filter(|e| !e.trim().is_empty());
        self
    }

    /// storeop 改写规则 (见层里 parse_storeop_prop); live = 孪生 pass 现场切换, 初始为关
    pub fn with_storeop(mut self, rule: Option<String>, live: bool) -> Self {
        self.storeop = rule.map(|r| (r, live));
        self
    }

    fn ashell(&self, cmd: &str, timeout: u64) -> String {
        adb(&self.serial, &["shell", cmd], timeout)
    }

    /// adb 输出剥掉尾部的 `#adb_exit=N` 标记。**判定空与否必须用这个**:
    /// cat 失败时 stdout 为空但尾部带着 `#adb_exit=1`, 直接判空会误判成
    /// "marker 出现了" (真机实测: 不存在的包被当成层已加载)。
    pub(crate) fn adb_stdout(out: &str) -> &str {
        match out.rfind("#adb_exit=") {
            Some(i) => &out[..i],
            None => out,
        }
    }

    fn getprop(&self, prop: &str) -> String {
        self.ashell(&format!("getprop {prop}"), 15)
            .trim_end_matches("#adb_exit=0")
            .trim()
            .to_string()
    }

    /// 解析一个包的 nativeLibraryDir —— 加载器真正会搜的那个目录。
    /// 加载器搜的是 <legacyNativeLibraryDir>/<abi 子目录>; 没有 arm64 子目录说明
    /// 这个包没有 64 位原生库 —— 如实失败, 不退回一个加载器根本不搜的路径。
    fn native_lib_dir(&self, pkg: &str) -> Result<String, String> {
        let out = self.ashell(
            &format!("dumpsys package {pkg} | grep -m1 legacyNativeLibraryDir"),
            30,
        );
        let legacy = out
            .lines()
            .find_map(|l| l.split_once("legacyNativeLibraryDir=").map(|(_, v)| v.trim().to_string()))
            .unwrap_or_default();
        if legacy.is_empty() {
            return Err(format!("解析不到 {pkg} 的 legacyNativeLibraryDir"));
        }
        let probe = self.ashell(
            &format!("su -c 'test -d \"{legacy}/arm64\"'"),
            15,
        );
        if adb_ok(&probe) {
            Ok(format!("{legacy}/arm64"))
        } else {
            Err(format!(
                "{pkg} 没有 arm64 原生库目录 ({legacy}/arm64 不存在); 本机制要求目标是 arm64-v8a 应用"
            ))
        }
    }

    /// 挂层: 推 .so → 对齐属主/SELinux 上下文 (每步回读核验) → 写 state →
    /// 重启目标 (已在跑的进程不会凭空挂上层) → 清旧 marker → 置属性 (回读核验)。
    fn mount_layer(&self, pkg: &str, keep_prop: bool) -> Result<(), String> {
        let so_path = self.so.clone().unwrap_or_else(layer_so_path);
        let Some(so) = resolve_path(&so_path) else {
            return Err(format!(
                "先跑 knobs/gray/build/build_layer.sh 出 {SO} (按 cwd 与仓库根都找不到: {so_path})"
            ));
        };
        let dir = self.native_lib_dir(pkg)?;
        let push = adb(
            &self.serial,
            &["push", &so.to_string_lossy(), DEV_TMP_SO],
            120,
        );
        if !adb_ok(&push) {
            return Err(format!("push 失败: {push}"));
        }
        // 属主与 SELinux 上下文对齐同目录既有 .so, 否则应用读不到。
        self.ashell(
            &format!(
                "su -c 'cp {DEV_TMP_SO} \"{dir}/\" && chmod 755 \"{dir}/{SO}\" && chown system:system \"{dir}/{SO}\" && chcon u:object_r:apk_data_file:s0 \"{dir}/{SO}\"'"
            ),
            60,
        );
        if !adb_ok(&self.ashell(&format!("su -c 'test -f \"{dir}/{SO}\"'"), 15)) {
            return Err(format!("安装 .so 失败: {dir}/{SO} 不存在"));
        }
        self.ashell(&format!("su -c 'echo \"{pkg}|{dir}\" > {STATE}'"), 15);
        if self
            .ashell(&format!("su -c 'cat {STATE} 2>/dev/null'"), 15)
            .trim_end_matches("#adb_exit=0")
            .trim()
            .is_empty()
        {
            return Err("写 state 失败, off 将无法精确回滚".into());
        }

        // 目标必须重启才会走到加载器; 旧 marker 必须清掉, 否则分不清新旧
        self.ashell(&format!("am force-stop {pkg}"), 30);
        self.ashell(
            &format!(
                "su -c 'rm -f /storage/emulated/0/Android/data/{pkg}/files/knobs_layer_out.json /data/data/{pkg}/knobs_layer_out.json /data/local/tmp/knobs_layer_out.{pkg}.json /storage/emulated/0/Android/data/{pkg}/files/knobs_frameaudit.json /data/data/{pkg}/knobs_frameaudit.json /data/local/tmp/knobs_frameaudit.{pkg}.json'"
            ),
            15,
        );
        self.ashell(&format!("su -c 'setprop debug.vulkan.layers {LAYER_NAME}'"), 15);
        if self.getprop("debug.vulkan.layers") != LAYER_NAME {
            return Err("setprop debug.vulkan.layers 没生效".into());
        }
        self.ashell(&format!("su -c 'ls -laZ \"{dir}/{SO}\"'"), 15);
        println!("已挂层于 {pkg}\n  .so:  {dir}/{SO}\n  prop: debug.vulkan.layers = {LAYER_NAME}");
        if keep_prop {
            println!(
                "  ⚠ --keep-prop: 属性会一直留着。这期间启动的、lib 目录里没有本 .so 的\n     Vulkan 应用可能崩溃重启 (实测 cn.nubia.gameassist 会)。用完务必跑 off。"
            );
        } else {
            println!("  正在启动 {pkg}; 一旦检测到层加载成功就自动清掉全局属性, 收窄误伤窗口。");
            self.launch_pkg(pkg);
            self.wait_and_narrow(pkg);
        }
        println!("查证据:");
        println!("  adb -s {} shell su -c 'cat /storage/emulated/0/Android/data/{pkg}/files/knobs_layer_out.json'", self.serial);
        println!("  adb -s {} shell su -c 'logcat -b main -d' | grep refknobs", self.serial);
        Ok(())
    }

    /// 启动目标应用。用 `am start -n <解析出的 activity>`, **不要用 monkey**:
    /// 无层对照实测 monkey 会把 refbench 的运行搅坏 (注入事件 = 与被测对象无关的扰动源)。
    fn launch_pkg(&self, pkg: &str) {
        let act = self.ashell(&format!("cmd package resolve-activity --brief {pkg}"), 30);
        // 必须先剥掉尾部的 `#adb_exit=N`: 它单独占最后一行, 不剥的话「最后一个非空行」
        // 永远是这个标记而不是 activity, 启动恒失败 (2026-10-11 真机实测)
        let act = Self::adb_stdout(&act)
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .unwrap_or_default();
        if act.contains('/') {
            let cmd = match &self.launch_extras {
                Some(x) => format!("am start -n {act} {x}"),
                None => format!("am start -n {act}"),
            };
            let _ = adb(&self.serial, &["shell", &cmd], 30);
        } else {
            eprintln!("  解析不到 {pkg} 的启动 activity, 请手动启动它");
        }
    }

    /// 等目标进程真的把层加载进去 (marker 出现), 然后立刻清掉全局属性。
    /// 层已在目标进程里, 清属性不影响它; 但新起的应用不会再找这个层。
    fn wait_and_narrow(&self, pkg: &str) {
        for i in 1..=40 {
            let marker = self.ashell(
                &format!(
                    "su -c 'cat /storage/emulated/0/Android/data/{pkg}/files/knobs_layer_out.json 2>/dev/null; cat /data/data/{pkg}/knobs_layer_out.json 2>/dev/null'"
                ),
                30,
            );
            if !Self::adb_stdout(&marker).trim().is_empty() {
                self.ashell("su -c 'setprop debug.vulkan.layers \"\"'", 15);
                println!("  层已加载, 全局属性已清 (属性暴露窗口约 {} 秒)", i * 2);
                println!("  marker: {}", marker.trim());
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        self.ashell("su -c 'setprop debug.vulkan.layers \"\"'", 15);
        println!("  等不到 {pkg} 的 marker; 已保险起见清掉全局属性。");
        println!(
            "  排查: adb -s {} shell su -c 'logcat -b main -d' | grep -iE 'vulkan|refknobs'",
            self.serial
        );
    }

    /// 每一档都把**全部**开关显式写一遍: 上一轮留下的 frameaudit=1 会给别的臂凭空加观测开销。
    fn setprops(&self, loadop: &str, dump: &str, cp: &str, upop: &str) {
        self.setprops_fa(loadop, dump, cp, upop, "0", "0");
    }

    fn setprops_fa(&self, loadop: &str, dump: &str, cp: &str, upop: &str, fa: &str, fa_ts: &str) {
        let (rule, live) = match &self.storeop {
            Some((r, l)) => (r.as_str(), if *l { "1" } else { "0" }),
            None => ("0", "0"),
        };
        self.ashell(
            &format!("su -c 'setprop {LOADOP_PROP} {loadop}; setprop {DUMP_PROP} {dump}; setprop {CP_PROP} {cp}; setprop {UPOP_PROP} {upop}; setprop {FA_PROP} {fa}; setprop {FA_TS_PROP} {fa_ts}; setprop {FA_GO_PROP} 0; setprop {STOREOP_PROP} \"{rule}\"; setprop {STOREOP_LIVE_PROP} {live}; setprop {STOREOP_ON_PROP} 0'"),
            15,
        );
    }


    /// 挂层之后核对 storeop 规则真的写进了属性。写不进 (例如超长) 时层读到的是关,
    /// 那一臂就成了对照臂 —— 拿 A 去和 A 比。
    pub fn require_storeop(&self) -> Result<(), String> {
        match &self.storeop {
            Some((rule, _)) => self.require_prop(STOREOP_PROP, rule, "写回改写"),
            None => Ok(()),
        }
    }

    /// 某个属性档的回读核验: 写不进就拒绝按那一档继续 (静默退化会让自报失真)。
    fn require_prop(&self, prop: &str, want: &str, label: &str) -> Result<(), String> {
        if self.getprop(prop) != want {
            return Err(format!("setprop {prop} 没生效, 拒绝按{label}档继续"));
        }
        Ok(())
    }

    pub fn mount_mode(&self, mode: &str, pkg: &str, keep_prop: bool) -> Result<(), String> {
        match mode {
            "probe" => {
                self.setprops("0", "0", "0", "0");
                self.mount_layer(pkg, keep_prop)
            }
            // 观测档: 只读 + 导出 pass 形状表 (定位"低分辨率->放大"那一步)
            "passdump" => {
                self.setprops("0", "1", "0", "0");
                self.require_prop(DUMP_PROP, "1", "观测")?;
                self.mount_layer(pkg, keep_prop)
            }
            // 改写档: 与 probe 唯一的差别就是这一个属性, A/B 两臂只切它一个。
            // 必须显式关 passdump: 上一轮留下的 1 会给对照组凭空加观测开销
            "loadop" => {
                self.setprops("1", "0", "0", "0");
                self.require_prop(LOADOP_PROP, "1", "改写")?;
                self.mount_layer(pkg, keep_prop)
            }
            "target" => {
                self.setprops("0", "0", "0", "0");
                self.mount_layer(pkg, keep_prop)
            }
            // 拷贝探针档: 1:1 拷贝算子把建 pipeline/建 image/换描述符全走一遍,
            // 画面应逐像素不变。是**改写**, 探的就是反作弊对这种侵入放不放行。
            "copyprobe" => {
                self.setprops("0", "1", "1", "0");
                self.require_prop(CP_PROP, "1", "探针")?;
                self.mount_layer(pkg, keep_prop)
            }
            // 真超分算子档: gen1_loc3 (2x2 bilinear), dst=送显分辨率
            "upop" => {
                self.setprops("0", "1", "0", "1");
                self.require_prop(UPOP_PROP, "1", "算子")?;
                self.mount_layer(pkg, keep_prop)
            }
            // 一帧步骤清单 (只读): 层按提交顺序记 pass/拷贝/清屏/dispatch, 由
            // `debug.knobs.frameaudit_go` 触发抓帧; -ts 档另在 pass 前后写 GPU 时间戳
            "frameaudit" | "frameaudit-ts" => {
                let ts = if mode == "frameaudit-ts" { "1" } else { "0" };
                self.setprops_fa("0", "1", "0", "0", "1", ts);
                self.require_prop(FA_PROP, "1", "步骤清单")?;
                self.require_prop(FA_TS_PROP, ts, "步骤清单时间戳")?;
                self.mount_layer(pkg, keep_prop)
            }
            other => Err(format!("不认识的挂层模式 {other:?}")),
        }
    }

    pub fn status(&self) -> String {
        let mut l = Vec::new();
        l.push(format!(
            "debug.vulkan.layers = [{}]",
            self.getprop("debug.vulkan.layers")
        ));
        for p in [LOADOP_PROP, DUMP_PROP, CP_PROP, UPOP_PROP, FA_PROP, FA_TS_PROP, FA_GO_PROP, STOREOP_PROP, STOREOP_LIVE_PROP, STOREOP_ON_PROP] {
            l.push(format!("{p}  = [{}]", self.getprop(p)));
        }
        let st = Self::adb_stdout(
            &self.ashell(&format!("su -c 'cat {STATE} 2>/dev/null'"), 15),
        )
        .trim()
        .to_string();
        if st.is_empty() {
            l.push("state = <无>  (未施加)".into());
        } else {
            let dir = st.split_once('|').map(|x| x.1).unwrap_or("");
            l.push(format!("state = {st}  (处于施加态)"));
            l.push(self.ashell(&format!("su -c 'ls -laZ \"{dir}/{SO}\" 2>&1'"), 15));
        }
        l.join("\n")
    }

    /// Stop only the validated target and deactivate our properties. This is
    /// deliberately narrower than restoring a game or completing a job lease.
    pub fn off(&self, expected_pkg: Option<&str>) -> Result<DeactivationEvidence, DeactivationFailure> {
        deactivate(&self.serial, expected_pkg, |command| self.ashell(command, 30))
    }

}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn root_command(command: &str) -> String {
    format!("su -c {}", shell_quote(&format!("{command} 2>&1")))
}

pub(crate) fn valid_package(package: &str) -> bool {
    !package.is_empty() && package.split('.').all(|part| {
        !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && part.as_bytes()[0].is_ascii_alphabetic()
    })
}

pub(crate) fn valid_install_dir(dir: &str) -> bool {
    dir.starts_with('/') && dir[1..].split('/').all(|part| {
        !part.is_empty() && part != "." && part != ".."
            && !part.chars().any(|c| c.is_control())
    })
}

fn parse_layer_state(text: &str) -> Result<(String, String), String> {
    let line = text.strip_suffix('\n').unwrap_or(text);
    let (package, dir) = line.split_once('|').ok_or("invalid layer state: expected package|directory")?;
    if !valid_package(package) || !valid_install_dir(dir) || dir.contains('|') || line.contains('\n') {
        return Err("invalid layer state package/directory; preserved".into());
    }
    Ok((package.into(), dir.into()))
}

/// The injected shell keeps the same bounded transport as the existing layer
/// calls; one command journal records both preflight and each checked mutation.
fn deactivate(
    serial: &str,
    expected_pkg: Option<&str>,
    mut shell: impl FnMut(&str) -> String,
) -> Result<DeactivationEvidence, DeactivationFailure> {
    let mut commands = Vec::new();
    let mut package = expected_pkg.map(str::to_owned);
    let result: Result<Option<String>, String> = (|| {
        if expected_pkg.is_some_and(|p| !valid_package(p)) {
            return Err("invalid explicit target package".into());
        }
        let mut run = |command: String| -> Result<String, String> {
            let output = shell(&command);
            let success = adb_ok(&output);
            let body = GrayLayer::adb_stdout(&output).to_owned();
            commands.push(DeactivationCommand { command: command.clone(), output });
            if success { Ok(body) } else { Err(format!("command failed or outcome unknown: {command}")) }
        };
        // Missing state must be proved by a successful parent listing. A failed
        // cat/test cannot distinguish absence from transport or permission loss.
        let listing = run(root_command("ls -1A -- /data/local/tmp"))?;
        let state = if listing.lines().any(|name| name == "knobs_layer_state") {
            Some(parse_layer_state(&run(root_command(&format!("cat {}", shell_quote(STATE))))?)?)
        } else { None };
        if let Some((recorded_package, _)) = &state {
            if expected_pkg.is_some_and(|p| p != recorded_package) {
                return Err("explicit target differs from layer state; no cleanup performed".into());
            }
            package = Some(recorded_package.clone());
        }
        let target = package.as_deref().ok_or("no layer state or explicit target; cannot verify a target process stopped")?;
        let layer = run("getprop debug.vulkan.layers 2>&1".into())?;
        if !matches!(layer.trim(), "" | LAYER_NAME) {
            return Err("debug.vulkan.layers contains a foreign layer; no cleanup performed".into());
        }
        let installation = if let Some((_, dir)) = &state {
            let dump = run(format!("dumpsys package {} 2>&1", shell_quote(target)))?;
            let native: Vec<_> = dump.lines().filter_map(|line| {
                line.trim().strip_prefix("legacyNativeLibraryDir=")
            }).collect();
            if native.len() != 1 || !valid_install_dir(native[0]) || format!("{}/arm64", native[0]) != *dir {
                return Err("recorded installation differs from the current target native library directory; preserved".into());
            }
            run(root_command(&format!("ls -1A -- {}", shell_quote(dir))))?;
            Some(format!("{dir}/{SO}"))
        } else { None };

        // All target/path/global-layer checks precede the first device write.
        run(format!("am force-stop {} 2>&1", shell_quote(target)))?;
        let processes = run(root_command("ps -A -o NAME"))?;
        let mut names = processes.lines().map(str::trim).filter(|line| !line.is_empty());
        if names.next() != Some("NAME") {
            return Err("unrecognized process-name readback".into());
        }
        if names.any(|name| name == target || name.starts_with(&format!("{target}:"))) {
            return Err("target process remains after force-stop".into());
        }
        for property in LAYER_PROPERTIES {
            let value = if property == "debug.vulkan.layers" { "" } else { "0" };
            run(root_command(&format!("setprop {property} {}", shell_quote(value))))?;
            if run(format!("getprop {property} 2>&1"))?.trim() != value {
                return Err(format!("property readback differs after deactivation: {property}"));
            }
        }
        if let Some(path) = &installation {
            run(root_command(&format!("rm -f -- {}", shell_quote(path))))?;
            let dir = &state.as_ref().unwrap().1;
            let listing = run(root_command(&format!("ls -1A -- {}", shell_quote(dir))))?;
            if listing.lines().any(|name| name == SO) {
                return Err("recorded layer installation remains after deletion".into());
            }
            // Preserve the only installation record until earlier checks pass.
            run(root_command(&format!("rm -f -- {}", shell_quote(STATE))))?;
        }
        let listing = run(root_command("ls -1A -- /data/local/tmp"))?;
        if listing.lines().any(|name| name == "knobs_layer_state") {
            return Err("layer state still exists or appeared during deactivation".into());
        }
        Ok(installation)
    })();
    match result {
        Ok(installation) => Ok(DeactivationEvidence {
            scope: DEACTIVATION_SCOPE, serial: serial.into(), package: package.unwrap(),
            recorded_installation_absent: installation.as_ref().map(|_| true),
            recorded_installation: installation, state_absent: true, processes_absent: true,
            properties_disabled: true, original_properties_restored: false,
            unregistered_installations_verified: false, game_state_restored: false,
            job_recovery_complete: false, commands,
        }),
        Err(error) => Err(DeactivationFailure {
            scope: DEACTIVATION_SCOPE, serial: serial.into(), package, error, game_state_restored: false,
            job_recovery_complete: false, commands,
        }),
    }
}

const DEV_TMP_SO: &str = "/data/local/tmp/libVkLayer_refknobs.so";

/// 层 .so 的解析: knobs/gray/build/out/ (build_layer.sh 的产物位)。
pub(crate) fn layer_so_path() -> String {
    match resolve_path("knobs/gray/build/out/libVkLayer_refknobs.so") {
        Some(p) => p.to_string_lossy().into_owned(),
        None => "knobs/gray/build/out/libVkLayer_refknobs.so".into(),
    }
}

pub fn run_gray_layer(args: &[String]) -> i32 {
    let mut positional: Vec<String> = Vec::new();
    let mut serial = std::env::var("REFBENCH_SERIAL")
        .unwrap_or_else(|_| std::env::var("SERIAL").unwrap_or_else(|_| "91253241019A".into()));
    let mut keep_prop = false;
    let mut json_output = false;
    let mut so: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--serial" => serial = it.next().cloned().unwrap_or(serial),
            "--so" => so = it.next().cloned(),
            "--keep-prop" => keep_prop = true,
            "--json" => json_output = true,
            other => positional.push(other.to_string()),
        }
    }
    let Some(mode) = positional.first().cloned() else {
        eprintln!("{USAGE}");
        return 2;
    };
    if mode == "off" && positional.len() > 2 {
        eprintln!("off 只接受一个可选目标包名\n{USAGE}");
        return 2;
    }
    // Status is a read-only plan/status path and does not interrupt the lease owner.
    let _lease = if mode == "status" { None } else {
        match crate::devlease::Lease::target(Some(&serial), "gray-layer") {
            Ok(v) => { serial = v.serial.clone(); Some(v) },
            Err(e) => { eprintln!("{e}"); return 2; }
        }
    };
    if adb_path().is_empty() {
        eprintln!("找不到 adb");
        return 2;
    }
    let gl = GrayLayer::new(&serial).with_so(so);
    match mode.as_str() {
        "status" => {
            println!("{}", gl.status());
            0
        }
        "off" => {
            let result = gl.off(positional.get(1).map(String::as_str));
            if json_output {
                println!("{}", serde_json::to_string_pretty(&deactivation_json(&result)).unwrap());
            } else {
                match &result {
                    Ok(evidence) => println!("{evidence}"),
                    Err(error) => eprintln!("{error}"),
                }
            }
            if result.is_ok() { 0 } else { 1 }
        }
        "probe" | "loadop" | "passdump" | "copyprobe" | "upop" | "frameaudit" | "frameaudit-ts" | "target" => {
            let default_pkg = if mode == "probe" || mode == "loadop" {
                "io.github.hgamey.refbench"
            } else {
                "com.miHoYo.Yuanshen"
            };
            if mode == "target" && positional.len() < 2 {
                eprintln!("target 要给包名");
                return 2;
            }
            let pkg = positional
                .get(1)
                .cloned()
                .unwrap_or_else(|| default_pkg.to_string());
            match gl.mount_mode(&mode, &pkg, keep_prop) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        other => {
            eprintln!("不认识的模式 {other:?}\n{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod launch_tests {
    use super::GrayLayer;

    /// resolve-activity 的真实输出 (2026-10-11 NX809J) 加上 adb() 追加的退出码标记
    #[test]
    fn activity_line_survives_exit_marker() {
        let raw = "priority=0 preferredOrder=0 match=0x108000 specificIndex=-1 isDefault=false\ncom.miHoYo.Yuanshen/com.miHoYo.GetMobileInfo.MainActivity\n#adb_exit=0";
        let act = GrayLayer::adb_stdout(raw).lines().rev().find(|l| !l.trim().is_empty()).unwrap();
        assert_eq!(act.trim(), "com.miHoYo.Yuanshen/com.miHoYo.GetMobileInfo.MainActivity");
    }
}

#[cfg(test)]
mod deactivation_tests {
    use super::*;

    #[test]
    fn state_is_one_target_and_an_absolute_directory() {
        assert_eq!(parse_layer_state("com.example.game|/data/app/~~id/pkg/lib/arm64\n").unwrap(),
            ("com.example.game".into(), "/data/app/~~id/pkg/lib/arm64".into()));
        for invalid in ["", "com.example.game|relative/path", "com.example.game|/data/../app",
            "com.example.game|/data/app\nother|/data/other", "com.example.game;id|/data/app"] {
            assert!(parse_layer_state(invalid).is_err(), "accepted {invalid:?}");
        }
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn failed_read_and_conflicting_preflight_never_start_cleanup() {
        for fault in ["listing", "target", "foreign_layer", "installation"] {
            let result = deactivate("fixture-serial", Some("com.example.game"), |command| {
                let body = if command.contains("ls -1A -- /data/local/tmp") {
                    if fault == "listing" { return "Permission denied\n#adb_exit=1".into(); }
                    "knobs_layer_state\n"
                } else if command.contains("cat ") {
                    if fault == "target" { "com.example.other|/data/app/pkg/lib/arm64\n" }
                    else { "com.example.game|/data/app/pkg/lib/arm64\n" }
                } else if command.starts_with("getprop ") {
                    if fault == "foreign_layer" { "VK_LAYER_other\n" } else { "\n" }
                } else if command.starts_with("dumpsys ") {
                    "legacyNativeLibraryDir=/data/app/new-version/lib\n"
                } else { panic!("unexpected mutation or command after failed preflight: {command}"); };
                format!("{body}#adb_exit=0")
            });
            let failure = result.unwrap_err();
            assert!(!failure.commands.is_empty());
            assert!(failure.commands.iter().all(|c| !c.command.contains("force-stop")
                && !c.command.contains("setprop") && !c.command.contains("rm -f")));
            assert!(!failure.job_recovery_complete);
            if fault == "listing" {
                assert_eq!(failure.commands[0].output, "Permission denied\n#adb_exit=1");
            }
        }
    }
}

#[cfg(test)]
mod storeop_tests {
    use super::storeop_spec;

    #[test]
    fn spec_joins_sorted_unique_items() {
        let s = storeop_spec(&[
            ("15A51A879DF99C32".into(), 2),
            ("9d0b4db80bf552b8".into(), 0),
            ("15a51a879df99c32".into(), 2),
        ])
        .unwrap();
        assert_eq!(s, "15a51a879df99c32:2,9d0b4db80bf552b8:0");
        assert_eq!(storeop_spec(&[]).unwrap(), "0");
    }

    #[test]
    fn short_or_non_hex_sigs_and_overlong_specs_are_refused() {
        assert!(storeop_spec(&[("1234567".into(), 0)]).is_err());
        assert!(storeop_spec(&[("zzzzzzzz".into(), 0)]).is_err());
        let many: Vec<(String, u32)> = (0..6).map(|i| ("0123456789abcdef".to_string(), i)).collect();
        assert!(storeop_spec(&many).unwrap_err().contains("上限"));
    }
}
