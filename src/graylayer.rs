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

const LAYER_NAME: &str = "VK_LAYER_refknobs_readonly";
const LOADOP_PROP: &str = "debug.knobs.loadop"; // 1 = 层把 loadOp LOAD 改写成 DONT_CARE
const DUMP_PROP: &str = "debug.knobs.passdump"; // 1 = 层导出 render pass 形状表
const CP_PROP: &str = "debug.knobs.copyprobe"; // 1 = 1:1 拷贝探针 (隐含 passdump 追踪)
const UPOP_PROP: &str = "debug.knobs.upop"; // 1 = 真超分算子 (gen1_loc3)
const SO: &str = "libVkLayer_refknobs.so";
const STATE: &str = "/data/local/tmp/knobs_layer_state"; // "把 .so 推进了哪个包"

const USAGE: &str = "用法: phonefarm gray-layer {probe|loadop [pkg]|passdump [pkg]|copyprobe [pkg]|upop [pkg]|target <pkg>|status|off} [--serial S] [--keep-prop]";

pub struct GrayLayer {
    serial: String,
}

impl GrayLayer {
    pub fn new(serial: &str) -> Self {
        Self {
            serial: serial.to_string(),
        }
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
        let so_path = layer_so_path();
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
                "su -c 'rm -f /storage/emulated/0/Android/data/{pkg}/files/knobs_layer_out.json /data/data/{pkg}/knobs_layer_out.json /data/local/tmp/knobs_layer_out.{pkg}.json'"
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
        let act = act
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .unwrap_or_default();
        if act.contains('/') {
            let _ = adb(&self.serial, &["shell", &format!("am start -n {act}")], 30);
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

    fn setprops(&self, loadop: &str, dump: &str, cp: &str, upop: &str) {
        self.ashell(
            &format!("su -c 'setprop {LOADOP_PROP} {loadop}; setprop {DUMP_PROP} {dump}; setprop {CP_PROP} {cp}; setprop {UPOP_PROP} {upop}'"),
            15,
        );
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
            other => Err(format!("不认识的挂层模式 {other:?}")),
        }
    }

    pub fn status(&self) -> String {
        let mut l = Vec::new();
        l.push(format!(
            "debug.vulkan.layers = [{}]",
            self.getprop("debug.vulkan.layers")
        ));
        for p in [LOADOP_PROP, DUMP_PROP, CP_PROP, UPOP_PROP] {
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

    /// 摘层并清理: 清属性 → 按 state 精确删 .so 与 marker → 兜底按文件名全盘扫残留
    /// (state 只记得最后一次挂在哪, 手动推过/异常退出留下的副本它不知道; .so 文件名
    /// 是我们独有的, 按名字清不会误伤) → 清掉立项时那套本来就不生效的 settings。
    pub fn off(&self) -> String {
        self.ashell(
            &format!(
                "su -c 'setprop debug.vulkan.layers \"\"; setprop {LOADOP_PROP} 0; setprop {DUMP_PROP} 0; setprop {CP_PROP} 0; setprop {UPOP_PROP} 0'"
            ),
            15,
        );
        let st = Self::adb_stdout(
            &self.ashell(&format!("su -c 'cat {STATE} 2>/dev/null'"), 15),
        )
        .trim()
        .to_string();
        if !st.is_empty() {
            let (pkg, dir) = st.split_once('|').unwrap_or(("", ""));
            self.ashell(
                &format!(
                    "su -c 'rm -f /storage/emulated/0/Android/data/{pkg}/files/knobs_layer_out.json /data/data/{pkg}/knobs_layer_out.json /data/local/tmp/knobs_layer_out.{pkg}.json'"
                ),
                15,
            );
            self.ashell(&format!("su -c 'rm -f \"{dir}/{SO}\"'"), 15);
            if adb_ok(&self.ashell(&format!("su -c 'test -e \"{dir}/{SO}\"'"), 15)) {
                println!("警告: {dir}/{SO} 没删掉");
            } else {
                println!("已删除 {dir}/{SO}");
            }
        }
        let stray = self
            .ashell(
                &format!("su -c 'find /data/app /data/data /data/local/tmp -name \"{SO}\" 2>/dev/null'"),
                60,
            )
            .trim_end_matches("#adb_exit=0")
            .trim()
            .to_string();
        for f in stray.lines() {
            let f = f.trim();
            if f.is_empty() {
                continue;
            }
            if adb_ok(&self.ashell(&format!("su -c 'rm -f \"{f}\"'"), 15)) {
                println!("已清理残留 {f}");
            }
        }
        self.ashell(&format!("su -c 'rm -f {STATE} /data/local/tmp/{SO}'"), 15);
        // 立项时那套 settings 本来就不生效, 但早先版本写过, 一并清干净
        for k in [
            "gpu_debug_layers",
            "gpu_debug_layer_app",
            "gpu_debug_app",
            "enable_gpu_debug_layers",
        ] {
            let _ = adb(
                &self.serial,
                &["shell", &format!("settings delete global {k}")],
                15,
            );
        }
        format!(
            "已摘层。回读:\n  debug.vulkan.layers = [{}]\n  state               = [{}]",
            self.getprop("debug.vulkan.layers"),
            self.ashell(&format!("su -c 'cat {STATE} 2>/dev/null'"), 15)
                .trim_end_matches("#adb_exit=0")
                .trim()
        )
    }
}

const DEV_TMP_SO: &str = "/data/local/tmp/libVkLayer_refknobs.so";

/// 层 .so 的解析: knobs/gray/build/out/ (build_layer.sh 的产物位)。
fn layer_so_path() -> String {
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
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--serial" => serial = it.next().cloned().unwrap_or(serial),
            "--keep-prop" => keep_prop = true,
            other => positional.push(other.to_string()),
        }
    }
    let Some(mode) = positional.first().cloned() else {
        eprintln!("{USAGE}");
        return 2;
    };
    if adb_path().is_empty() {
        eprintln!("找不到 adb");
        return 2;
    }
    let gl = GrayLayer::new(&serial);
    match mode.as_str() {
        "status" => {
            println!("{}", gl.status());
            0
        }
        "off" => {
            println!("{}", gl.off());
            0
        }
        "probe" | "loadop" | "passdump" | "copyprobe" | "upop" | "target" => {
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
