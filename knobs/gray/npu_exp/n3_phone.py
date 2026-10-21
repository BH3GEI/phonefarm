# n3_phone.py — 三臂实验的设备操作层 (真机 Phone + 无设备模拟 DryPhone)
#
# 规矩 (BRIEF / ruler CONTEXT 硬规则):
#   - 手机和电脑上不删除、不覆盖任何数据。本模块里没有 rm。
#     要「挪走」的东西 (标记文件、我们放进游戏库目录的 .so) 一律 mv 到本次运行的设备目录
#     /data/local/tmp/n3/<run_id>/ 下面, 新文件名, 不覆盖已有文件。
#   - 不登录: 本模块不点任何东西, 点击由 n3_ui.py 做, 并且只点 OCR 核对过的目标。
#   - 只读的操作 (cat、ls、dumpsys、getprop、logcat -d) 随便用; 写操作只有:
#     setprop debug.*、往游戏库目录 cp 层 .so、fan_enable、亮度模式、停充 (走 charge_suspend.sh)。
#     每一项写之前先把原值记进 state.json, restore 照它还原。
import json
import os
import random
import re
import struct
import subprocess
import time
import urllib.parse
import zlib

PREFS = "/data/data/{pkg}/shared_prefs/{pkg}.v2.playerprefs.xml"
MARKER_DIRS = ["/storage/emulated/0/Android/data/{pkg}/files/knobs_layer_out.json",
               "/data/data/{pkg}/knobs_layer_out.json",
               "/data/local/tmp/knobs_layer_out.{pkg}.json"]
LAYER_SO = "libVkLayer_refknobs.so"
LAYER_NAME = "VK_LAYER_refknobs_readonly"
FAN_NODE = "/sys/kernel/fan/fan_enable"
# 层认的全部开关 (vk_layer_refknobs.cpp 读的属性)。每臂都显式写一遍, 不信上一个人留下的值。
KNOB_PROPS = ["debug.knobs.loadop", "debug.knobs.passdump", "debug.knobs.copyprobe",
              "debug.knobs.upop", "debug.knobs.npuop", "debug.knobs.npuprobe",
              "debug.knobs.storeop", "debug.knobs.frameaudit", "debug.knobs.frameaudit_ts"]
# N1 npuop 的调参属性 (取值见配置 npu.props)。三臂都写同样的值, 只有 npuop 开关不同。
NPU_TUNE_PROPS = ["debug.knobs.npuop_lag", "debug.knobs.npuop_budget_ms", "debug.knobs.npuop_budget_frames",
                  "debug.knobs.npuop_init_frame", "debug.knobs.npuop_zc", "debug.knobs.npuop_ts"]
ALL_PROPS = KNOB_PROPS + NPU_TUNE_PROPS + ["debug.vulkan.layers"]
SELINUX_RULE = "allow appdomain vendor_qdsp_device chr_file { open read ioctl getattr map }"


def q(s):
    """设备命令里的引号: 一律双引号。命令外层是 su -c '...' (单引号), 里面不能再出现单引号;
    shlex.quote 遇到 ~ = 这类字符 (游戏库目录 /data/app/~~xxx==/) 会加单引号, 所以不用它。"""
    assert '"' not in s and "$" not in s and "`" not in s, s
    return '"' + s + '"'



# ───────────────────────── 裸帧 → PNG (给 OCR 用; iq-judge 直接吃裸帧) ─────────────────────────

def raw_dims(d):
    """adb screencap 裸帧: 头 12 或 16 字节 (w,h,fmt[,colorspace]), 之后 RGBA。"""
    if len(d) < 16:
        return None
    w, h, fmt = struct.unpack("<III", d[:12])
    for hdr in (16, 12):
        if len(d) - hdr == w * h * 4:
            return w, h, hdr
    return None


def raw_to_png(raw_path, png_path):
    d = open(raw_path, "rb").read()
    dims = raw_dims(d)
    if not dims:
        return False
    w, h, hdr = dims
    px = d[hdr:hdr + w * h * 4]
    stride = w * 4
    rows = b"".join(b"\x00" + px[y * stride:(y + 1) * stride] for y in range(h))

    def chunk(t, data):
        c = struct.pack(">I", len(data)) + t + data
        return c + struct.pack(">I", zlib.crc32(t + data) & 0xFFFFFFFF)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(rows, 3)) + chunk(b"IEND", b"")
    with open(png_path, "wb") as f:
        f.write(png)
    return True


def prefs_entries(xml_text):
    """原神 playerprefs 里 GENERAL_DATA 的 {entryType: index}。只读解析, 不写回。
    entryType 2 = 渲染精度 (4 = 极高), 1 = 帧率 (10-20 实验的 genshin_prefs.py)。"""
    m = re.search(r'name="GENERAL_DATA">([^<]*)<', xml_text)
    if not m:
        return None
    t = urllib.parse.unquote(m.group(1))
    return {int(a): int(b) for a, b in re.findall(r'entryType\\?":(\d+),\\?"index\\?":(\d+)', t)}


class Phone:
    """真机。所有 adb 调用经过这里, 并逐条写进 cmd.log。"""
    dry = False

    def __init__(self, serial, pkg, run_id, logf):
        self.serial, self.pkg, self.run_id = serial, pkg, run_id
        self.devdir = f"/data/local/tmp/n3/{run_id}"
        self.logf = logf

    # ── 基础 ──
    def _log(self, s):
        with open(self.logf, "a", encoding="utf-8") as f:
            f.write(time.strftime("%H:%M:%S ") + s + "\n")

    def adb(self, args, timeout=60):
        self._log("adb " + " ".join(args))
        try:
            p = subprocess.run(["adb", "-s", self.serial] + args, stdin=subprocess.DEVNULL,
                               capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            return 124, ""
        return p.returncode, p.stdout.decode("utf-8", "replace")

    def sh(self, cmd, timeout=60):
        return self.adb(["shell", cmd], timeout)

    def su(self, cmd, timeout=60, mm=False):
        # 外层单引号: 本模块拼的命令里不出现单引号。mm = Magisk 全局挂载命名空间 (bind mount 要挂在这里,
        # 之后 zygote 新起的游戏进程才看得见; 同 N1 ondevice.sh 的 asu_mm)
        assert "'" not in cmd, cmd
        return self.adb(["shell", f"su {'-mm ' if mm else ''}-c '{cmd}'"], timeout)


    def sleep(self, s):
        time.sleep(s)

    def now(self):
        return time.time()

    # ── 只读查询 ──
    def online(self):
        rc, o = self.adb(["get-state"], 15)
        return rc == 0 and "device" in o

    def root_ok(self):
        return "uid=0" in self.out("id", root=True)

    def info(self):
        lib = self.out(f"dumpsys package {self.pkg} | grep -m1 legacyNativeLibraryDir")
        lib = lib.split("=", 1)[1].strip() + "/arm64" if "=" in lib else ""
        ver = self.out(f"dumpsys package {self.pkg} | grep -m1 versionName")
        return {
            "model": self.out("getprop ro.product.model"),
            "android": self.out("getprop ro.build.version.release"),
            "wm_size": self.out("wm size"),
            "pkg_path": self.out(f"pm path {self.pkg}"),
            "version": ver.split("=", 1)[1].strip() if "=" in ver else "",
            "libdir": lib,
        }

    def batt(self):
        o = self.out("B=/sys/class/power_supply/battery; cat $B/status $B/capacity $B/current_now $B/voltage_now", True)
        p = o.split("\n") + ["", "", "", ""]
        t = self.out("dumpsys battery | grep -m1 temperature")
        num = lambda s: int(s) if re.fullmatch(r"-?\d+", s.strip() or "x") else None
        tc = re.search(r"(\d+)", t)
        return {"status": p[0].strip(), "capacity": num(p[1]), "current_ua": num(p[2]),
                "voltage_uv": num(p[3]), "temp_c": int(tc.group(1)) / 10 if tc else None}

    def soc_max_c(self):
        o = self.out("for z in /sys/class/thermal/thermal_zone*; do t=$(cat $z/type 2>/dev/null); "
                     "case $t in cpu-*|cpullc*|gpuss*) cat $z/temp 2>/dev/null;; esac; done", True)
        xs = [int(x) for x in o.split() if re.fullmatch(r"\d+", x) and 0 < int(x) < 150000]
        return max(xs) / 1000 if xs else None

    def getprop(self, k):
        return self.out(f"getprop {k}")

    def prefs(self):
        return prefs_entries(self.out(f"cat {PREFS.format(pkg=self.pkg)}", True))

    def pidof(self):
        return self.out(f"pidof {self.pkg}")

    def marker(self):
        for p in MARKER_DIRS:
            o = self.out(f"cat {p.format(pkg=self.pkg)} 2>/dev/null", True)
            if o.startswith("{"):
                try:
                    return json.loads(o.splitlines()[0])
                except ValueError:
                    pass
        return None

    def libdir_listing(self, libdir):
        """{文件名: md5}。用来在 restore 时找出「本次运行放进去的文件」并核对原有文件没被改。"""
        o = self.out(f"cd {q(libdir)} && for f in *; do echo \"$f $(md5sum \"$f\" | cut -d\\  -f1)\"; done", True)
        res = {}
        for line in o.splitlines():
            parts = line.rsplit(" ", 1)
            if len(parts) == 2:
                res[parts[0]] = parts[1]
        return res

    def df_data_mb(self):
        o = self.out("df -m /data | tail -1")
        xs = o.split()
        return int(xs[3]) if len(xs) > 3 and xs[3].isdigit() else None

    def fan(self):
        return self.out(f"cat {FAN_NODE} 2>/dev/null", True)

    def brightness_mode(self):
        return self.out("settings get system screen_brightness_mode")

    def logcat_dump(self, host_path, pattern):
        o = self.out(f"logcat -b main,crash -d 2>/dev/null | grep -iE {q(pattern)} | tail -400", True, 60)
        with open(host_path, "w", encoding="utf-8") as f:
            f.write(o + "\n")
        return o

    # ── 写操作 (都有记录、都可还原) ──
    def mkdevdir(self, sub=""):
        self.su(f"mkdir -p {self.devdir}/{sub}")

    def setprop(self, k, v):
        self.su(f"setprop {k} {q(v) if v else chr(34)*2}")
        return self.getprop(k) == v

    def fan_set(self, v):
        self.su(f"echo {v} > {FAN_NODE}")
        return self.fan() == v

    def brightness_mode_set(self, v):
        self.sh(f"settings put system screen_brightness_mode {v}")

    def push(self, local, remote):
        rc, _ = self.adb(["push", local, remote], 300)
        return rc == 0

    def pull(self, remote, local):
        rc, _ = self.adb(["pull", remote, local], 300)
        return rc == 0 and os.path.exists(local)

    # ── 层与 NPU 依赖: 照 N1 ondevice.sh 的做法, 游戏库目录一字不动 ──
    # 在 <devdir>/libdir 拼「原库目录副本 + 层 + NPU 运行库 + 模型 + 从 /vendor 复制的 libcdsprpc 及依赖」,
    # 再 bind mount 盖到游戏库目录上; 还原 = umount, 原目录自动回来。副本留在手机上不删。
    def is_mountpoint(self, libdir):
        return self.out(f"grep -q {q(' ' + libdir + ' ')} /proc/mounts && echo m", True, mm=True) == "m"

    def out(self, cmd, root=False, timeout=60, mm=False):
        rc, o = (self.su(cmd, timeout, mm) if root else self.sh(cmd, timeout))
        return o.replace("\r", "").strip()

    def stage_and_mount(self, libdir, local_files, model, vendor_libs, log):
        """回 (ok, why)。任何一步失败都不挂载。"""
        if self.is_mountpoint(libdir):
            return False, f"{libdir} 已经是挂载点 (上一次没还原?)"
        if self.out("command -v magiskpolicy", True) == "":
            return False, "没有 magiskpolicy"
        lib_kb = self.out(f"du -sk {q(libdir)} | cut -f1", True)
        free_kb = (self.out("df -k /data | tail -1", True).split() + ["?"] * 4)[3]
        st, push = f"{self.devdir}/libdir", f"{self.devdir}/push"
        self.su(f"mkdir -p {push} {st} && chmod 777 {push}")
        pre = self.out(f"ls {q(libdir)}", True).split()
        names = [os.path.basename(f) for f in local_files] + ["libnpusr_model.so"] + vendor_libs
        clash = [n for n in names if n in pre]
        if clash:
            return False, f"游戏库目录里已有同名文件 {clash}, 不覆盖"
        for f in local_files:
            if not self.push(f, f"{push}/{os.path.basename(f)}"):
                return False, f"push 失败 {f}"
        if not self.push(model, f"{push}/libnpusr_model.so"):
            return False, "push 模型失败"
        lab_dir = (self.out(f"ls -Zd {q(libdir)}", True).split() or [""])[0]
        lab_file = ""
        for line in self.out(f"ls -Z {q(libdir)}", True).splitlines():
            if len(line.split()) >= 2:
                lab_file = line.split()[0]
                break
        self.su(f"cp -a {q(libdir)}/. {st}/ && cp {push}/*.so {st}/", 300)
        for n in vendor_libs:
            if self.out(f"cp /vendor/lib64/{n} {st}/{n} && echo ok", True) != "ok":
                return False, f"复制 /vendor/lib64/{n} 失败"
        self.su(f"chown -R system:system {st} && chmod 755 {st} && chmod 755 {st}/* && chcon {lab_dir} {st} && chcon {lab_file} {st}/*")
        self.su(f"magiskpolicy --live {q(SELINUX_RULE)}")
        self.su(f"mount --bind {st} {q(libdir)}", mm=True)
        if LAYER_SO not in self.out(f"ls {q(libdir)}", True).split():
            return False, "bind mount 后库目录里看不到层"
        log(f"  已 bind mount {st} → {libdir} (库目录 {lib_kb} KB, /data 剩 {free_kb} KB)")
        return True, ""

    def umount_libdir(self, libdir):
        for _ in range(3):
            if not self.is_mountpoint(libdir):
                break
            self.su(f"umount {q(libdir)}", mm=True)
        return not self.is_mountpoint(libdir)

    def park(self, path, tag):
        """把一个文件挪到本次运行的设备目录 (mv, 不删)。目标名带时间戳, 不覆盖。"""
        self.mkdevdir("parked")
        base = os.path.basename(path)
        dst = f"{self.devdir}/parked/{tag}_{int(time.time()*1000)}_{base}"
        self.su(f"mv {q(path)} {q(dst)}")
        return dst if self.out(f"test -e {q(dst)} && echo yes", True) == "yes" else None

    def npu_csv(self):
        """knobs_npuop_frames.csv (层只追加写) 的当前全文, 只读。"""
        return self.out(f"cat /storage/emulated/0/Android/data/{self.pkg}/files/knobs_npuop_frames.csv 2>/dev/null", True, 120)

    def archive_marker(self, tag):
        """挂层前把上一个标记挪走 (否则分不清新旧); 挪到 parked, 不删。"""
        got = []
        for p in MARKER_DIRS:
            p = p.format(pkg=self.pkg)
            if self.out(f"test -e {q(p)} && echo yes", True) == "yes":
                got.append(self.park(p, f"marker_{tag}"))
        return got

    def launch(self):
        act = self.out(f"cmd package resolve-activity --brief {self.pkg} | tail -1")
        if "/" not in act:
            return False
        self.sh(f"am start -n {act}")
        return True

    def force_stop(self):
        self.sh(f"am force-stop {self.pkg}")

    def screencap(self, host_path):
        try:
            with open(host_path, "wb") as f:
                p = subprocess.run(["adb", "-s", self.serial, "exec-out", "screencap"],
                                   stdout=f, stdin=subprocess.DEVNULL, timeout=30)
        except subprocess.TimeoutExpired:
            return False
        self._log(f"screencap -> {host_path}")
        return p.returncode == 0 and raw_dims(open(host_path, "rb").read()) is not None

    def wake(self):
        """等冷时游戏是停着的, 屏幕可能超时灭掉: 亮屏 + 收起锁屏 (研究机没设锁屏密码; 有密码时这里不解锁, 后面进门会失败并停)。"""
        self.sh("input keyevent 224; wm dismiss-keyguard")

    def brightness(self):
        return self.out("settings get system screen_brightness")

    def tap(self, x, y):
        self.sh(f"input tap {int(x)} {int(y)}")

    def key(self, k):
        self.sh(f"input keyevent {k}")

    def swipe(self, x1, y1, x2, y2, ms=400):
        self.sh(f"input swipe {int(x1)} {int(y1)} {int(x2)} {int(y2)} {ms}")

    def charge(self, sub, chain_dev):
        return self.out(f"sh {chain_dev}/charge_suspend.sh {sub}", True, 200)

    def charge_state_file(self):
        return self.out("cat /data/local/tmp/loop_v1_charge.state 2>/dev/null", True)

    def input_suspend_nodes(self):
        out = []
        for n in ["/sys/class/power_supply/battery/input_suspend", "/sys/class/power_supply/usb/input_suspend"]:
            v = self.out(f"cat {n} 2>/dev/null", True)
            if v != "":
                out.append((n, v))
        return out

    def write_node(self, node, v):
        self.su(f"echo {v} > {node}")
        return self.out(f"cat {node}", True) == str(v)

    # ── 一臂的测量: 功耗采样与 ftrace 并行, 采样器在 trace 文件出现时停 (fix-C) ──
    def measure(self, chain_dev, seconds, arm_dir, label):
        tr = f"{self.devdir}/trace_{label}.txt"   # 每臂新文件名, 采完留在设备上 (不删), 拉一份回来
        env_path = os.path.join(arm_dir, "env.txt")
        self._log(f"measure {label} {seconds}s")
        with open(env_path, "wb") as envf:
            sampler = subprocess.Popen(
                ["adb", "-s", self.serial, "shell", f"su -c 'sh {chain_dev}/sample_env.sh {seconds + 20} 2 {tr}'"],
                stdout=envf, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
            rc, cap = self.su(f"sh {chain_dev}/ftrace_capture.sh {seconds} {tr} {self.pkg}", seconds + 120)
            try:
                sampler.wait(timeout=90)
            except subprocess.TimeoutExpired:
                sampler.kill()
        with open(os.path.join(arm_dir, "capture.log"), "w") as f:
            f.write(cap)
        ok = self.pull(tr, os.path.join(arm_dir, "trace.txt"))
        return ok


# ═══════════════════════ 无设备模拟 (dry-run 用) ═══════════════════════
#
# 不追求逼真, 只为把编排逻辑、文件落盘、判定链 (真的 phonefarm 二进制) 整条走一遍。
# 模拟的效应: B 臂功耗比 C 低 DRY_EFFECT_W (默认 0.30 W), A 臂低 0.35 W; 帧时都在 33.3 ms 附近。

class DryPhone(Phone):
    dry = True
    W, H = 2688, 1216

    def __init__(self, serial, pkg, run_id, logf, seed=1):
        super().__init__(serial, pkg, run_id, logf)
        self.rng = random.Random(seed)
        self.t = 1_000_000.0
        self.props = {"debug.vulkan.layers": ""}
        self.prefs_ = {18: 4, 1: 2, 11: 3, 2: 4}      # 与 10-20 实验存下的原始 prefs 相同
        self.applied = dict(self.prefs_)             # 本次进程启动时生效的设置
        self.running = False
        self.layer_loaded = False
        self.npu_active = False
        self.frames = 0
        self.ui = "home"
        self.cap = 82
        self.temp = 31.0
        self.soc = 40.0
        self.libdir_files = {f"lib{i:02d}.so": f"{i:032x}" for i in range(32)}
        self.charge_state = ""
        self.fan_v = "0"
        self.bmode = "1"
        self.uptime = 5000.0
        self.effect = float(os.environ.get("DRY_EFFECT_W", "0.30"))

    def adb(self, args, timeout=60):
        self._log("[dry] adb " + " ".join(args))
        return 0, ""

    def sleep(self, s):
        self.t += s
        self.uptime += s
        if self.running:
            self.frames += int(30 * s)
            self.temp = min(42.0, self.temp + 0.010 * s)
            self.soc = min(60.0, self.soc + 0.05 * s)
            self.cap = max(0, self.cap - s / 900)
        else:
            self.temp = max(30.0, self.temp - 0.015 * s)
            self.soc = max(38.0, self.soc - 0.08 * s)
            self.cap = max(0, self.cap - s / 4000)

    def now(self):
        return self.t

    def online(self):
        return True

    def root_ok(self):
        return True

    def info(self):
        return {"model": "NX809J(dry)", "android": "16", "wm_size": f"Physical size: {self.H}x{self.W}",
                "pkg_path": "package:/data/app/x/base.apk", "version": "7.1.0(dry)",
                "libdir": "/data/app/~~dry/com.miHoYo.Yuanshen-dry/lib/arm64"}

    def batt(self):
        charging = self.charge_state == ""
        return {"status": "Charging" if charging else "Discharging", "capacity": int(self.cap),
                "current_ua": 300000 if charging else -900000, "voltage_uv": 4100000,
                "temp_c": round(self.temp, 1)}

    def soc_max_c(self):
        return round(self.soc, 1)

    def getprop(self, k):
        return self.props.get(k, "")

    def setprop(self, k, v):
        self.props[k] = v
        return True

    def prefs(self):
        return dict(self.prefs_)

    def pidof(self):
        return "12345" if self.running else ""

    def _low(self):
        return self.applied.get(2) != 4

    def marker(self):
        if not (self.running and self.layer_loaded):
            return None
        low = self._low()
        scene = (1134, 514) if low else (2140, 968)
        tbl = [{"w": scene[0], "h": scene[1], "att": 4, "fmt0": 97, "begins": self.frames, "frames": self.frames, "draws": 170},
               {"w": scene[0], "h": scene[1], "att": 1, "fmt0": 37, "begins": self.frames, "frames": self.frames, "draws": 1},
               {"w": 2141, "h": 969, "att": 2, "fmt0": 37, "begins": self.frames, "frames": self.frames, "draws": 56}]
        m = {"knob": "gray_readonly_probe", "layer_loaded": True, "pkg": self.pkg,
             "readonly_stats": {"frames": self.frames, "render_pass_begins": self.frames * 40, "render_passes_created": 60},
             "swapchain": {"w": 2141, "h": 969, "fmt": 37}, "pass_table": tbl}
        if getattr(self, "npu_prop", False):
            ok = self.frames - 300 if (self.npu_active and self.frames > 300) else 0
            m["npuop"] = {"phase": "ready" if self.npu_active else "disabled", "reason": None if self.npu_active else "dry: 不是低档",
                          "counts": {"npu_ok": ok, "bilinear": self.frames - ok, "npu_fail": 0, "over_budget": 0},
                          "timing_ms": {"npu_cpu": {"p50": 2.8}, "pack_gpu": {"p50": 0.6}, "unpack_gpu": {"p50": 1.2}},
                          "notes": []}
        return m

    def libdir_listing(self, libdir):
        return dict(self.libdir_files)

    def df_data_mb(self):
        return 50000

    def fan(self):
        return self.fan_v

    def fan_set(self, v):
        self.fan_v = v
        return True

    def brightness_mode(self):
        return self.bmode

    def brightness_mode_set(self, v):
        self.bmode = v

    def wake(self):
        pass

    def brightness(self):
        return "128"

    def logcat_dump(self, host_path, pattern):
        txt = "refknobs: npuop frame 900 pack 0.61 npu 2.79 unpack 1.18 ms\n" if self.npu_active else ""
        open(host_path, "w").write(txt)
        return txt

    def stage_and_mount(self, libdir, local_files, model, vendor_libs, log):
        self.mounted = True
        log(f"  [dry] bind mount 拼好的库目录 → {libdir}")
        return True, ""

    def umount_libdir(self, libdir):
        self.mounted = False
        return True

    def is_mountpoint(self, libdir):
        return getattr(self, "mounted", False)

    def npu_csv(self):
        return "# pid,frame,pack_gpu_ms,wait_cpu_ms,npu_cpu_ms,unpack_gpu_ms\n12345,900,0.6,0.1,2.8,1.2\n" if self.npu_active else ""

    def park(self, path, tag):
        name = os.path.basename(path)
        self.libdir_files.pop(name, None)
        return f"{self.devdir}/parked/{tag}_{name}"

    def archive_marker(self, tag):
        return []

    def launch(self):
        self.running = True
        self.frames = 0
        self.applied = dict(self.prefs_)
        self.layer_loaded = self.props.get("debug.vulkan.layers") == LAYER_NAME
        self.npu_prop = self.layer_loaded and self.props.get("debug.knobs.npuop") == "1"
        # DRY_SCENARIO=fallback: 模拟层回退到双线性 (B 臂必须判「未按设计施加」, 不能判「NPU 没用」)
        self.npu_active = self.npu_prop and self._low() and os.environ.get("DRY_SCENARIO") != "fallback"
        self.ui = "gate"
        return True

    def force_stop(self):
        self.running = False
        self.layer_loaded = False
        self.ui = "home"

    def charge(self, sub, chain_dev):
        if sub == "suspend":
            if self.cap >= 100:
                self.charge_state = "x\n1\nbypass"
                return "CHARGE_SUSPENDED_BYPASS (dry)"
            self.charge_state = "/sys/class/power_supply/battery/input_suspend\n0\nbattery"
            return "CHARGE_SUSPENDED /sys/class/power_supply/battery/input_suspend: 0 -> 1 (dry)"
        if sub == "restore":
            self.charge_state = ""
            return "CHARGE_RESTORED (dry)"
        return "state: (dry)"

    def charge_state_file(self):
        return self.charge_state

    def input_suspend_nodes(self):
        return [("/sys/class/power_supply/battery/input_suspend", "0")]

    def write_node(self, node, v):
        return True

    # ── UI 模拟: 只够 n3_ui 的流程跑通 ──
    def screencap(self, host_path):
        # 小尺寸合成画面 (纹理 + 少量噪声), 只为让 iq-judge 真读一遍文件, 不追求逼真
        w, h = 320, 160
        rng = self.rng
        px = bytearray()
        for y in range(h):
            for x in range(w):
                v = (x * 7 + y * 13) % 256
                n = rng.randint(-1, 1)
                px += bytes((max(0, min(255, v + n)), (v * 3) % 256, (255 - v), 255))
        open(host_path, "wb").write(struct.pack("<IIII", w, h, 1, 0) + bytes(px))
        return True

    def ocr_items(self):
        W, H = self.W, self.H
        box = lambda t, x, y: {"t": t, "b": [x - 40, y - 20, x + 40, y + 20]}
        if self.ui == "gate":
            if os.environ.get("DRY_SCENARIO") == "login":   # 模拟「要登录」: 脚本必须停, 不点
                return [box("账号密码登录", W // 2, H // 2), box("进入游戏", W // 2, int(H * 0.7))]
            return [box("点击进入", W // 2, int(H * 0.8))]
        if self.ui == "paimon":
            return [box("冒险等阶", 600, 300), box("世界等级", 600, 360), box("设置", 120, 900)]
        if self.ui in ("settings", "graphics"):
            items = [box("图像", 200, 300), box("音频", 200, 400), box("控制", 200, 500)]
            if self.ui == "graphics":
                r = {4: "极高", 3: "高", 2: "中", 1: "低", 0: "最低"}[self.prefs_[2]]
                f = {1: "30", 2: "60", 3: "45"}.get(self.prefs_[1], "60")
                items += [box("帧率", 900, 420), box(f, 1900, 420), box("渲染精度", 900, 560), box(r, 1900, 560)]
                if getattr(self, "dropdown", None):
                    row, opts = self.dropdown
                    items += [box(o, 1900, 650 + 70 * i) for i, o in enumerate(opts)]
            return items
        if self.ui == "exit_dialog":
            return [box("是否退出游戏", W // 2, H // 2 - 100), box("取消", W // 2 - 300, H // 2 + 150),
                    box("确认", W // 2 + 300, H // 2 + 150)]
        return []

    def tap(self, x, y):
        hit = [i["t"] for i in self.ocr_items() if i["b"][0] <= x <= i["b"][2] and i["b"][1] <= y <= i["b"][3]]
        t = hit[0] if hit else None
        if self.ui == "gate" and t == "点击进入":
            self.ui = "world"
        elif self.ui == "world" and abs(x - 190) < 60 and abs(y - 90) < 60:
            self.ui = "paimon"
        elif self.ui == "paimon" and t == "设置":
            self.ui = "settings"
        elif self.ui in ("settings", "graphics") and t == "图像":
            self.ui = "graphics"
        elif self.ui == "graphics":
            dd = getattr(self, "dropdown", None)
            if dd and t in dd[1] and y > 600:
                row = dd[0]
                if row == "渲染精度":
                    self.prefs_[2] = {"极高": 4, "高": 3, "中": 2, "低": 1, "最低": 0}[t]
                else:
                    self.prefs_[1] = {"30": 1, "60": 2, "45": 3}[t]
                self.dropdown = None
                if os.environ.get("DRY_SCENARIO") == "live":   # 模拟「设置现场生效」
                    self.applied = dict(self.prefs_)
                    self.npu_active = getattr(self, "npu_prop", False) and self._low()
            elif t in ("极高", "高", "中", "低", "最低") and abs(y - 560) < 30:
                self.dropdown = ("渲染精度", ["最低", "低", "中", "高", "极高"])
            elif t in ("30", "45", "60") and abs(y - 420) < 30:
                self.dropdown = ("帧率", ["30", "45", "60"])
        elif self.ui == "exit_dialog" and t == "确认":
            self.force_stop()
        elif self.ui == "exit_dialog" and t == "取消":
            self.ui = "world"

    def key(self, k):
        if k in ("BACK", "4"):
            self.ui = {"graphics": "paimon", "settings": "paimon", "paimon": "world",
                       "world": "exit_dialog"}.get(self.ui, self.ui)

    def swipe(self, *a, **k):
        pass

    def measure(self, chain_dev, seconds, arm_dir, label):
        """合成一份 ftrace 文本与 sample_env 输出, 让 parse-trace / env_stats_window 真跑一遍。"""
        rng = self.rng
        arm = "B" if self.npu_active else ("A" if self._low() else "C")
        start = self.uptime + 1.0
        end = start + seconds
        pid, tid = 12345, 12400
        lines = [f"#loop_v1_meta capture_start_uptime={start:.2f} capture_end_uptime={end:.2f} duration_req={seconds}",
                 f"#loop_v1_meta target_pkg={self.pkg} target_pid={pid} target_pid_start={pid} target_tids={pid},{tid}"]
        t, ts = start, 1
        while t < end:
            dt = 1 / 30 + rng.gauss(0, 0.0015) + (0.012 if rng.random() < 0.03 else 0)
            for k in range(2):  # 每帧 2 次提交
                lines.append(f"  UnityGfxDeviceW-{tid}   [001] ..... {t + k * 0.004:.6f}: adreno_cmdbatch_submitted: "
                             f"ctx=7 ctx_prio=1 ts={ts} inflight=1 flags=0x0 prio=1")
                ts += 1
            t += dt
        open(os.path.join(arm_dir, "trace.txt"), "w").write("\n".join(lines) + "\n")
        base = 3.40 - (self.effect if arm == "B" else 0.35 if arm == "A" else 0.0)
        env = ["#sample_env v2 dur=80 interval=2 power_interval=0.2 stop=x", "#battery_status=Discharging",
               f"#battery_capacity={int(self.cap)}", "#charge_suspended=yes", "#charge_mode=battery", "#fan_state=fan_enable=0"]
        u = start - 2
        k = 0
        while u < end + 1:
            w = base + rng.gauss(0, 0.25)
            ua = -int(w / 4.0 * 1e6)
            if k % 10 == 0:
                env.append(f"ENV {u:.2f} 4000000 {ua} NA 0 0 cpu-1-0 {int(self.soc*1000)}")
            else:
                env.append(f"PWR {u:.2f} 4000000 {ua} NA 0 0")
            u += 0.2
            k += 1
        env.append("#sample_env done")
        open(os.path.join(arm_dir, "env.txt"), "w").write("\n".join(env) + "\n")
        open(os.path.join(arm_dir, "capture.log"), "w").write("CAPTURE_OK (dry)\n")
        self.sleep(seconds + 5)
        return True
