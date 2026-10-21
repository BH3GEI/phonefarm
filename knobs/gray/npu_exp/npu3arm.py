#!/usr/bin/env python3
# npu3arm.py — 原神 + 红魔: NPU 超分三臂对照实验 (N3)
#
# 三臂 (同一站位、同一镜头、游戏内帧率 30):
#   C 原生     : 渲染精度原始档 (应为「极高」, 场景 pass 2140×968), 游戏自己的路径
#   A 低+自带  : 渲染精度「低」(场景 pass 1134×514), 游戏自带放大
#   B 低+NPU   : 渲染精度「低」, 层开 debug.knobs.npuop=1 (N1 的 pack → NPU → unpack)
# 三臂都挂同一个层 .so、都开 passdump (同样的追踪开销, 也用它的 pass 表核对设置是否生效)。
#
# 子命令 (实验设计与判定规则见 hgamey/_tmp/npu/N3/REPORT.md):
#   dry-run   无设备: 核对工具链与配置, 用模拟设备把整条编排 + 判定 (真 phonefarm) 跑一遍, 给出计划与时长估计
#   smoke     约 10–15 分钟, 不做统计: NPU 确实替换了画面 (截图)、游戏不崩、逐帧 pack/npu/unpack 耗时
#   ui-probe  只走一遍设置界面并截图存证; --try-switch 时现场切一次渲染精度, 回答「要不要重启才生效」
#   drain     满电时先放电到 power.drain_target 以下 (实验要求电池轨放电态)
#   run       正式实验。--perf-first: 画质只作参考; 默认 (full) 另加同会话交错拍画质并作为判据
#   iq        只做画质会话 (同一会话里 C K C K C … 交错截图)
#   restore   按 <out>/state.json 还原 (渲染精度/帧率回原值、摘层、属性复原、恢复充电、风扇/亮度模式回原值)
#   judge     离线判定 (= python3 n3_judge.py <out>)
#
# 硬规则: 不删除/覆盖任何数据 (设备上要挪走的都 mv 到 /data/local/tmp/n3/<run_id>/parked/);
#         不登录 (出现登录/下载等界面立刻停); 不 push。
import argparse
import itertools
import json
import os
import random
import shutil
import subprocess
import sys
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from n3_phone import Phone, DryPhone, KNOB_PROPS, LAYER_NAME, LAYER_SO, raw_dims  # noqa: E402
from n3_ui import Ui, UiStop  # noqa: E402
import n3_judge  # noqa: E402

HG = "/Users/yao/Desktop/code/agent/hgamey"
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
DEF_PF = os.environ.get("PF", f"{HG}/_wt/integration/src/target/release/phonefarm")
DEF_CHAIN = os.environ.get("CHAIN", f"{HG}/_wt/integration/loop_v1")
DEF_SO = os.environ.get("LAYER_SO", f"{HG}/_wt/npu-n1/knobs/gray/build/out/{LAYER_SO}")
DEF_MODEL = f"{HG}/internal/hgamey-runs/2026-10-20-npu-genshin-sr/genshin_sr_16_16_int8.tflite"
CHAIN_FILES = {"auto/sample_env.sh": "PWR_IVL", "tools/ftrace_capture.sh": "target_tids",
               "auto/charge_suspend.sh": "usb_limit_check"}


class Abort(Exception):
    pass


# ═══════════════════════════ 主机侧工具核对 ═══════════════════════════

def check_tools(a, need_layer, out, log):
    """回 (问题列表, 警告列表)。dry-run 下全做一遍。"""
    errs, warns = [], []
    pf = a.pf
    if not os.path.exists(pf):
        errs.append(f"phonefarm 二进制不存在: {pf} (用整合分支 ruler/integration 编: cd {HG}/_wt/integration/src && cargo build --release)")
    else:
        p = subprocess.run([pf, "iq-judge"], capture_output=True)
        if p.returncode != 2 or "iq-judge" not in p.stderr.decode("utf-8", "replace"):
            errs.append(f"{pf} 没有 iq-judge 子命令 (image-quality 修复还没合进这个二进制)")
        try:
            env = n3_judge.loopstat(pf, {"op": "env_stats_window", "text": "ENV 1.0 4000000 -1000000 NA 0 0 cpu 40000\n",
                                         "trace_head": "#loop_v1_meta capture_start_uptime=0 capture_end_uptime=2\n"})
            if "power_window" not in env or "supply_mode" not in env:
                errs.append("loopstat env_stats_window 没有 power_window/supply_mode 字段 (fix-C/fix-D 没合进)")
        except Exception as e:  # noqa: BLE001
            errs.append(f"loopstat env_stats_window 不可用: {e}")
        tmp = os.path.join(out, "_selfcheck_trace.txt")
        lines = ["#loop_v1_meta capture_start_uptime=0 capture_end_uptime=3",
                 "#loop_v1_meta target_pkg=x target_pid=1 target_pid_start=1 target_tids=1,2"]
        for i in range(90):
            lines.append(f"  UnityGfxDeviceW-2   [001] ..... {i/30:.6f}: adreno_cmdbatch_submitted: ctx=7 ctx_prio=1 ts={i+1} inflight=1 flags=0x0 prio=1")
        open(tmp, "w").write("\n".join(lines) + "\n")
        p = subprocess.run([pf, "parse-trace", tmp], capture_output=True)
        try:
            s = json.loads(p.stdout)
            if s.get("frame_attribution") != "target_tids":
                errs.append("parse-trace 不按目标进程认帧 (frame-source 修复没合进)")
        except ValueError:
            errs.append("parse-trace 输出不是 JSON")
    for f, key in CHAIN_FILES.items():
        p = os.path.join(a.chain, f)
        if not os.path.exists(p):
            errs.append(f"测量链脚本不存在: {p}")
        elif key not in open(p, encoding="utf-8").read():
            errs.append(f"{p} 不是修过的版本 (找不到 {key})")
    if not os.path.exists(a.layer_so):
        (errs if need_layer else warns).append(f"层 .so 不存在: {a.layer_so} (N1 还没出构建产物?)")
    elif b"debug.knobs.npuop" not in open(a.layer_so, "rb").read():
        (errs if need_layer else warns).append(f"{a.layer_so} 里没有 debug.knobs.npuop (不是 N1 带 npuop 的版本)")
    if a.model and not os.path.exists(a.model):
        warns.append(f"模型文件不存在: {a.model}")
    if not shutil.which("adb"):
        errs.append("找不到 adb")
    ocr = ocr_bin(out)
    if not ocr:
        errs.append("编不出 OCR 工具 (需要 swiftc 与仓库根 ocr.swift)")
    if a.npu_deploy and not os.access(a.npu_deploy, os.X_OK):
        errs.append(f"--npu-deploy 不可执行: {a.npu_deploy}")
    if not a.npu_deploy:
        warns.append("没给 --npu-deploy: 假定 NPU 依赖 (libcdsprpc 等、QNN 库、模型、SELinux 放行) 已由 N1 布好; "
                     "B 臂的 npuop 自报会核对, 没在跑的臂判无效")
    return errs, warns


def ocr_bin(out):
    dst = os.path.join(out, "tools", "ocr")
    if os.path.exists(dst):
        return dst
    src = os.path.join(REPO, "ocr.swift")
    if not os.path.exists(src) or not shutil.which("swiftc"):
        return None
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    p = subprocess.run(["swiftc", "-O", src, "-o", dst], capture_output=True, timeout=600)
    return dst if p.returncode == 0 else None


# ═══════════════════════════ 计划 ═══════════════════════════

def make_plan(blocks, seed):
    """每块 C/A/B 各一臂, 块内顺序随机。6 种排列先各用一次 (打乱), 用完再来一轮, 先后位置尽量平衡。"""
    rng = random.Random(seed)
    perms = list(itertools.permutations("CAB"))
    seq = []
    while len(seq) < blocks:
        p = perms[:]
        rng.shuffle(p)
        seq += p
    return [list(p) for p in seq[:blocks]]


def estimate_s(order, cfg, mode):
    t = cfg["timing"]
    per_arm = 240 + 150 + t["warmup_s"] + t["measure_s"] + 60   # 等冷(估) + 启动进世界 + 预热 + 采集 + 收尾
    sw, cur = 0, None
    for blk in order:
        for arm in blk:
            s = cfg["arms"][arm]["setting"]
            if s != cur:
                sw += 1
                cur = s
    est = len(order) * 3 * per_arm + (sw + 1) * 240 + 300
    if mode == "full":
        est += 1200
    return est, sw


# ═══════════════════════════ 实验 ═══════════════════════════

class Exp:
    def __init__(self, a, phone, cfg, out):
        self.a, self.p, self.cfg, self.out = a, phone, cfg, out
        self.state_path = os.path.join(out, "state.json")
        self.state = json.load(open(self.state_path)) if os.path.exists(self.state_path) else {}
        self.ui = Ui(phone, cfg, os.path.join(out, "ui"), ocr_bin(out) or "", self.log)
        self.chain_dev = f"{phone.devdir}/chain"

    # ── 记录 ──
    def log(self, s):
        line = time.strftime("%H:%M:%S ") + s
        print(line, flush=True)
        with open(os.path.join(self.out, "run.log"), "a", encoding="utf-8") as f:
            f.write(line + "\n")

    def save(self):
        tmp = self.state_path + ".tmp"
        json.dump(self.state, open(tmp, "w"), ensure_ascii=False, indent=1)
        os.replace(tmp, self.state_path)

    def wj(self, rel, obj):
        p = os.path.join(self.out, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        json.dump(obj, open(p, "w"), ensure_ascii=False, indent=1)

    # ── 开跑前 ──
    def preflight(self, strict_power=True):
        p, c = self.p, self.cfg
        if not p.online():
            raise Abort(f"adb 看不到 {p.serial}")
        if not p.root_ok():
            raise Abort("su 不可用 (这台机必须有 root)")
        info = p.info()
        pre = {"info": info, "batt": p.batt(), "soc_c": p.soc_max_c(), "df_mb": p.df_data_mb(),
               "props": {k: p.getprop(k) for k in KNOB_PROPS + ["debug.vulkan.layers"]},
               "fan": p.fan(), "brightness_mode": p.brightness_mode(), "prefs": p.prefs(),
               "charge_state_file": p.charge_state_file(), "pid": p.pidof()}
        self.wj("preflight.json", pre)
        probs = []
        if "NX809J" not in info["model"]:
            probs.append(f"机型 {info['model']} 不是红魔 NX809J")
        if not info["pkg_path"]:
            probs.append(f"{p.pkg} 没装")
        if not info["libdir"]:
            probs.append("解析不到原神 arm64 库目录")
        if pre["props"]["debug.vulkan.layers"]:
            probs.append(f"debug.vulkan.layers 已被设成 {pre['props']['debug.vulkan.layers']} (别人在用层?)")
        if pre["charge_state_file"]:
            probs.append("/data/local/tmp/loop_v1_charge.state 已存在: 有别的任务停着充, 先确认再跑")
        if pre["df_mb"] is not None and pre["df_mb"] < c["disk"]["min_free_mb"]:
            probs.append(f"/data 只剩 {pre['df_mb']} MB, 每臂 trace 留在设备上不删, 需要 ≥ {c['disk']['min_free_mb']} MB")
        if not pre["prefs"] or c["setting"]["prefs_render_entry"] not in pre["prefs"]:
            probs.append("读不出原神 playerprefs 里的渲染精度 (只读), 没法记原值")
        cap = pre["batt"]["capacity"] or 0
        if strict_power:
            if cap < c["power"]["min_start_capacity"]:
                probs.append(f"电量 {cap}% 低于开跑下限 {c['power']['min_start_capacity']}% (整轮约耗 25–30%)")
            if cap > c["power"]["max_start_capacity"]:
                probs.append(f"电量 {cap}% 太满: 停充后会进 USB 旁路而不是电池轨。先跑 `drain` 放到 {c['power']['drain_target']}% 以下")
        lst = p.libdir_listing(info["libdir"]) if info["libdir"] else {}
        if LAYER_SO in lst:
            probs.append(f"游戏库目录里已有 {LAYER_SO} (上次没摘干净?), 先人工确认再跑")
        if probs:
            raise Abort("开跑前检查没过:\n  - " + "\n  - ".join(probs))
        st = self.state
        if "orig" not in st:   # 只在第一次记原值; 断点续跑不覆盖
            st["orig"] = {"props": pre["props"], "fan": pre["fan"], "brightness_mode": pre["brightness_mode"],
                          "prefs": {str(k): v for k, v in pre["prefs"].items()}, "libdir_listing": lst}
        st.update({"run_id": p.run_id, "serial": p.serial, "pkg": p.pkg, "libdir": info["libdir"], "devdir": p.devdir})
        idx = pre["prefs"][c["setting"]["prefs_render_entry"]]
        exp = c["setting"]["prefs_expected_index"]
        st.setdefault("setting_current", "native" if idx == exp["native"] else "low" if idx == exp["low"] else f"index{idx}")
        if idx != exp["native"]:
            self.log(f"注意: 渲染精度原值 index={idx}, 不是预期的极高 (4)。C 臂仍以 pass 表核对「原生」(场景 pass ≈ swapchain)")
        self.save()
        self.log(f"开跑前检查通过: {info['model']} 原神 {info['version']} 电量 {cap}% "
                 f"电池 {pre['batt']['temp_c']}°C SoC {pre['soc_c']}°C 渲染精度 index={idx}")
        return pre

    def prepare(self):
        """推测量链脚本、装层、(可选) N1 的 NPU 依赖布置。每一步先记 state 再动设备。"""
        p, st = self.p, self.state
        p.mkdevdir("chain")
        for f in CHAIN_FILES:
            if not p.push(os.path.join(self.a.chain, f), f"{self.chain_dev}/{os.path.basename(f)}") and not p.dry:
                raise Abort(f"推 {f} 失败")
        st["layer_installed"] = "pending"
        self.save()
        ok, why = p.install_layer(self.a.layer_so, st["libdir"])
        if not ok:
            raise Abort(f"装层失败: {why}")
        st["layer_installed"] = True
        self.save()
        if self.a.npu_deploy and not st.get("npu_deployed"):
            st["npu_deployed"] = "pending"
            self.save()
            env = dict(os.environ, SERIAL=p.serial, PKG=p.pkg, LIBDIR=st["libdir"], DEVDIR=p.devdir,
                       MODEL=self.a.model or "")
            self.log(f"布置 NPU 依赖: {self.a.npu_deploy}")
            if not p.dry:
                r = subprocess.run([self.a.npu_deploy, "install"], env=env, capture_output=True, timeout=600)
                open(os.path.join(self.out, "npu_deploy.log"), "wb").write(r.stdout + r.stderr)
                if r.returncode != 0:
                    raise Abort(f"NPU 依赖布置失败 (rc={r.returncode}), 见 npu_deploy.log")
            st["npu_deployed"] = True
            self.save()
        after = p.libdir_listing(st["libdir"])
        changed = [k for k, v in st["orig"]["libdir_listing"].items() if after.get(k) != v]
        if changed:
            raise Abort(f"游戏库目录里原有文件被改动了: {changed} (不应发生; 先停下核查, restore 不会自动处理)")
        st["libdir_added"] = sorted(set(after) - set(st["orig"]["libdir_listing"]))
        self.save()
        self.log(f"已装层 + 依赖, 库目录新增: {st['libdir_added']}")

    # ── 挂层启动 → 进大世界 ──
    def set_knobs(self, npuop):
        p = self.p
        for k in KNOB_PROPS:
            p.setprop(k, "0")
        okp = p.setprop("debug.knobs.passdump", "1")
        okn = p.setprop("debug.knobs.npuop", npuop)
        if not (okp and okn):
            raise Abort("setprop debug.knobs.* 回读不对")

    def launch_to_world(self, npuop, tag):
        p, t = self.p, self.cfg["timing"]
        p.force_stop()
        p.archive_marker(tag)
        self.set_knobs(npuop)
        if not p.setprop("debug.vulkan.layers", LAYER_NAME):
            raise Abort("setprop debug.vulkan.layers 没生效")
        self.state["layer_prop_set"] = True
        self.save()
        try:
            if not p.launch():
                raise Abort("解析不到原神启动 activity")
            m, waited = None, 0
            while waited < t["marker_wait_s"]:
                p.sleep(3)
                waited += 3
                m = p.marker()
                if m:
                    break
                if waited > 20 and not p.pidof():
                    raise Abort("原神启动后进程没了 (崩溃或被拦)")
        finally:
            p.setprop("debug.vulkan.layers", "")   # 收窄全局属性的误伤窗口 (enable_layer.sh 的教训)
            self.state["layer_prop_set"] = False
            self.save()
        if not m:
            raise Abort(f"等了 {t['marker_wait_s']}s 层没加载 (没有 knobs_layer_out.json)")
        if not self.ui.pass_gate(t["gate_wait_s"]):
            raise Abort(f"等了 {t['gate_wait_s']}s 没看到「点击进入」, 原神不在可玩状态 (截图在 ui/)。脚本不登录, 已停止")
        if not self.in_world(t["world_wait_s"]):
            raise Abort("点了进门, 等不到大世界 (pass/帧 < 25)")
        self.ui.dismiss_popups()
        return m

    def in_world(self, timeout):
        """与 genshin_upop_ab.sh 的 in_world 同判据: 每帧 render pass 数 ≥ 25。
        标记每 300 帧才刷新一次 (30 fps 下 10 秒), 所以取两次不同的快照算差。"""
        p, waited, prev = self.p, 0, None
        while waited < timeout:
            p.sleep(4)
            waited += 4
            m = p.marker()
            if not m:
                continue
            f, r = m["readonly_stats"]["frames"], m["readonly_stats"]["render_pass_begins"]
            if prev and f > prev[0]:
                ppf = (r - prev[1]) / (f - prev[0])
                if ppf >= 25:
                    self.log(f"    已进大世界 (pass/帧={ppf:.0f})")
                    return True
                prev = (f, r)
            elif not prev:
                prev = (f, r)
            if not p.pidof():
                return False
        return False

    def observe_setting(self, m):
        """从 pass 表读实际生效的渲染精度: 场景主 pass (att=4、宽 ≥ 1000) 里帧数最多的那一档。"""
        s = self.cfg["setting"]
        if not m or "pass_table" not in m:
            return None, None
        rows = [r for r in m["pass_table"] if r.get("att") == 4 and r.get("w", 0) >= 1000]
        if not rows:
            return "unknown", None
        r = max(rows, key=lambda r: r.get("frames", 0))
        wh = [r["w"], r["h"]]
        sw = (m.get("swapchain") or {}).get("w") or 0
        if wh == s["low_wh"]:
            return "low", wh
        if sw and r["w"] >= s["native_min_frac_of_swapchain"] * sw:
            return "native", wh
        return f"other_{r['w']}x{r['h']}", wh

    @staticmethod
    def npu_section(m):
        if not m:
            return None
        if isinstance(m.get("npuop"), dict):
            return m["npuop"]
        cp = m.get("copyprobe")
        if isinstance(cp, dict) and "npu" in str(cp.get("mode", "")):
            return cp
        return None

    def npu_status(self, m0, m1):
        """B 臂: 标记里的 npuop 段在窗口内确实在跑、没有回退。接口约定见 REPORT「N1 接口」。"""
        n0, n1 = self.npu_section(m0) or {}, self.npu_section(m1)
        if not n1:
            return {"ok": False, "why": "标记里没有 npuop 段 (层没编进 npuop / 没打开 / 初始化失败)"}
        runs = lambda n: n.get("runs", n.get("frames", n.get("copies", 0))) or 0
        fb = lambda n: (1 if n.get("fallback") is True else 0) if isinstance(n.get("fallback"), bool) else (n.get("fallback") or n.get("fallbacks") or 0)
        f0 = (m0 or {}).get("readonly_stats", {}).get("frames", 0)
        f1 = m1["readonly_stats"]["frames"]
        dr, df, dfb = runs(n1) - runs(n0), f1 - f0, fb(n1) - fb(n0)
        frac = self.cfg["npu"]["require_runs_frac"]
        ok = not n1.get("disabled") and dfb == 0 and df > 0 and dr >= frac * df
        why = "" if ok else f"npuop 跑了 {dr}/{df} 帧, 回退 {dfb}, disabled={n1.get('disabled')}"
        return {"ok": ok, "why": why, "runs_delta": dr, "frames_delta": df, "fallback_delta": dfb, "section": n1}

    # ── 设置切换 (只在需要时) ──
    def ensure_setting(self, level, why=""):
        st, s = self.state, self.cfg["setting"]
        if st.get("setting_current") == level and st.get("fps_set"):
            return
        self.log(f"  切设置 → 渲染精度 {s['labels'][level]}、帧率 {s['fps_label']} {why}")
        before = self.p.prefs() or {}
        m = self.launch_to_world("0", f"set_{level}")
        rec = {"t": time.time(), "to": level, "prefs_before": {str(k): v for k, v in before.items()},
               "observed_before": self.observe_setting(m)[0]}
        self.ui.open_graphics()
        if not st.get("fps_set"):
            r = self.ui.set_option(s["fps_row_text"], s["fps_label"])
            rec["fps_set"] = r
            st.setdefault("fps_orig_label", r.get("orig_label"))
        rec["render_set"] = self.ui.set_option(s["render_row_text"], s["labels"][level])
        self.ui.close_to_world()
        # 现场是否生效 (回答「改渲染精度要不要重启」): 等一会儿, 看 pass 表里目标尺寸那一档的帧数是否在涨
        m1 = self.p.marker()
        self.p.sleep(self.cfg["timing"]["settle_after_ui_s"])
        m2 = self.p.marker()
        rec["live_effect"] = self.live_effect(m1, m2, level)
        if not self.ui.graceful_quit():
            self.log("    优雅退出后进程还在, 改用 force-stop (设置可能没落盘, 下面用 prefs 核对)")
            self.p.force_stop()
        after = self.p.prefs() or {}
        rec["prefs_after"] = {str(k): v for k, v in after.items()}
        idx = after.get(s["prefs_render_entry"])
        rec["render_index_after"] = idx
        if idx == before.get(s["prefs_render_entry"]) and rec["observed_before"] != level:
            self.wj(f"settings/{int(time.time())}_{level}.json", rec)
            raise Abort(f"渲染精度改了但 prefs 没变 (index 仍为 {idx}): 设置没落盘。截图在 ui/")
        st.setdefault("render_index_seen", {})[level] = idx
        st["setting_current"] = level
        st["fps_set"] = True
        self.save()
        self.wj(f"settings/{int(time.time())}_{level}.json", rec)

    def live_effect(self, m1, m2, level):
        if not (m1 and m2):
            return "unknown"
        want = self.cfg["setting"]["low_wh"] if level == "low" else None
        def frames_at(m, fn):
            return sum(r.get("frames", 0) for r in m.get("pass_table", []) if r.get("att") == 4 and fn(r))
        fn = (lambda r: [r["w"], r["h"]] == want) if want else (lambda r: r.get("w", 0) >= 2000)
        grow = frames_at(m2, fn) - frames_at(m1, fn)
        return {"target_scene_frames_grew": grow, "live": grow > 0}

    # ── 等冷 ──
    def cool(self):
        th, p = self.cfg["thermal"], self.p
        p.force_stop()
        t0, series = p.now(), []
        while True:
            b, s = p.batt(), p.soc_max_c()
            series.append([round(p.now() - t0), b["temp_c"], s, b["capacity"], b["current_ua"]])
            if b["temp_c"] is not None and b["temp_c"] <= th["batt_start_c"] and s is not None and s <= th["soc_start_c"]:
                return {"reached": True, "waited_s": round(p.now() - t0), "batt_c": b["temp_c"], "soc_c": s, "series": series}
            if p.now() - t0 > th["max_wait_s"]:
                return {"reached": False, "waited_s": round(p.now() - t0), "batt_c": b["temp_c"], "soc_c": s, "series": series}
            p.sleep(th["poll_s"])

    # ── 停充: 电池轨放电态 ──
    def suspend_charging(self):
        p, st = self.p, self.state
        st["charge_suspended"] = "pending"
        self.save()
        o = p.charge("suspend", self.chain_dev)
        self.log("  停充: " + o.splitlines()[0] if o else "  停充: (无输出)")
        open(os.path.join(self.out, "charge_suspend.log"), "a").write(o + "\n")
        mode = (p.charge_state_file().splitlines() + ["", "", ""])[2]
        if "ALREADY_ON_BATTERY" in o:
            st["charge_suspended"] = False
            mode = "battery"
        else:
            st["charge_suspended"] = bool(p.charge_state_file())
        self.save()
        if mode != "battery":
            raise Abort(f"停充后不是电池放电态 (charge_mode={mode or '无'}; 输出: {o[:200]})。"
                        f"满电时会进 USB 旁路, 先跑 drain 把电量放到 {self.cfg['power']['drain_target']}% 以下")
        for _ in range(60):   # 等电池电流稳定为放电 (fix-F: 停充后最初一段电池仍在吸电)
            b1 = p.batt()
            p.sleep(2)
            b2 = p.batt()
            if all((b["current_ua"] or 0) < 0 and b["status"] != "Charging" for b in (b1, b2)):
                self.log(f"  电池放电态已稳定: {b2['current_ua']} µA, {b2['capacity']}%")
                return
        raise Abort("停充后 2 分钟电池电流仍不是稳定放电")

    # ── 一臂 ──
    def run_arm(self, idx, blk, arm, attempt):
        c, p = self.cfg, self.p
        label = f"{idx:02d}_b{blk}_{arm}" + (f"_r{attempt}" if attempt else "")
        d = os.path.join(self.out, "arms", label)
        os.makedirs(d, exist_ok=True)
        rec = {"label": label, "block": blk, "arm": arm, "attempt": attempt, "collected": False,
               "invalid_reason": [], "t_start": time.time()}
        try:
            self.ensure_setting(c["arms"][arm]["setting"], f"(臂 {label})")
            b = p.batt()
            if (b["capacity"] or 0) < c["power"]["abort_capacity"]:
                raise Abort(f"电量 {b['capacity']}% 已到停止线 {c['power']['abort_capacity']}%")
            if b["status"] == "Charging" or (b["current_ua"] or 0) > 0:
                rec["invalid_reason"].append(f"开跑前不是放电态 ({b['status']}, {b['current_ua']} µA)")
            cl = self.cool()
            rec["cool"] = cl
            rec["start_temp"] = {"batt_c": cl["batt_c"], "soc_c": cl["soc_c"], "reached": cl["reached"]}
            self.log(f"  [{label}] 等冷 {cl['waited_s']}s → 电池 {cl['batt_c']}°C SoC {cl['soc_c']}°C")
            self.launch_to_world(c["arms"][arm]["npuop"], label)
            rec["fan_after_world"] = {"before": p.fan()}
            if rec["fan_after_world"]["before"] not in ("", "0"):
                p.fan_set("0")
            rec["fan_after_world"]["after"] = p.fan()
            p.sleep(c["timing"]["warmup_s"])
            m0 = p.marker()
            obs, wh = self.observe_setting(m0)
            rec["setting_expected"], rec["setting_observed"], rec["scene_pass"] = c["arms"][arm]["setting"], obs, wh
            if obs != c["arms"][arm]["setting"]:
                rec["invalid_reason"].append(f"设置没生效: 要 {c['arms'][arm]['setting']}, pass 表是 {obs} {wh}")
                self.state["setting_current"] = obs   # 下一臂会重新切
                self.save()
            rec["shots"] = []
            if p.screencap(os.path.join(d, "shot_pre.raw")):
                rec["shots"].append("shot_pre.raw")
            rec["batt_start"] = p.batt()
            ok = p.measure(self.chain_dev, c["timing"]["measure_s"], d, label)
            rec["batt_end"] = p.batt()
            m1 = p.marker()
            if p.screencap(os.path.join(d, "shot_post.raw")):
                rec["shots"].append("shot_post.raw")
            json.dump({"before": m0, "after": m1}, open(os.path.join(d, "marker.json"), "w"), ensure_ascii=False)
            rec["fan_end"] = p.fan()
            if not p.pidof():
                rec["invalid_reason"].append("采集结束时游戏进程不在 (崩溃?)")
            ns = self.npu_status(m0, m1)
            rec["npuop"] = ns
            if arm == "B" and not ns["ok"]:
                rec["invalid_reason"].append("NPU 没在跑: " + ns["why"])
            if arm != "B" and ns.get("runs_delta", 0) > 0:
                rec["invalid_reason"].append("对照臂里 npuop 在跑")
            p.logcat_dump(os.path.join(d, "logcat_refknobs.txt"), "refknobs|npu|qnn|Fatal signal|FATAL EXCEPTION")
            if not ok:
                rec["invalid_reason"].append("trace 没拉回")
            rec["collected"] = ok
        except UiStop as e:
            rec["invalid_reason"].append(f"界面: {e}")
            raise Abort(str(e))
        finally:
            p.force_stop()
            rec["t_end"] = time.time()
            json.dump(rec, open(os.path.join(d, "arm.json"), "w"), ensure_ascii=False, indent=1)
        self.log(f"  [{label}] 完成: {'有效' if not rec['invalid_reason'] else '无效: ' + '; '.join(rec['invalid_reason'])}")
        return not rec["invalid_reason"]

    # ── 画质会话: 同一次游戏会话里现场切渲染精度, 交错拍 ──
    def iq_session(self):
        c, s, p = self.cfg, self.cfg["setting"], self.p
        d = os.path.join(self.out, "iq")
        os.makedirs(d, exist_ok=True)
        seqs = {}
        live_any = None
        for k, npuop in (("B", "1"), ("A", "0")):
            self.ensure_setting("native", "(画质会话从原生起)")
            self.launch_to_world(npuop, f"iq_{k}")
            p.sleep(c["timing"]["warmup_s"])
            order = ["C"] + [x for _ in range(c["iq"]["n_ctrl"] - 1) for x in (k, "C")]
            shots, cur = [], "C"
            for i, arm in enumerate(order):
                want = "native" if arm == "C" else "low"
                switched = want != cur
                if switched:
                    self.ui.open_graphics()
                    self.ui.set_option(s["render_row_text"], s["labels"][want])
                    self.ui.close_to_world()
                    p.sleep(10)
                    cur = want
                m = p.marker()
                f = f"seq{k}_{i:02d}_{arm}.raw"
                ok = p.screencap(os.path.join(d, f))
                p.sleep(4)
                m2 = p.marker()
                obs = self.observe_setting(m2)[0]
                live = self.live_effect(m, m2, want) if (m and m2) else "unknown"
                ns = self.npu_status(m, m2) if (m and m2) else {"ok": None}
                good = ok and (live == "unknown" or live.get("live"))
                if arm == k and k == "B" and ns.get("ok") is False:
                    good = False
                if arm == "C" and ns.get("runs_delta", 0) > 0:
                    good = False   # 原生时 npuop 不应在跑
                shots.append({"file": f, "arm": arm, "ok": bool(good), "observed": obs, "live": live,
                              "npu": {kk: ns.get(kk) for kk in ("ok", "runs_delta", "frames_delta")}})
                # 只有刚切到「低」的那一张能说明「现场是否生效」(1134×514 那档开始涨帧);
                # 切回原生时大尺寸那档本来就在涨 (没切成也在涨), 不作证据
                if switched and want == "low" and isinstance(live, dict):
                    live_any = bool(live.get("live")) or bool(live_any)
            # 收尾: 设回原生并优雅退出 (落盘)
            if cur != "native":
                self.ui.open_graphics()
                self.ui.set_option(s["render_row_text"], s["labels"]["native"])
                self.ui.close_to_world()
            if not self.ui.graceful_quit():
                p.force_stop()
            self.state["setting_current"] = "native"
            self.save()
            seqs[f"seq_{k}"] = shots
        seqs["live_switch"] = live_any
        json.dump(seqs, open(os.path.join(d, "sequence.json"), "w"), ensure_ascii=False, indent=1)
        if live_any is False:
            self.log("  画质会话: 渲染精度现场不生效 (要重启), 交错拍不成立; 画质只剩跨会话参考")

    def write_exclude(self):
        """UI 排除框按截图尺寸换算成像素, 写 iq_exclude_px.json (judge 用)。"""
        for root, _, files in os.walk(os.path.join(self.out, "arms")):
            for f in files:
                if f.endswith(".raw"):
                    d = open(os.path.join(root, f), "rb").read()
                    dims = raw_dims(d)
                    if dims:
                        w, h, _ = dims
                        px = [[round(x0 * w), round(y0 * h), round(x1 * w), round(y1 * h)]
                              for x0, y0, x1, y1 in self.cfg["iq"]["exclude_frac"]]
                        self.wj("iq_exclude_px.json", px)
                        return px
        return []

    # ── 还原 ──
    def restore(self, skip_settings=False):
        p, st, s = self.p, self.state, self.cfg["setting"]
        if not st:
            self.log("没有 state.json, 无需还原")
            return
        rep = {"t": time.time()}
        self.log("── 还原 ──")
        try:
            p.setprop("debug.vulkan.layers", "")
            orig_idx = st["orig"]["prefs"].get(str(s["prefs_render_entry"]))
            orig_fps = st["orig"]["prefs"].get(str(s["prefs_fps_entry"]))
            cur = p.prefs() or {}
            need_r = cur.get(s["prefs_render_entry"]) != orig_idx
            need_f = cur.get(s["prefs_fps_entry"]) != orig_fps
            if skip_settings and (need_r or need_f):
                self.log("  --keep-setting: 渲染精度/帧率留在当前值, 不改回 (之后的 restore 会改回)")
            elif (need_r or need_f) and st.get("layer_installed") is True:
                self.log(f"  设置回原值: 渲染精度 index {cur.get(s['prefs_render_entry'])}→{orig_idx}, 帧率 index {cur.get(s['prefs_fps_entry'])}→{orig_fps}")
                try:
                    self.launch_to_world("0", "restore")
                    self.ui.open_graphics()
                    if need_f:
                        lab = st.get("fps_orig_label") or self.cfg["setting"]["fps_orig_label_fallback"]
                        self.ui.set_option(s["fps_row_text"], lab)
                    if need_r:
                        if orig_idx == s["prefs_expected_index"]["native"]:
                            self.ui.set_option(s["render_row_text"], s["labels"]["native"])
                        else:
                            raise UiStop(f"原渲染精度 index={orig_idx} 不是配置里认得的档, 请人工在游戏里改回")
                    self.ui.close_to_world()
                    if not self.ui.graceful_quit():
                        p.force_stop()
                except (UiStop, Abort) as e:
                    rep["settings_error"] = str(e)
                    self.log(f"  !! 设置没能自动改回: {e}")
                    p.force_stop()
                cur = p.prefs() or {}
            rep["prefs_after"] = {str(k): v for k, v in cur.items()}
            rep["render_restored"] = cur.get(s["prefs_render_entry"]) == orig_idx
            rep["fps_restored"] = cur.get(s["prefs_fps_entry"]) == orig_fps
        except Exception as e:  # noqa: BLE001
            rep["settings_error"] = f"{type(e).__name__}: {e}"
        p.force_stop()
        # 属性回原值
        for k, v in st.get("orig", {}).get("props", {}).items():
            p.setprop(k, v)
        rep["props_after"] = {k: p.getprop(k) for k in st.get("orig", {}).get("props", {})}
        # 库目录: 本次新增的文件挪到 parked (不删), 核对和原样一致
        if st.get("libdir"):
            now = p.libdir_listing(st["libdir"])
            orig = st["orig"]["libdir_listing"]
            parked = []
            for f in sorted(set(now) - set(orig)):
                parked.append([f, p.park(f"{st['libdir']}/{f}", "libdir")])
            after = p.libdir_listing(st["libdir"])
            rep["libdir_parked"] = parked
            rep["libdir_same_as_orig"] = after == orig
            st["layer_installed"] = False
        # 充电
        if st.get("charge_suspended"):
            o = p.charge("restore", self.chain_dev)
            rep["charge_restore"] = o
            st["charge_suspended"] = False
        if st.get("drain"):
            node, v = st["drain"]
            rep["drain_restore"] = p.write_node(node, v)
            st["drain"] = None
        # 风扇 / 亮度模式
        of, ob = st.get("orig", {}).get("fan"), st.get("orig", {}).get("brightness_mode")
        if of not in (None, "") and p.fan() != of:
            p.fan_set(of)
        if ob not in (None, "") and p.brightness_mode() != ob:
            p.brightness_mode_set(ob)
        rep["fan_after"], rep["brightness_mode_after"] = p.fan(), p.brightness_mode()
        rep["batt_after"] = p.batt()
        rep["note"] = ("SELinux 的 magiskpolicy --live 放行 (若 N1 的布置用了) 重启后自动失效, 本脚本不动它; "
                       f"设备上的 trace 与挪走的文件都留在 {st.get('devdir')}/ 下, 没删")
        self.save()
        self.wj(f"restore_{int(time.time())}.json", rep)
        bad = [k for k in ("render_restored", "fps_restored", "libdir_same_as_orig") if rep.get(k) is False]
        self.log("还原完成" + (f", 但这些没回到原样: {bad} (见 restore_*.json, 需要人工处理)" if bad else ", 全部回读一致"))


# ═══════════════════════════ 子命令 ═══════════════════════════

def new_out(a, mode):
    out = a.out or os.path.join(HG, "_tmp", "npu", "N3", "runs", f"{mode}_{time.strftime('%Y%m%d_%H%M%S')}")
    if os.path.exists(os.path.join(out, "plan.json")) and mode not in ("restore", "judge") and not a.resume:
        sys.exit(f"{out} 已有一次运行 (不覆盖)。换 --out, 或加 --resume 续跑")
    os.makedirs(out, exist_ok=True)
    return out


def make_phone(a, cfg, out, dry):
    run_id = os.path.basename(out.rstrip("/"))
    cls = DryPhone if dry else Phone
    return cls(a.serial or cfg["serial"], cfg["pkg"], run_id, os.path.join(out, "cmd.log"))


def cmd_run(a, cfg, dry=False):
    mode = "perf-first" if a.perf_first else "full"
    out = new_out(a, ("dry_" if dry else "") + mode)
    # stat-guards: 精确置换 + Bonferroni/3 下判得出显著的最少块数, 用同一个 compare 算 (不另写公式)
    min_pairs = next(n for n in range(2, 65) if n3_judge.min_reachable_p(a.pf, n) < 0.05 / 3) if os.path.exists(a.pf) else 5
    if a.blocks < min_pairs:
        sys.exit(f"--blocks {a.blocks} 判不出显著 (精确置换 + Bonferroni/3 至少要 {min_pairs} 块)")
    errs, warns = check_tools(a, need_layer=not dry, out=out, log=print)
    for w in warns:
        print("警告:", w)
    if errs:
        print("工具核对没过:\n  - " + "\n  - ".join(errs))
        if not dry:
            sys.exit(2)
    seed = a.seed if a.seed is not None else int(time.time())
    order = make_plan(a.blocks, seed)
    est, sw = estimate_s(order, cfg, mode)
    plan = {"mode": mode, "dry": dry, "seed": seed, "blocks": a.blocks, "order": ["".join(b) for b in order],
            "est_s": est, "ui_switches": sw, "min_pairs": min_pairs, "pf": a.pf, "chain": a.chain,
            "layer_so": a.layer_so, "model": a.model, "npu_deploy": a.npu_deploy, "config": cfg,
            "tool_errors": errs, "tool_warnings": warns, "created": time.strftime("%Y-%m-%d %H:%M:%S")}
    if not (a.resume and os.path.exists(os.path.join(out, "plan.json"))):
        json.dump(plan, open(os.path.join(out, "plan.json"), "w"), ensure_ascii=False, indent=1)
    else:
        plan = json.load(open(os.path.join(out, "plan.json")))
        order = [list(x) for x in plan["order"]]
    print(f"计划: {a.blocks} 块, 块内顺序 {plan['order']} (seed={plan['seed']}), 设置切换 {sw} 次, "
          f"预计 {est/3600:.1f} 小时 (等冷时间是估计)。输出 {out}")
    p = make_phone(a, cfg, out, dry)
    x = Exp(a, p, cfg, out)
    t0 = p.now()
    rc = 0
    try:
        x.preflight(strict_power=True)
        x.prepare()
        x.ensure_setting(cfg["arms"][order[0][0]]["setting"], "(第一臂之前; 同时是可玩性检查)")
        x.suspend_charging()
        done = {(r["block"], r["arm"]) for r in
                (json.load(open(os.path.join(dp, "arm.json"))) for dp in
                 sorted(os.path.join(out, "arms", q) for q in os.listdir(os.path.join(out, "arms")))
                 if os.path.exists(os.path.join(dp, "arm.json")))
                if not r["invalid_reason"]} if os.path.isdir(os.path.join(out, "arms")) else set()
        idx = len(os.listdir(os.path.join(out, "arms"))) if os.path.isdir(os.path.join(out, "arms")) else 0
        for blk, arms in enumerate(order, 1):
            for arm in arms:
                if (blk, arm) in done:
                    continue
                for attempt in range(cfg["timing"]["arm_retries"] + 1):
                    idx += 1
                    if x.run_arm(idx, blk, arm, attempt):
                        break
        x.write_exclude()
        if mode == "full":
            x.iq_session()
    except (Abort, UiStop) as e:
        x.log(f"!! 停止: {e}")
        plan["stopped"] = str(e)
        rc = 3
    except Exception:  # noqa: BLE001
        x.log("!! 意外错误:\n" + traceback.format_exc())
        plan["stopped"] = "意外错误, 见 run.log"
        rc = 4
    finally:
        if not a.no_restore:
            x.restore()
    x.log(f"设备阶段用时 {(p.now() - t0)/3600:.2f} 小时{' (模拟时钟)' if dry else ''}")
    plan["elapsed_s"] = round(p.now() - t0)
    json.dump(plan, open(os.path.join(out, "plan.json"), "w"), ensure_ascii=False, indent=1)
    if os.path.exists(a.pf):
        subprocess.run([sys.executable, os.path.join(HERE, "n3_judge.py"), out, "--pf", a.pf])
    return rc


def cmd_smoke(a, cfg, dry=False):
    out = new_out(a, ("dry_" if dry else "") + "smoke")
    errs, warns = check_tools(a, need_layer=not dry, out=out, log=print)
    for w in warns:
        print("警告:", w)
    if errs and not dry:
        sys.exit("工具核对没过:\n  - " + "\n  - ".join(errs))
    json.dump({"mode": "smoke", "config": cfg, "pf": a.pf, "layer_so": a.layer_so}, open(os.path.join(out, "plan.json"), "w"),
              ensure_ascii=False, indent=1)
    p = make_phone(a, cfg, out, dry)
    x = Exp(a, p, cfg, out)
    res = {"t": time.strftime("%Y-%m-%d %H:%M:%S")}
    t0 = p.now()
    try:
        x.preflight(strict_power=False)
        x.prepare()
        x.ensure_setting("low", "(冒烟: 低档)")
        tomb0 = p.out("ls /data/tombstones 2>/dev/null | wc -l", True)
        for k, npuop, secs in (("A", "0", 30), ("B", "1", 180)):
            d = os.path.join(out, f"smoke_{k}")
            os.makedirs(d, exist_ok=True)
            m0 = x.launch_to_world(npuop, f"smoke_{k}")
            p.sleep(15)
            m0 = p.marker()
            checks, shots = [], []
            for i in range(secs // 30):
                p.sleep(30)
                m = p.marker()
                checks.append({"t": (i + 1) * 30, "alive": bool(p.pidof()), "npu": x.npu_status(m0, m) if m else None,
                               "observed": x.observe_setting(m)[0] if m else None})
            for i in range(3):
                f = os.path.join(d, f"shot{i}.raw")
                if p.screencap(f):
                    shots.append(f)
                p.sleep(3)
            m1 = p.marker()
            lc = p.logcat_dump(os.path.join(d, "logcat.txt"), "refknobs|npu|qnn|Fatal signal|FATAL EXCEPTION")
            res[k] = {"checks": checks, "alive_end": bool(p.pidof()), "npu": x.npu_status(m0, m1),
                      "observed": x.observe_setting(m1)[0], "shots": shots,
                      "npu_timing_lines": [l for l in lc.splitlines() if "npu" in l.lower()][-60:],
                      "fatal_in_logcat": [l for l in lc.splitlines() if "Fatal signal" in l or "FATAL EXCEPTION" in l]}
            json.dump({"before": m0, "after": m1}, open(os.path.join(d, "marker.json"), "w"), ensure_ascii=False)
            p.force_stop()
        res["tombstones_before_after"] = [tomb0, p.out("ls /data/tombstones 2>/dev/null | wc -l", True)]
        if res["A"]["shots"] and res["B"]["shots"] and os.path.exists(a.pf):
            # A 三张自己比是「游戏自带放大」的自噪声; B 落在它之外说明画面确实被换了 (iq-judge 判 fail = 不同)
            r = n3_judge.iq(a.pf, res["A"]["shots"], res["B"]["shots"][:2], [], os.path.join(out, "smoke_B_vs_A_iq.json"))
            res["B_differs_from_A"] = {"iq_verdict": r.get("verdict"), "note": "fail = B 画面和游戏自带放大不同 (预期); pass = 可能没替换上"}
        b = res["B"]
        res["ok"] = {"npu_running": bool(b["npu"].get("ok")), "game_alive": b["alive_end"] and all(c["alive"] for c in b["checks"]),
                     "frame_replaced": res.get("B_differs_from_A", {}).get("iq_verdict") == "fail",
                     "no_fatal": not b["fatal_in_logcat"]}
    except (Abort, UiStop) as e:
        x.log(f"!! 冒烟停止: {e}")
        res["abort"] = str(e)
    except Exception:  # noqa: BLE001
        res["error"] = traceback.format_exc()
        x.log("!! 意外错误:\n" + res["error"])
    finally:
        if not a.no_restore:
            x.restore(skip_settings=a.keep_setting)
    res["elapsed_s"] = round(p.now() - t0)
    json.dump(res, open(os.path.join(out, "smoke.json"), "w"), ensure_ascii=False, indent=1)
    print(json.dumps(res.get("ok", res.get("abort")), ensure_ascii=False, indent=1))
    print(f"冒烟用时 {res['elapsed_s']/60:.1f} 分钟{' (模拟时钟)' if dry else ''}; 明细 {out}/smoke.json")
    return 0 if all(res.get("ok", {}).values() or [False]) else 3


def cmd_ui_probe(a, cfg):
    out = new_out(a, "uiprobe")
    p = make_phone(a, cfg, out, False)
    x = Exp(a, p, cfg, out)
    res = {}
    try:
        x.preflight(strict_power=False)
        x.prepare()
        m = x.launch_to_world("0", "probe")
        res["observed_start"] = x.observe_setting(m)
        x.ui.open_graphics()
        s = cfg["setting"]
        for row in (s["fps_row_text"], s["render_row_text"]):
            r, items = x.ui.find_row(row)
            res[f"row_{row}"] = [i["t"] for i in x.ui.row_values(items, r)]
        if a.try_switch:
            orig = res["observed_start"][0]
            to = "low" if orig == "native" else "native"
            m1 = p.marker()
            res["switch_to"] = to
            res["set"] = x.ui.set_option(s["render_row_text"], s["labels"][to])
            x.ui.close_to_world()
            p.sleep(cfg["timing"]["settle_after_ui_s"])
            m2 = p.marker()
            res["live_effect"] = x.live_effect(m1, m2, to)
            x.ui.open_graphics()
            res["set_back"] = x.ui.set_option(s["render_row_text"], s["labels"][orig])
        x.ui.close_to_world()
        res["graceful_quit"] = x.ui.graceful_quit()
        if a.try_switch:
            m = x.launch_to_world("0", "probe_verify")
            res["observed_after_relaunch"] = x.observe_setting(m)
            p.force_stop()
    except (Abort, UiStop) as e:
        res["stopped"] = str(e)
    finally:
        x.restore()
    json.dump(res, open(os.path.join(out, "ui_probe.json"), "w"), ensure_ascii=False, indent=1)
    print(json.dumps(res, ensure_ascii=False, indent=1))


def cmd_drain(a, cfg):
    """满电 → 放到 drain_target 以下: 写 input_suspend=1 (先记原值), 原神原生档挂在大世界里耗电。完后写回原值。"""
    out = new_out(a, "drain")
    p = make_phone(a, cfg, out, False)
    x = Exp(a, p, cfg, out)
    try:
        x.preflight(strict_power=False)
        nodes = p.input_suspend_nodes()
        if not nodes:
            raise Abort("这台机没有 input_suspend 节点, 没法在插线时放电; 请拔线放电或换办法")
        node, orig = nodes[0]
        x.state["drain"] = [node, orig]
        x.save()
        if not p.write_node(node, "1"):
            raise Abort(f"写 {node}=1 不生效")
        x.prepare()
        x.launch_to_world("0", "drain")
        t0 = p.now()
        while True:
            b = p.batt()
            x.log(f"  放电中: {b['capacity']}% {b['current_ua']} µA {b['temp_c']}°C")
            if (b["capacity"] or 100) <= cfg["power"]["drain_target"]:
                break
            if p.now() - t0 > cfg["power"]["drain_max_s"]:
                raise Abort("放电超时")
            if not p.pidof():
                x.launch_to_world("0", "drain_again")
            p.sleep(60)
    except (Abort, UiStop) as e:
        x.log(f"!! {e}")
    finally:
        x.restore()


def cmd_iq(a, cfg):
    out = new_out(a, "iq")
    p = make_phone(a, cfg, out, False)
    x = Exp(a, p, cfg, out)
    json.dump({"mode": "iq", "config": cfg, "pf": a.pf}, open(os.path.join(out, "plan.json"), "w"), ensure_ascii=False)
    try:
        x.preflight(strict_power=False)
        x.prepare()
        x.iq_session()
    except (Abort, UiStop) as e:
        x.log(f"!! {e}")
    finally:
        x.restore()
    n3_judge.judge_images(a.pf, out, cfg)


def main():
    ap = argparse.ArgumentParser(description="原神 NPU 超分三臂实验 (N3)")
    ap.add_argument("cmd", choices=["dry-run", "smoke", "ui-probe", "drain", "run", "iq", "restore", "judge"])
    ap.add_argument("--out")
    ap.add_argument("--config", default=os.path.join(HERE, "n3_config.json"))
    ap.add_argument("--serial")
    ap.add_argument("--pf", default=DEF_PF)
    ap.add_argument("--chain", default=DEF_CHAIN)
    ap.add_argument("--layer-so", default=DEF_SO)
    ap.add_argument("--model", default=DEF_MODEL)
    ap.add_argument("--npu-deploy", default=None, help="N1 的 NPU 依赖布置脚本, 以 `<脚本> install` 调用, 环境变量 SERIAL PKG LIBDIR DEVDIR MODEL")
    ap.add_argument("--blocks", type=int, default=6)
    ap.add_argument("--seed", type=int)
    ap.add_argument("--perf-first", action="store_true", help="画质只作参考, 不作为判定门槛")
    ap.add_argument("--smoke", action="store_true", help="dry-run 时模拟 smoke 而不是 run")
    ap.add_argument("--try-switch", action="store_true")
    ap.add_argument("--resume", action="store_true")
    ap.add_argument("--no-restore", action="store_true")
    ap.add_argument("--keep-setting", action="store_true")
    a = ap.parse_args()
    cfg = json.load(open(a.config, encoding="utf-8"))
    if a.cmd == "dry-run":
        return cmd_smoke(a, cfg, dry=True) if a.smoke else cmd_run(a, cfg, dry=True)
    if a.cmd == "run":
        return cmd_run(a, cfg)
    if a.cmd == "smoke":
        return cmd_smoke(a, cfg)
    if a.cmd == "ui-probe":
        return cmd_ui_probe(a, cfg)
    if a.cmd == "drain":
        return cmd_drain(a, cfg)
    if a.cmd == "iq":
        return cmd_iq(a, cfg)
    if a.cmd == "restore":
        if not a.out:
            sys.exit("restore 要 --out <那次运行的输出目录>")
        p = make_phone(a, cfg, a.out, False)
        Exp(a, p, cfg, a.out).restore()
        return 0
    if a.cmd == "judge":
        if not a.out:
            sys.exit("judge 要 --out")
        return subprocess.run([sys.executable, os.path.join(HERE, "n3_judge.py"), a.out, "--pf", a.pf]).returncode


if __name__ == "__main__":
    sys.exit(main() or 0)
