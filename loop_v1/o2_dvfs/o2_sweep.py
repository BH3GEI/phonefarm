#!/usr/bin/env python3
"""O2: 原神锁 30/60 帧时, 把 CPU/GPU 频率上限压到「刚好够用」—— 扫参与确认的主机端外壳。

用 phonefarm eval (kind=sysparam) 跑每一个候选: 进窗后施加、窗口内每 3 秒回读、按守住比例
剔除被厂商服务抢回的改写臂 (fix-B), 臂序随机 (fix-F), 功耗只认放电态 (fix-C/D)。
本脚本只负责: 会话前后记原值与还原、切原神帧率设置、(可选) 挂厂商配置副本、
游戏运行时探白名单、按阶梯生成候选请求、读回包做筛选与判定。

子命令 (都支持 --dry-run: 只打印要执行的命令、写出请求文件, 不碰手机):
  plan     [--cap 60|30]                  候选清单与时长估计 (纯离线)
  recon    --out DIR [--watch S] [--attrib S]   厂商服务只读调查 (vendor_recon.sh)
  screen   --cap 60|30 --out DIR          初筛: 每轴阶梯下探 + 组合, 每个候选 stage=screen 2 对
  confirm  --cap 60|30 --out DIR (--from-screen DIR | --cand JSON ...) [--rounds 6]
  restore  --out DIR                      按会话记录还原 (节点 / 原神设置 / 厂商配置)
  selftest                                离线自测 (不碰手机)

碰手机的子命令要在设备锁里跑:
  tools/devlock run o2-dvfs -- python3 -I loop_v1/o2_dvfs/o2_sweep.py screen --cap 60 --out ...
"""
import argparse
import json
import math
import os
import random
import shlex
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
PKG = "com.miHoYo.Yuanshen"
PREFS = f"/data/data/{PKG}/shared_prefs/{PKG}.v2.playerprefs.xml"
DEV = "/data/local/tmp/o2"
CHARGE_NODE = "/sys/class/qcom-battery/charging_enabled"
WORKLOAD = "loop_v1/scripts/workload_spin_touch_v1.json"          # 标称 40 s: 撑得住 6 s + 30 s 的初筛窗
WORKLOAD_LONG = "loop_v1/scripts/workload_spin_touch_long_v1.json"  # 标称 100 s: 确认阶段 60 s 窗 (eval 会拒绝撑不满的组合)

# ── 频率表: 2026-10-12 红魔 NX809J probe (megacity-power-1012c/inputs_probe/probe.txt), 实测 ──
P0_FREQS = [384000, 537600, 691200, 787200, 883200, 998400, 1113600, 1228800, 1324800, 1440000,
            1555200, 1670400, 1785600, 1900800, 1996800, 2112000, 2227200, 2361600, 2496000, 2611200,
            2745600, 2899200, 3033600, 3187200, 3302400, 3398400, 3513600, 3628800]
P6_FREQS = [768000, 883200, 1017600, 1132800, 1267200, 1382400, 1497600, 1632000, 1747200, 1862400,
            1977600, 2112000, 2227200, 2438400, 2668800, 2880000, 3129600, 3398400, 3648000, 3763200,
            3859200, 3974400, 4070400, 4185600, 4262400, 4339200, 4396800]
GPU_MHZ = [1200, 1050, 967, 902, 826, 726, 646, 578, 539, 500, 461, 422, 382, 342, 282, 222, 191, 160]

# 原神运行中厂商服务给的值 (runs_sysparam/genshin*/…/snap_at_run.txt 多数臂, 实测; 开跑时以探测为准)
ARCHIVED_STOCK = {"cpu.policy0.scaling_max_freq": "1228800",
                  "cpu.policy6.scaling_max_freq": "1382400",
                  "gpu.max_pwrlevel": "0"}
# 下探不低于各自 scaling_min (同一批快照: policy0 787200, policy6 883200), 不动 min
FLOORS = {"cpu.policy0.scaling_max_freq": 787200, "cpu.policy6.scaling_max_freq": 883200}
# GPU 下探到哪一档为止 (数字越大越慢)。60 帧时 GPU 每帧忙 ~13 ms (gpu_active_mean, 实测), 余量小
GPU_DEEPEST = {60: 8, 30: 13}

AXES = {
    "cpu0": ("cpu.policy0.scaling_max_freq", "/sys/devices/system/cpu/cpufreq/policy0/scaling_max_freq"),
    "cpu6": ("cpu.policy6.scaling_max_freq", "/sys/devices/system/cpu/cpufreq/policy6/scaling_max_freq"),
    "gpu": ("gpu.max_pwrlevel", "/sys/class/kgsl/kgsl-3d0/max_pwrlevel"),
}

# frame_p95 的最小可检出效应: 原神 60 帧 33 个对照臂, 臂间标准差 0.442 ms, 均值 19.50 ms (实测);
# 5 对、改善侧 alpha/2 (z=2.24)、功效 0.8 (z=0.84): 3.08*sd*sqrt(2/5) = 0.86 ms = 4.4%
P95_SD_HIST_REL = 0.442 / 19.50
Z_SUM = 2.24 + 0.84
ARM_MIN = 3.0          # 每臂分钟数估计 (等冷 + 6 s 引导 + 30 s 采集 + 拉 trace/解析)
PROBE_MIN, ONBOARD_MIN, PREP_MIN = 6.0, 3.0, 3.0


def mde_ms(sd_ms, pairs=5):
    return Z_SUM * sd_ms * math.sqrt(2.0 / pairs)


# ══════════════ 候选生成 ══════════════

def axis_values(axis, stock, cap):
    """该轴从厂商给的值往下的候选 (降序, 不含 stock 本身)。"""
    if axis == "gpu":
        lv0 = int(stock)
        return [str(l) for l in range(lv0 + 1, GPU_DEEPEST[cap] + 1)]
    key = AXES[axis][0]
    table = P0_FREQS if axis == "cpu0" else P6_FREQS
    s = int(stock)
    return [str(f) for f in sorted(table, reverse=True) if f < s and f >= FLOORS[key]]


def coarse_order(vals, step=2):
    """阶梯先隔一档走 (省一半臂), 失败时再补中间那一档。回先走的那几档。"""
    out = vals[step - 1::step]
    if vals and (not out or out[-1] != vals[-1]):
        out.append(vals[-1])
    return out


def describe(params):
    out = []
    for k, v in params.items():
        if k == "gpu.max_pwrlevel":
            out.append(f"GPU≤{GPU_MHZ[int(v)]}MHz(lv{v})")
        elif not str(v).isdigit():   # governor 之类的非频率键 (确认阶段 --cand 可以给任意白名单键)
            out.append(f"{k}={v}")
        else:
            out.append(f"{k.split('.')[1]}≤{int(v) / 1e6:.2f}GHz")
    return " + ".join(out) if out else "厂商原样"


def plan_text(cap, stock=None):
    stock = stock or ARCHIVED_STOCK
    lines = [f"## 锁 {cap} 帧 · 厂商给的起点: " + describe(stock)]
    worst = 0
    for ax, (key, _) in AXES.items():
        vals = axis_values(ax, stock[key], cap)
        co = coarse_order(vals)
        n = len(co) + (1 if len(vals) > len(co) else 0)
        worst += n
        lines.append(f"- {ax} {key}: 候选 {vals} ; 先走 {co} ; 最多 {n} 个候选")
    worst += 2
    lines.append(f"- 组合: 各轴下限一起 (floor) + 各轴下限上抬一档 (margin), 2 个候选")
    scr = worst * 4 * ARM_MIN + PROBE_MIN + ONBOARD_MIN + PREP_MIN
    typ = (worst * 0.65) * 4 * ARM_MIN + PROBE_MIN + ONBOARD_MIN + PREP_MIN
    conf = 2 * 6 * 2 * ARM_MIN + PROBE_MIN + ONBOARD_MIN + PREP_MIN
    lines.append(f"- 初筛: 每候选 stage=screen 2 对 = 4 臂 ≈ {4 * ARM_MIN:.0f} 分钟; "
                 f"最多 {worst} 个候选 ≈ {scr / 60:.1f} 小时, 一般 ≈ {typ / 60:.1f} 小时 (不含充电等待)")
    lines.append(f"- 确认: 1–2 个候选 × 6 对 (12 臂) ≈ {conf / 60:.1f} 小时 (2 个候选时)")
    return "\n".join(lines)


# ══════════════ 请求 ══════════════

# SoC 中止温度: 2026-10-21 用户定为 80 °C (连续两次采样确认), 原 70 °C 作废
TEMP_CAP_C = 80.0


def make_request(cid, params, whitelist, cap, stage, rounds, cool_c, seed, why, seconds=30):
    req = {
        "version": 2, "kind": "sysparam", "candidate_id": cid, "package": PKG,
        "workload": {"carrier": "game_replay", "seconds": seconds, "script": WORKLOAD if seconds <= 30 else WORKLOAD_LONG,
                     "render_comm": "UnityGfxDeviceW"},
        "conditions": {"cool_c": cool_c, "temp_cap_c": TEMP_CAP_C, "require_root": True,
                       "require_discharging": True,
                       "charging_suspend": {"enabled": True, "node": CHARGE_NODE}, "fan": "off"},
        "protocol": {"cool_c": cool_c, "replay_seconds": seconds, "rounds": rounds, "alternating": True,
                     "p_threshold": 0.05, "stat": "exact_permutation",
                     "multiple_comparison": "bonferroni_on_wins",
                     "metrics": [{"id": "fps_p95_ms", "better": "lower"}, {"id": "fps_mean", "better": "higher"},
                                 {"id": "power_watt", "better": "lower"}],
                     "stage": stage, "sysparam_hold_min": 0.9, "order_seed": seed},
        "payload": {"why": why, "params": params,
                    "deny_keywords": ["thermal", "trip_point", "cooling", "fan", "tsens", "bcl", "throttl"]},
        "o2": {"cap_fps": cap},
    }
    if whitelist:
        req["payload"]["whitelist"] = whitelist
    else:
        req["payload"]["probe_only"] = True
    return req


def archived_whitelist(stock=None):
    """dry-run 用: 用存档频率表拼一份白名单 (真跑时一律用游戏运行时探出来的)。"""
    stock = stock or ARCHIVED_STOCK
    return {
        "cpu.policy0.scaling_max_freq": {"kind": "sysfs", "path": AXES["cpu0"][1], "values": [str(x) for x in P0_FREQS],
                                         "current": stock["cpu.policy0.scaling_max_freq"], "group": "cpu.policy0.freq", "effect": "live"},
        "cpu.policy6.scaling_max_freq": {"kind": "sysfs", "path": AXES["cpu6"][1], "values": [str(x) for x in P6_FREQS],
                                         "current": stock["cpu.policy6.scaling_max_freq"], "group": "cpu.policy6.freq", "effect": "live"},
        "gpu.max_pwrlevel": {"kind": "sysfs", "path": AXES["gpu"][1], "values": [str(i) for i in range(18)],
                             "current": stock["gpu.max_pwrlevel"], "group": "gpu.pwrlevel", "effect": "live"},
    }


# ══════════════ 读回包 ══════════════

def pm(report, metric, field):
    v = (((report.get("verdict") or {}).get("per_metric") or {}).get(metric) or {}).get(field)
    return v if isinstance(v, (int, float)) else None


def arm_summaries(evid_dir):
    """逐臂 summary.json (frame_p95 / frame_p99), 键 ('knob'|'ctrl', 轮)。"""
    out = {}
    d = Path(evid_dir)
    if not d.is_dir():
        return out
    for sub in d.iterdir():
        for arm in ("knob", "ctrl"):
            if sub.name.startswith(arm) and sub.name[len(arm):].isdigit():
                f = sub / "summary.json"
                try:
                    out[(arm, int(sub.name[len(arm):]))] = json.loads(f.read_text())
                except (OSError, ValueError):
                    pass
    return out


def mean(xs):
    xs = [x for x in xs if isinstance(x, (int, float))]
    return sum(xs) / len(xs) if xs else None


def sd(xs):
    xs = [x for x in xs if isinstance(x, (int, float))]
    if len(xs) < 2:
        return None
    m = sum(xs) / len(xs)
    return math.sqrt(sum((x - m) ** 2 for x in xs) / (len(xs) - 1))


def p95_tolerance(ctrl_p95s, ctrl_mean):
    """初筛的帧时容差 = 确认阶段 (5 对) 能检出的最小效应。对照臂够 6 个就用当天的离散度, 否则用历史相对值。"""
    hist = mde_ms(P95_SD_HIST_REL * ctrl_mean) if ctrl_mean else None
    s = sd(ctrl_p95s)
    today = mde_ms(s) if s is not None and len(ctrl_p95s) >= 6 else None
    vals = [v for v in (hist, today) if v is not None]
    return max(vals) if vals else None


def screen_verdict(report, arms, cap, tol_p95):
    """初筛一个候选: pass / fail / contested / invalid, 附原因。"""
    co = report.get("conditions_observed") or {}
    if report.get("status") == "ABORT" or not (report.get("verdict") or {}).get("per_metric"):
        return "invalid", f"没出判定: {(report.get('verdict') or {}).get('reason', '')}"
    if (co.get("contested_arms") or 0) > 0:
        return "contested", f"{co['contested_arms']} 个改写臂没守住 (厂商服务抢回), 该节点要先接管厂商配置"
    d_fps, b_fps = pm(report, "fps_mean", "diff_mean"), pm(report, "fps_mean", "b_mean")
    d_p95 = pm(report, "frame_p95", "diff_mean")
    if None in (d_fps, b_fps, d_p95):
        return "invalid", "帧率或帧时缺数据"
    why = []
    if b_fps < 0.98 * cap or d_fps < -0.01 * cap:
        why.append(f"帧率没守住 (候选 {b_fps:.2f}, 差 {d_fps:+.2f})")
    if tol_p95 is not None and d_p95 > tol_p95:
        why.append(f"frame_p95 +{d_p95:.2f} ms 超过容差 {tol_p95:.2f} ms")
    k99 = mean([s.get("frame_p99") for (a, _), s in arms.items() if a == "knob"])
    c99 = mean([s.get("frame_p99") for (a, _), s in arms.items() if a == "ctrl"])
    if k99 is not None and c99 is not None and tol_p95 is not None and k99 - c99 > 2 * tol_p95:
        why.append(f"frame_p99 +{k99 - c99:.2f} ms (卡顿) 超过 2 倍容差")
    return ("fail" if why else "pass"), "; ".join(why) or "帧率守住, 帧时在容差内"


def confirm_verdict(report, cap, tol_p95_rel):
    """确认阶段判定 (见 REPORT 的判定规则)。回 (结论, 理由)。"""
    v = report.get("verdict") or {}
    co = report.get("conditions_observed") or {}
    if report.get("status") == "ABORT" or not v.get("per_metric"):
        return "无法判定", f"没出判定: {v.get('reason', '')}"
    pairs = (v.get("power_pairs") or {})
    n_power = pairs.get("n_pairs", pairs.get("pairs"))
    if "power_w_mean" not in (v.get("per_metric") or {}):
        return "无法判定", "功耗没进判定表 (两臂不在同一放电口径), 只报帧时: " + json.dumps(v["per_metric"].get("frame_p95"), ensure_ascii=False)
    if isinstance(n_power, int) and n_power < 5:
        return "无法判定", f"功耗有效对数 {n_power} < 5"
    if (co.get("contested_arms") or 0) > 1:
        return "无法判定", f"{co['contested_arms']} 个改写臂被厂商服务抢回"
    if v.get("regressions"):
        return "否决", f"显著变差: {v['regressions']}"
    b_fps = pm(report, "fps_mean", "b_mean")
    a_p95, d_p95 = pm(report, "frame_p95", "a_mean"), pm(report, "frame_p95", "diff_mean")
    if b_fps is not None and b_fps < 0.98 * cap:
        return "否决", f"候选臂平均帧率 {b_fps:.2f} 低于锁帧 {cap} 的 98%"
    if a_p95 and d_p95 is not None and d_p95 > tol_p95_rel * a_p95:
        return "否决", f"frame_p95 点估计 +{d_p95:.2f} ms 超过 {tol_p95_rel:.1%} (不显著也不收)"
    if "power_w_mean" in (v.get("wins") or []):
        d = pm(report, "power_w_mean", "diff_mean")
        ci = pm(report, "power_w_mean", "ci95")
        return "增益", f"功耗 {d:+.3f} W 显著 (p={pm(report, 'power_w_mean', 'p')}), 帧时 {d_p95:+.2f} ms, 帧率守住 {ci or ''}"
    return "没有增益", "功耗没有显著下降 (p={})".format(pm(report, "power_w_mean", "p"))


# ══════════════ 设备 ══════════════

class Dev:
    def __init__(self, serial, dry, log):
        self.serial, self.dry, self.log = serial, dry, log

    def note(self, s):
        print(s, flush=True)
        with open(self.log, "a") as f:
            f.write(s + "\n")

    def run(self, argv, timeout=600, check=False):
        self.note("$ " + " ".join(shlex.quote(a) for a in argv))
        if self.dry:
            return ""
        r = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
        out = (r.stdout or "") + (r.stderr or "")
        with open(self.log, "a") as f:
            f.write(out[-4000:] + "\n")
        if check and r.returncode != 0:
            raise RuntimeError(f"命令失败 rc={r.returncode}: {' '.join(argv)}\n{out[-2000:]}")
        return out

    def adb(self, *a, **kw):
        return self.run(["adb", "-s", self.serial, *a], **kw)

    def su(self, cmd, **kw):
        return self.adb("shell", "su", "-c", shlex.quote(cmd), **kw)

    def pull_su(self, remote, local):
        """root 才读得到的文件: su cat 出来写到本机新文件 (adb pull 以 shell 身份读不到)。"""
        # exec-out 不经设备端 sh 拆参数: su -c 与命令必须合成一个参数 (2026-10-22 真机, 分开传会把引号原样交给 su)
        argv = ["adb", "-s", self.serial, "exec-out", "su -c " + shlex.quote(f"cat {remote}")]
        self.note("$ " + " ".join(argv) + f" > {local}")
        if self.dry:
            return
        if Path(local).exists():
            raise RuntimeError(f"{local} 已存在, 不覆盖")
        with open(local, "wb") as f:
            subprocess.run(argv, stdout=f, timeout=300, check=True)
        head = Path(local).read_bytes()[:200]
        if head.startswith(b"/system/bin/sh:") or b"inaccessible or not found" in head:
            raise RuntimeError(f"从设备读 {remote} 失败, 拿到的是报错: {head[:120]!r}")

    def push_su(self, local, remote):
        """推到 root 目录: 先推到 /data/local/tmp 下的中转名, 再 su cp 过去 (中转件留着, 不删)。"""
        tmp = f"/data/local/tmp/o2_in_{time.strftime('%Y%m%d-%H%M%S')}_{Path(remote).name}"
        self.adb("push", str(local), tmp, check=not self.dry)
        return self.su(f"cp {tmp} {remote} && sha256sum {remote}", check=not self.dry)

    def pull_dir_su(self, remote, local_dir):
        """root 目录整个打包拉回并解开到本机新目录。"""
        tar = Path(local_dir).with_suffix(".tar")
        argv = ["adb", "-s", self.serial, "exec-out", "su -c " + shlex.quote(f"tar -C {remote} -cf - .")]
        self.note("$ " + " ".join(argv) + f" > {tar}  (再解到 {local_dir})")
        if self.dry:
            return
        with open(tar, "wb") as f:
            subprocess.run(argv, stdout=f, timeout=900, check=True)
        import tarfile
        Path(local_dir).mkdir(parents=True, exist_ok=False)
        with tarfile.open(tar) as t:
            t.extractall(local_dir, filter="data")


# 缺省用本脚本所在仓库的根和二进制。以前写死指向旧 worktree (_wt/integration): eval 嵌套拿锁时走那边的
# 旧 tools/devlock, 它直接改旧锁域, 把租约存储的退役围栏当过期锁抢走 (2026-10-23 两次), 所有设备锁随即失效;
# 回放也用了那边带 --app 遥测的旧 run_once.sh。
REPO = HERE.parent.parent


def pf_bin():
    return os.environ.get("PF_BIN", str(REPO / "src/target/release/phonefarm"))


def pf_root():
    return os.environ.get("PF_REPO_ROOT", str(REPO))


class Session:
    def __init__(self, a, kind):
        self.a = a
        self.out = Path(a.out).resolve()
        if self.out.exists() and any(self.out.iterdir()):
            sys.exit(f"{self.out} 已存在且非空; 换一个新目录 (不覆盖旧数据)")
        for sub in ("requests", "reports", "evid"):
            (self.out / sub).mkdir(parents=True, exist_ok=True)
        self.dev = Dev(a.serial, a.dry_run, self.out / "commands.log")
        self.sid = time.strftime("%Y%m%d-%H%M%S") + f"-{kind}{a.cap}"
        self.ddir = f"{DEV}/{self.sid}"
        self.state = {"session": self.sid, "kind": kind, "cap": a.cap, "serial": a.serial, "dry_run": a.dry_run,
                      "device_dir": self.ddir, "overrides": [], "restored": None, "results": []}
        self.save()
        if not a.dry_run and not os.environ.get("DEVLOCK_LABEL"):
            sys.exit("碰手机的步骤要在设备锁里跑: tools/devlock run o2-dvfs -- python3 -I " + " ".join(sys.argv))

    def save(self):
        (self.out / "session.json").write_text(json.dumps(self.state, ensure_ascii=False, indent=1))

    # ── 准备 ──
    def prepare(self):
        d = self.dev
        d.adb("shell", "su", "-c", shlex.quote(f"mkdir -p {self.ddir} {DEV}/vendor_edit"), check=not self.a.dry_run)
        for f in ("o2_nodes.sh", "vendor_override.sh", "vendor_recon.sh"):
            tmpn = f if f.startswith("o2_") else f"o2_{f}"
            d.adb("push", str(HERE / f), f"/data/local/tmp/{tmpn}", check=not self.a.dry_run)
            d.su(f"cp /data/local/tmp/{tmpn} {DEV}/{f}")
        d.adb("shell", f"am force-stop {PKG}")
        time.sleep(0 if self.a.dry_run else 5)
        # 1) 频率节点原值 (游戏停着时)
        d.su(f"sh {DEV}/o2_nodes.sh save {self.ddir}/nodes_pre.txt", check=not self.a.dry_run)
        d.su(f"cat {CHARGE_NODE} > {self.ddir}/charging_enabled_pre.txt")
        d.pull_su(f"{self.ddir}/nodes_pre.txt", str(self.out / "nodes_pre.txt"))
        self.state["nodes_pre"] = f"{self.ddir}/nodes_pre.txt"
        # 先撤掉之前会话可能钉在 sysfs 上的约束, 游戏启动时厂商从干净状态下发配置 (见 o2_nodes.sh 头注释)
        rel = d.su(f"sh {DEV}/o2_nodes.sh release {self.ddir}/nodes_pre.txt")
        self.state["release_at_start"] = rel.strip()[-300:]
        # 2) 原神帧率设置: 原文件留底 (设备上一份、本机一份), 再写锁帧档
        d.su(f"cp -p {PREFS} {self.ddir}/prefs_orig.xml", check=not self.a.dry_run)
        d.pull_su(f"{self.ddir}/prefs_orig.xml", str(self.out / "prefs_orig.xml"))
        self.state["prefs_orig"] = f"{self.ddir}/prefs_orig.xml"
        self.save()
        idx = self.a.fps_index if self.a.fps_index is not None else {60: 2, 30: 0}[self.a.cap]
        if not self.a.dry_run:
            src = (self.out / "prefs_orig.xml").read_text(encoding="utf-8")
            new, before = set_fps_index(src, idx)
            self.state["prefs_fps_index"] = {"before": before, "after": idx}
            if before != idx:
                (self.out / "prefs_cap.xml").write_text(new, encoding="utf-8")
                d.push_su(self.out / "prefs_cap.xml", f"{self.ddir}/prefs_cap.xml")
                d.su(f"cat {self.ddir}/prefs_cap.xml > {PREFS}", check=True)
                self.state["prefs_written"] = True
        else:
            d.note(f"(dry-run) 原神 GENERAL_DATA entryType 1 (帧率) 改成 index {idx}, 写回 {PREFS}")
            self.state["prefs_written"] = True
        self.save()
        # 3) 厂商配置副本 (可选)
        for ov in self.overrides():
            d.push_su(ov["edited_local"], ov["edited_device"])
            out = d.su(f"sh {DEV}/vendor_override.sh apply {ov['target']} {ov['edited_device']} {ov['svc']}")
            self.state["overrides"].append(ov)
            self.save()
            if not self.a.dry_run and "O2_OVERRIDE_APPLIED" not in out:
                raise RuntimeError(f"厂商配置副本没挂上: {out[-800:]}")
        # 4) 进大世界
        self.enter_world()

    def overrides(self):
        if not self.a.override:
            return []
        spec = json.loads(Path(self.a.override).read_text())
        out = []
        for o in spec:
            out.append({"target": o["target"], "edited_local": o["edited_local"], "svc": o["svc"],
                        "edited_device": f"{DEV}/vendor_edit/{Path(o['edited_local']).name}"})
        return out

    def enter_world(self):
        self.dev.run([pf_bin(), "onboard", "--package", PKG, "--serial", self.a.serial, "--enter-only",
                      "--no-guest", "--out", str(self.out / f"onboard-{int(time.time())}")], timeout=900)

    def game_up(self):
        if self.a.dry_run:
            return True
        pid = self.dev.adb("shell", f"pidof {PKG}").strip()
        fg = self.dev.adb("shell", "dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'")
        return bool(pid) and PKG in fg

    def battery_gate(self, lo, hi):
        """开每个请求前: 电量在 [lo, hi] 之间。低了就开着充电等 (eval 结束时已恢复充电), 满了就提示。"""
        if self.a.dry_run:
            self.dev.note(f"(dry-run) 电量检查: 需在 {lo}–{hi}% 之间")
            return
        t0 = time.time()
        while True:
            out = self.dev.adb("shell", "dumpsys battery")
            lvl = next((int(l.split(":")[1]) for l in out.splitlines() if l.strip().startswith("level:")), None)
            if lvl is None or lo <= lvl <= hi:
                return
            if lvl > hi:
                self.dev.note(f"电量 {lvl}% > {hi}%: 满电时供电轨会在电池和旁路间跳, 第一臂起会停充放电, 先照跑, 回包里看 power_pairs")
                return
            if time.time() - t0 > 3 * 3600:
                raise RuntimeError(f"充了 3 小时电量还是 {lvl}%")
            self.dev.note(f"电量 {lvl}% < {lo}%, 充电中等到 {lo + 25}% ...")
            self.dev.su(f"echo 1 > {CHARGE_NODE}")
            while True:
                time.sleep(120)
                out = self.dev.adb("shell", "dumpsys battery")
                lvl = next((int(l.split(":")[1]) for l in out.splitlines() if l.strip().startswith("level:")), 0)
                if lvl >= lo + 25 or time.time() - t0 > 3 * 3600:
                    break

    def thermal_gate(self, timeout_s=1800):
        """开每个请求前: 停充, 并等 GPU 温控档回到 0 (连续两次, 间隔 30 秒)。

        2026-10-22 红魔实测: 满电还接着充电时, 厂商温控在 SoC 只有 44 °C 时就把 GPU 压到
        thermal_pwrlevel 7、CPU 上限压到最低档; 在这种状态下探出来的「厂商起点」与停充后的
        测量条件不是一回事, 整轮初筛作废。停充由 eval 结束时 / restore 恢复 (do_restore 会把
        charging_enabled 改回 1)。"""
        if self.a.dry_run:
            self.dev.note("(dry-run) 温控关: 停充, 等 thermal_pwrlevel 连续两次为 0")
            return
        node = "/sys/class/kgsl/kgsl-3d0/thermal_pwrlevel"
        if self.dev.su(f"cat {CHARGE_NODE}").strip() != "0":
            self.dev.su(f"echo 0 > {CHARGE_NODE}")
            self.dev.note("温控关: 先停充 (充电发热会让厂商温控提前压频)")
        t0, ok = time.time(), 0
        while True:
            lv = self.dev.su(f"cat {node}").strip()
            ok = ok + 1 if lv == "0" else 0
            if ok >= 2:
                return
            if time.time() - t0 > timeout_s:
                raise RuntimeError(f"等了 {timeout_s // 60} 分钟 GPU 温控档还是 {lv}, 手机在温控降频, 不开跑")
            if ok == 0:
                self.dev.note(f"温控关: GPU thermal_pwrlevel={lv}, 等它回到 0 ...")
            time.sleep(30)

    # ── 白名单 (游戏运行时探) ──
    def probe(self):
        if not self.game_up():
            self.enter_world()
        req = make_request("o2_probe", {}, None, self.a.cap, "screen", 1, self.a.cool_c, 0, "游戏运行时探白名单 (fix-B)")
        rep = self.eval(req)
        if self.a.dry_run:
            wl = archived_whitelist()
        else:
            wl = (rep.get("conditions_observed") or {}).get("whitelist") or {}
        (self.out / "whitelist.json").write_text(json.dumps(wl, ensure_ascii=False, indent=1))
        missing = [AXES[ax][0] for ax in self.axes() if AXES[ax][0] not in wl]
        self.state["probe"] = {"missing": missing, "stock": {k: (wl.get(k) or {}).get("current") for k, _ in AXES.values()}}
        self.save()
        # 注意: 红魔的 gpu.max_pwrlevel 是厂商每几秒改一次的动态值 (2026-10-22 实测 0↔17), 探测瞬间的
        # current 不代表「厂商起点」; GPU 轴大概率会被 fix-B 的守住比例判成 contested。
        if missing:
            self.dev.note(f"游戏运行时这些节点没进白名单 (厂商服务抢回或不可写): {missing}。"
                          "要扫它们得先用 vendor_override.sh 接管厂商配置, 再重开会话 (--override)。")
        return wl, missing

    def axes(self):
        return [x for x in self.a.axes.split(",") if x]

    def eval(self, req):
        cid = req["candidate_id"]
        req = dict(req, serial=self.a.serial)
        rq = self.out / "requests" / f"{cid}.json"
        rq.write_text(json.dumps(req, ensure_ascii=False, indent=1))
        rp = self.out / "reports" / f"{cid}.json"
        if not self.a.dry_run and not self.game_up():
            self.enter_world()
        self.battery_gate(self.a.battery_min, 99)
        self.thermal_gate()
        env_note = f"PF_REPO_ROOT={pf_root()} "
        self.dev.note(env_note + "(eval 在下一行)")
        argv = [pf_bin(), "eval", "--request", str(rq), "--serial", self.a.serial, "--out", str(self.out / "evid" / cid)]
        if req["protocol"].get("stage") == "full":
            argv.append("--no-curtail")
        if self.a.dry_run:
            self.dev.run(argv)
            return {}
        r = subprocess.run(argv, capture_output=True, text=True, timeout=4 * 3600,
                           env=dict(os.environ, PF_REPO_ROOT=pf_root()))
        rp.write_text(r.stdout or "{}")
        (self.out / "reports" / f"{cid}.stderr.txt").write_text(r.stderr or "")
        try:
            return json.loads(r.stdout)
        except ValueError:
            return {"status": "ABORT", "verdict": {"reason": f"eval 没回 JSON (rc={r.returncode})"}}

    # ── 收尾 ──
    def restore(self):
        do_restore(self.dev, self.state, self.out)
        self.save()


def set_fps_index(xml_text, idx):
    """原神 GENERAL_DATA 里 entryType 1 (帧率) 的 index 改成 idx, 其余字节不动。回 (新文本, 原 index)。"""
    import re
    import urllib.parse
    m = re.search(r'name="GENERAL_DATA">([^<]*)<', xml_text)
    if not m:
        raise ValueError("prefs 里没有 GENERAL_DATA")
    t = urllib.parse.unquote(m.group(1))
    if urllib.parse.quote(t, safe="") != m.group(1):
        raise ValueError("GENERAL_DATA 编码方式不是 quote(safe='')")
    pat = r'(\\"entryType\\":1,\\"index\\":)(\d+)'
    found = re.findall(pat, t)
    if len(found) != 1:
        raise ValueError(f"entryType 1 出现 {len(found)} 次")
    before = int(found[0][1])
    t2 = re.sub(pat, lambda mm: mm.group(1) + str(idx), t)
    return xml_text[:m.start(1)] + urllib.parse.quote(t2, safe="") + xml_text[m.end(1):], before


def do_restore(dev, st, out):
    """按会话记录还原。顺序: 停游戏 → 原神设置 → 厂商配置 → 频率节点 → 停充节点核对。"""
    res = {}
    dev.adb("shell", f"am force-stop {PKG}")
    time.sleep(0 if dev.dry else 5)
    if st.get("prefs_written") and st.get("prefs_orig"):
        o = dev.su(f"cat {st['prefs_orig']} > {PREFS} && sha256sum {st['prefs_orig']} {PREFS}")
        sums = [l.split()[0] for l in o.splitlines() if len(l.split()) == 2 and len(l.split()[0]) == 64]
        res["prefs"] = "ok" if dev.dry or (len(sums) == 2 and sums[0] == sums[1]) else f"FAIL {o[-300:]}"
    for ov in reversed(st.get("overrides") or []):
        o = dev.su(f"sh {DEV}/vendor_override.sh restore {ov['target']} {ov['svc']}")
        res[f"override:{ov['target']}"] = "ok" if dev.dry or "O2_RESTORE_OK" in o else f"FAIL {o[-300:]}"
    if st.get("nodes_pre"):
        time.sleep(0 if dev.dry else 10)
        # 撤掉约束而不是写回快照: 快照可能是在厂商限频时记的, 写回去会把限频值钉住 (见 o2_nodes.sh 头注释)。
        # 之后游戏再启动时厂商会重新下发它的游戏配置。
        o = dev.su(f"sh {DEV}/o2_nodes.sh release {st['nodes_pre']}")
        res["nodes"] = "ok" if dev.dry or "O2_RELEASED" in o else f"PARTIAL {o[-600:]}"
        o = dev.su(f"sh {DEV}/o2_nodes.sh check {st['nodes_pre']}")
        res["nodes_vs_snapshot"] = o.strip()[-600:]
    o = dev.su(f"cat {CHARGE_NODE}")
    res["charging_enabled_now"] = o.strip()
    if not dev.dry and o.strip() != "1":
        dev.su(f"echo 1 > {CHARGE_NODE}")
        res["charging_enabled_fix"] = dev.su(f"cat {CHARGE_NODE}").strip()
    st["restored"] = res
    dev.note("还原结果: " + json.dumps(res, ensure_ascii=False))


# ══════════════ 子命令 ══════════════

def cmd_plan(a):
    caps = [a.cap] if a.cap else [60, 30]
    for c in caps:
        print(plan_text(c))
        print()


def cmd_recon(a):
    out = Path(a.out).resolve()
    if out.exists() and any(out.iterdir()):
        sys.exit(f"{out} 已存在且非空")
    out.mkdir(parents=True, exist_ok=True)
    if not a.dry_run and not os.environ.get("DEVLOCK_LABEL"):
        sys.exit("要在设备锁里跑: tools/devlock run o2-dvfs -- python3 -I " + " ".join(sys.argv))
    d = Dev(a.serial, a.dry_run, out / "commands.log")
    ts = time.strftime("%Y%m%d-%H%M%S")
    rd = f"{DEV}/recon-{ts}"
    d.adb("push", str(HERE / "vendor_recon.sh"), "/data/local/tmp/o2_vendor_recon.sh")
    d.su(f"mkdir -p {DEV} && cp /data/local/tmp/o2_vendor_recon.sh {DEV}/vendor_recon.sh")
    d.su(f"sh {DEV}/vendor_recon.sh static {rd}", timeout=900)
    if a.watch:
        # 先停游戏, 开始盯节点, 10 秒后拉起原神, 盯满 a.watch 秒
        d.adb("shell", f"am force-stop {PKG}")
        w = subprocess.Popen(["adb", "-s", a.serial, "shell", "su", "-c",
                              shlex.quote(f"sh {DEV}/vendor_recon.sh watch {a.watch} {rd}/watch.txt")]) if not a.dry_run else None
        d.note(f"$ (后台) vendor_recon.sh watch {a.watch} {rd}/watch.txt; 10 秒后启动原神")
        time.sleep(0 if a.dry_run else 10)
        d.adb("shell", f"monkey -p {PKG} -c android.intent.category.LAUNCHER 1")
        if w:
            w.wait(timeout=a.watch + 120)
    if a.attrib:
        d.adb("shell", f"am force-stop {PKG}")
        w = subprocess.Popen(["adb", "-s", a.serial, "shell", "su", "-c",
                              shlex.quote(f"sh {DEV}/vendor_recon.sh attrib {a.attrib} {rd}/attrib.txt")]) if not a.dry_run else None
        d.note(f"$ (后台) vendor_recon.sh attrib {a.attrib} {rd}/attrib.txt; 10 秒后启动原神, 结束前停掉")
        time.sleep(0 if a.dry_run else 10)
        d.adb("shell", f"monkey -p {PKG} -c android.intent.category.LAUNCHER 1")
        time.sleep(0 if a.dry_run else max(a.attrib - 30, 10))
        d.adb("shell", f"am force-stop {PKG}")
        if w:
            w.wait(timeout=a.attrib + 120)
    d.pull_dir_su(rd, str(out / f"recon-{ts}"))
    d.note(f"拉回 {out}/recon-{ts}")


def run_staircase(s, wl, stock):
    """逐轴阶梯下探 (其余轴保持厂商原样)。回 {轴: 守得住的最低值或 None}, 以及每个候选的记录。"""
    cap, recs, floors, tol_state = s.a.cap, [], {}, {"ctrl_p95": []}

    def screen_one(cid, params, why):
        seed = random.SystemRandom().randrange(1 << 31)
        req = make_request(cid, params, wl, cap, "screen", 2, s.a.cool_c, seed, why, s.a.seconds)
        rep = s.eval(req)
        if s.a.dry_run:
            rec = {"cid": cid, "params": params, "desc": describe(params), "verdict": "pass", "why": "dry-run: 按通过展开"}
        else:
            arms = arm_summaries(s.out / "evid" / cid)
            tol_state["ctrl_p95"] += [v.get("frame_p95") for (arm, _), v in arms.items() if arm == "ctrl"]
            a_mean = pm(rep, "frame_p95", "a_mean")
            tol = p95_tolerance(tol_state["ctrl_p95"], a_mean)
            ver, why2 = screen_verdict(rep, arms, cap, tol)
            cap_seen = (rep.get("verdict") or {}).get("fps_locked_cap")
            rec = {"cid": cid, "params": params, "desc": describe(params), "verdict": ver, "why": why2,
                   "tol_p95_ms": tol, "fps_locked_cap": cap_seen,
                   "d_p95": pm(rep, "frame_p95", "diff_mean"), "d_fps": pm(rep, "fps_mean", "diff_mean"),
                   "d_power": pm(rep, "power_w_mean", "diff_mean"),
                   "hold": [(h.get("arm"), h.get("frac")) for h in (rep.get("conditions_observed") or {}).get("hold_per_arm", [])]}
            a_fps = pm(rep, "fps_mean", "a_mean")
            if a_fps is not None and abs(a_fps - cap) > 0.06 * cap:
                rec["verdict"], rec["why"] = "invalid", f"对照臂帧率 {a_fps:.1f} 不在锁帧 {cap} 附近: 帧率设置没生效, 停"
                recs.append(rec)
                raise RuntimeError(rec["why"])
        recs.append(rec)
        s.state["results"] = recs
        s.save()
        s.dev.note(f"[{rec['verdict']}] {cid} {rec['desc']}: {rec['why']}")
        return rec["verdict"]

    for ax in s.axes():
        key = AXES[ax][0]
        if key not in wl:
            floors[ax] = None
            continue
        vals = axis_values(ax, stock[key], cap)
        last_pass, last_pass_i, fail_i = None, -1, None
        for v in coarse_order(vals):
            i = vals.index(v)
            ver = screen_one(f"scr{cap}_{ax}_{v}", {key: v}, f"O2 初筛 {ax}: 锁 {cap} 帧时上限压到 {v}")
            if ver == "pass":
                last_pass, last_pass_i = v, i
            else:
                fail_i = i
                break
        # 失败点与上一个通过点之间隔了一档: 补测那一档
        if fail_i is not None and fail_i - last_pass_i == 2:
            v = vals[fail_i - 1]
            if screen_one(f"scr{cap}_{ax}_{v}", {key: v}, f"O2 初筛 {ax} 补档 {v}") == "pass":
                last_pass = v
        floors[ax] = last_pass
    # 组合: 各轴下限一起 / 各轴下限上抬一档
    got = {ax: v for ax, v in floors.items() if v is not None}
    combos = []
    if len(got) >= 2:
        floor_p = {AXES[ax][0]: v for ax, v in got.items()}
        margin_p = {}
        for ax, v in got.items():
            vals = axis_values(ax, stock[AXES[ax][0]], cap)
            i = vals.index(v)
            if i > 0:
                margin_p[AXES[ax][0]] = vals[i - 1]
        for name, p in (("floor", floor_p), ("margin", margin_p)):
            if len(p) >= 2:
                ver = screen_one(f"scr{cap}_combo_{name}", p, f"O2 初筛组合 {name}")
                combos.append((name, p, ver))
    return floors, combos, recs


def pick_confirm(floors, combos, recs):
    """确认阶段取 1–2 个: 先取通过的组合 (floor 优先), 不够再取单轴下限里功耗方向最好的。"""
    out = [p for name, p, ver in combos if ver == "pass"]
    if len(out) < 2:
        singles = [r for r in recs if r["verdict"] == "pass" and len(r["params"]) == 1
                   and r["params"] in [{AXES[ax][0]: v} for ax, v in floors.items() if v]]
        singles.sort(key=lambda r: r.get("d_power") if isinstance(r.get("d_power"), (int, float)) else 0.0)
        out += [r["params"] for r in singles[: 2 - len(out)]]
    return out[:2]


def cmd_screen(a):
    s = Session(a, "screen")
    try:
        s.prepare()
        wl, missing = s.probe()
        stock = {k: (wl.get(k) or {}).get("current") or ARCHIVED_STOCK[k] for k, _ in AXES.values()}
        s.dev.note(plan_text(a.cap, stock))
        floors, combos, recs = run_staircase(s, wl, stock)
        pick = pick_confirm(floors, combos, recs)
        summ = {"cap": a.cap, "stock_in_game": stock, "floors": floors,
                "combos": [{"name": n, "params": p, "verdict": v} for n, p, v in combos],
                "contested_or_missing": missing + [r["cid"] for r in recs if r["verdict"] == "contested"],
                "confirm_candidates": pick}
        (s.out / "screen_summary.json").write_text(json.dumps(summ, ensure_ascii=False, indent=1))
        s.dev.note("初筛结论: " + json.dumps(summ, ensure_ascii=False))
    finally:
        s.restore()


def cmd_confirm(a):
    if a.from_screen:
        cands = json.loads((Path(a.from_screen) / "screen_summary.json").read_text())["confirm_candidates"]
    else:
        cands = [json.loads(c) for c in (a.cand or [])]
    if not cands:
        sys.exit("没有候选: --from-screen <初筛目录> 或 --cand '{\"gpu.max_pwrlevel\":\"4\"}'")
    if a.rounds < 5:
        sys.exit("确认至少 5 对 (建议 6: 剔掉一对后仍够 5 对)")
    s = Session(a, "confirm")
    try:
        s.prepare()
        wl, missing = s.probe()
        need = sorted({k for c in cands for k in c})
        if any(k not in wl for k in need):
            raise RuntimeError(f"候选用到的节点游戏运行时不在白名单里: {[k for k in need if k not in wl]}")
        rows = []
        for i, p in enumerate(cands, 1):
            seed = random.SystemRandom().randrange(1 << 31)
            req = make_request(f"conf{a.cap}_{i}", p, wl, a.cap, "full", a.rounds, a.cool_c, seed,
                               f"O2 确认: 锁 {a.cap} 帧时 {describe(p)} 对厂商原样", a.seconds)
            s.battery_gate(max(a.battery_min, 50), 95)
            rep = s.eval(req)
            if a.dry_run:
                continue
            ver, why = confirm_verdict(rep, a.cap, a.p95_rel_tol)
            co = rep.get("conditions_observed") or {}
            rows.append({"params": p, "desc": describe(p), "conclusion": ver, "why": why,
                         "order_seed": seed, "contested_arms": co.get("contested_arms"),
                         "contested": co.get("contested"), "power_pairs": (rep.get("verdict") or {}).get("power_pairs"),
                         "per_metric": (rep.get("verdict") or {}).get("per_metric")})
            s.state["results"] = rows
            s.save()
            s.dev.note(f"[{ver}] {describe(p)}: {why}")
        (s.out / "confirm_summary.json").write_text(json.dumps(rows, ensure_ascii=False, indent=1))
    finally:
        s.restore()


def cmd_restore(a):
    out = Path(a.out).resolve()
    st = json.loads((out / "session.json").read_text())
    if not a.dry_run and not os.environ.get("DEVLOCK_LABEL"):
        sys.exit("要在设备锁里跑: tools/devlock run o2-dvfs -- python3 -I " + " ".join(sys.argv))
    dev = Dev(st["serial"], a.dry_run, out / "commands.log")
    do_restore(dev, st, out)
    if not a.dry_run:
        (out / "session.json").write_text(json.dumps(st, ensure_ascii=False, indent=1))


# ══════════════ 离线自测 ══════════════

def cmd_selftest(_a):
    ok = True

    def chk(name, cond):
        nonlocal ok
        print(("PASS " if cond else "FAIL ") + name)
        ok &= bool(cond)

    chk("cpu0 60 帧候选", axis_values("cpu0", "1228800", 60) == ["1113600", "998400", "883200", "787200"])
    chk("cpu6 候选不低于 min", axis_values("cpu6", "1382400", 30)[-1] == "883200")
    chk("gpu 60 帧到 lv8", axis_values("gpu", "0", 60) == [str(i) for i in range(1, 9)])
    chk("隔档先走且含最后一档", coarse_order(["a", "b", "c", "d", "e"]) == ["b", "d", "e"])
    chk("MDE 60 帧约 0.86 ms", abs(mde_ms(0.442) - 0.86) < 0.01)
    rep = {"status": "REJECT", "conditions_observed": {"contested_arms": 0},
           "verdict": {"per_metric": {"fps_mean": {"diff_mean": -0.1, "b_mean": 59.3, "a_mean": 59.4},
                                      "frame_p95": {"diff_mean": 0.3, "a_mean": 19.5}}}}
    chk("初筛通过", screen_verdict(rep, {}, 60, 0.86)[0] == "pass")
    rep["verdict"]["per_metric"]["frame_p95"]["diff_mean"] = 1.2
    chk("初筛帧时超容差", screen_verdict(rep, {}, 60, 0.86)[0] == "fail")
    rep["conditions_observed"]["contested_arms"] = 1
    chk("初筛被抢回", screen_verdict(rep, {}, 60, 0.86)[0] == "contested")
    full = {"status": "PASS", "conditions_observed": {"contested_arms": 0},
            "verdict": {"wins": ["power_w_mean"], "regressions": [], "power_pairs": {"n_pairs": 6},
                        "per_metric": {"fps_mean": {"b_mean": 59.4}, "frame_p95": {"a_mean": 19.5, "diff_mean": 0.1},
                                       "power_w_mean": {"diff_mean": -0.3, "p": 0.002}}}}
    chk("确认增益", confirm_verdict(full, 60, 0.02)[0] == "增益")
    full["verdict"]["per_metric"]["frame_p95"]["diff_mean"] = 0.5
    chk("确认: 帧时点估计超 2% 否决", confirm_verdict(full, 60, 0.02)[0] == "否决")
    del full["verdict"]["per_metric"]["power_w_mean"]
    chk("确认: 功耗不在表里无法判定", confirm_verdict(full, 60, 0.02)[0] == "无法判定")
    xml = '<map><string name="GENERAL_DATA">' + \
        __import__("urllib.parse").parse.quote('{"x":[{\\"entryType\\":1,\\"index\\":2}]}', safe="") + '</string></map>'
    new, before = set_fps_index(xml, 0)
    chk("帧率 index 只改一处", before == 2 and "%5C%22index%5C%22%3A0" in new and len(new) == len(xml))
    chk("确认取组合优先", pick_confirm({"gpu": "4"}, [("floor", {"a": 1, "b": 2}, "pass"), ("margin", {"a": 2}, "fail")],
                                    [{"verdict": "pass", "params": {"gpu.max_pwrlevel": "4"}, "d_power": -0.2}])
        == [{"a": 1, "b": 2}, {"gpu.max_pwrlevel": "4"}])
    sys.exit(0 if ok else 1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    def common(p, need_out=True, cap=True):
        p.add_argument("--serial", default=os.environ.get("SERIAL", "91253241019A"))
        p.add_argument("--dry-run", action="store_true")
        if need_out:
            p.add_argument("--out", required=True)
        if cap:
            p.add_argument("--cap", type=int, choices=[30, 60], required=True)
            p.add_argument("--axes", default="cpu0,cpu6,gpu")
            p.add_argument("--cool-c", type=float, default=50.0)
            p.add_argument("--fps-index", type=int, default=None,
                           help="原神帧率档 index (缺省 60→2 存档实测; 30→0 未验证, 首个请求会核对对照臂帧率)")
            p.add_argument("--override", default=None,
                           help="厂商配置副本清单 JSON: [{target, edited_local, svc}]")
            p.add_argument("--battery-min", type=int, default=30)

    p = sub.add_parser("plan")
    p.add_argument("--cap", type=int, choices=[30, 60])
    p = sub.add_parser("recon")
    common(p, cap=False)
    p.add_argument("--watch", type=int, default=0)
    p.add_argument("--attrib", type=int, default=0)
    p = sub.add_parser("screen")
    common(p)
    p.add_argument("--seconds", type=int, default=30, help="每臂采集秒数")
    p = sub.add_parser("confirm")
    common(p)
    p.add_argument("--from-screen")
    p.add_argument("--cand", action="append")
    p.add_argument("--rounds", type=int, default=6)
    # 电池电量计电流有十几秒平滑 (2026-10-20 实测), 确认阶段每臂 60 秒, 初筛 30 秒只作粗筛
    p.add_argument("--seconds", type=int, default=60, help="每臂采集秒数")
    p.add_argument("--p95-rel-tol", type=float, default=0.02)
    p = sub.add_parser("restore")
    common(p, cap=False)
    sub.add_parser("selftest")
    a = ap.parse_args()
    {"plan": cmd_plan, "recon": cmd_recon, "screen": cmd_screen, "confirm": cmd_confirm,
     "restore": cmd_restore, "selftest": cmd_selftest}[a.cmd](a)


if __name__ == "__main__":
    main()
