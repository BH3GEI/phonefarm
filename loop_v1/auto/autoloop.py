#!/usr/bin/env python3
"""autoloop.py — 系统参数的全自动优化闭环编排。

    真机探白名单 → 冻结判定规则 → [大模型挑一组参数 → 应用 → 等冷 → A/B 交替实测
                                   → 置换检验 → 保留/淘汰 → 还原] × N → 结果喂回模型

每一层的职责边界
----------------
- `whitelist.py` 决定「准改什么」, 不决定改成什么
- `llm.py`       决定「改成什么」, 不决定好不好
- `verdict.py`   决定「好不好」, 规则在看数据之前冻结
- 本文件只做编排与证据归档, 不含任何判定口径

复用而非重造: 负载回放与 ftrace 采集直接调 `tools/run_once.sh`, 统计用
`tools/analyze.py` 的精确置换检验, 快照比对用 `tools/device_snapshot.sh` +
`tools/report.py::snapshot_diff`。本文件新增的只有「参数怎么挑、怎么下发、怎么还原」。

用法:
    python3 loop_v1/auto/autoloop.py --out loop_v1/runs_sysparam/<标签> \\
        [--generations 2] [--children 3] [--pairs 5] [--no-llm] [--probe-only]
"""
from __future__ import annotations
import argparse
import json
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.path.join(os.path.dirname(HERE), "tools")
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, TOOLS)
sys.path.insert(0, HERE)

from analyze import compare, describe                      # noqa: E402
from report import snapshot_diff                            # noqa: E402
import whitelist as WL                                      # noqa: E402
import verdict as V                                         # noqa: E402
import llm as LLM                                           # noqa: E402

SERIAL = os.environ.get("SERIAL", "91253241019A")


def _find_adb() -> str:
    """adb 的位置因装法而异 (homebrew / Android SDK), 别写死一条路径。"""
    env = os.environ.get("ADB")
    if env:
        return env
    found = shutil.which("adb")
    if found:
        return found
    return "/Users/mac/Library/Android/sdk/platform-tools/adb"


ADB = _find_adb()
DEV_TMP = "/data/local/tmp"
DEV_SCRIPTS = ["device_snapshot.sh", "ftrace_capture.sh"]
AUTO_SCRIPTS = ["probe_sysparam.sh", "knob_sysparam.sh", "sample_env.sh",
                "charge_suspend.sh"]

# 等冷门槛的下界。真正的门槛在基线之后定 (见 main): 目标是「回到基线是在什么
# 热态下量的」, 而不是一个拍脑袋的绝对温度 —— 连跑几十轮游戏之后设备根本降不到
# 40C, 用绝对值当门槛会把每一组候选都卡死在等冷超时上。
COOL_C_FLOOR = 40.0
COOL_TIMEOUT_S = 420
# 温度上限的下界。真正的上限 = max(这个值, 基线实测最高温 + 余量), 在看候选数据前冻结。
TEMP_CAP_FLOOR_C = 45.0
TEMP_CAP_MARGIN_C = 3.0


def log(msg: str) -> None:
    print(f"[autoloop] {msg}", flush=True)


def adb(args: list[str], timeout: int = 120, want_status: bool = False) -> str:
    """want_status=True 时把 stderr 与非零退出码一并带回。

    设备端脚本的失败信息有一部分走 stderr (如 knob_sysparam.sh 的「缺 plan 文件」),
    adb shell v2 不会把 stderr 并进 stdout —— 只读 stdout 会把失败当成功。
    快照类调用仍走默认 (stdout only), 免得偶发的 adb 告警污染逐行比对的快照文本。
    """
    r = subprocess.run([ADB, "-s", SERIAL] + args, capture_output=True, text=True,
                       timeout=timeout)
    if not want_status:
        return r.stdout
    out = r.stdout + (r.stderr or "")
    if r.returncode != 0:
        out += f"\n#adb_exit={r.returncode}\n"
    return out


def su(cmd: str, timeout: int = 180, want_status: bool = False) -> str:
    """root shell。命令整体单引号包住, 与 phonefarm hwcond.rs 的 snapshot_cmd 同形。"""
    return adb(["shell", f"su -c '{cmd}'"], timeout=timeout, want_status=want_status)


# ── 设备准备 ──

def push_tools() -> None:
    for f in DEV_SCRIPTS:
        adb(["push", os.path.join(TOOLS, f), DEV_TMP])
    for f in AUTO_SCRIPTS:
        adb(["push", os.path.join(HERE, f), DEV_TMP])
    names = ",".join(s[:-3] for s in DEV_SCRIPTS + AUTO_SCRIPTS)
    adb(["shell", f"chmod 755 {DEV_TMP}/{{{names}}}.sh"])


def snapshot() -> str:
    return su(f"sh {DEV_TMP}/device_snapshot.sh")


def read_soc_temp() -> tuple[str, float] | None:
    """最热的 SoC 结温 (只看 cpu-/cpullc/gpuss, 与 hwcond.rs::soc_max_c 同口径)。"""
    txt = adb(["shell",
               'for z in /sys/class/thermal/thermal_zone*; do '
               'echo "$(cat $z/type) $(cat $z/temp)"; done 2>/dev/null'])
    best = None
    for line in txt.splitlines():
        parts = line.split()
        if len(parts) < 2:
            continue
        ty = parts[0]
        if not (ty.startswith("cpu-") or ty.startswith("cpullc") or ty.startswith("gpuss")):
            continue
        try:
            t = int(parts[1])
        except ValueError:
            continue
        if not (0 < t < 100000):
            continue
        if best is None or t > best[1]:
            best = (ty, t)
    return (best[0], best[1] / 1000.0) if best else None


def wait_cool(cool_c: float = COOL_C_FLOOR, timeout_s: int = COOL_TIMEOUT_S) -> dict:
    t0 = time.time()
    while True:
        cur = read_soc_temp()
        if cur is None:
            return {"ok": False, "reason": "读不到任何 SoC 热区"}
        if cur[1] < cool_c:
            return {"ok": True, "zone": cur[0], "c": cur[1],
                    "waited_s": round(time.time() - t0, 1)}
        if time.time() - t0 > timeout_s:
            return {"ok": False, "reason": f"等冷超时: {cur[0]}={cur[1]}C 仍 >= {cool_c}C",
                    "zone": cur[0], "c": cur[1]}
        log(f"等冷: {cur[0]}={cur[1]}C >= {cool_c}C, 8s 后重测 "
            f"(已等 {int(time.time() - t0)}s)")
        time.sleep(8)


# 这几项由驱动按当前温度/负载自己改, 本闭环从不写它们。拿它们当「留痕」,
# 会把「跑完比跑前热」误判成没还原干净, 一整组 5 对实验就白跑了。
# 所以分两层报: strict 是原样的逐行 diff (什么都不藏), ours 只看
# 「我们写过的那类项」—— 判定用 ours, 证据里两份都留。
# kgsl.max_gpuclk 是 thermal_pwrlevel 对应的那个频率, 同样由驱动按温度自己改:
# 实测探测前后 6 -> 4 / 646MHz -> 826MHz, 只是设备凉了一点, 不是我们留的痕。
DRIVER_OWNED_PREFIXES = ("kgsl.thermal_pwrlevel", "kgsl.max_gpuclk")


def classify_diff(sd: dict) -> dict:
    """把快照 diff 拆成「我们留的痕」与「驱动自己动的」两堆。"""
    ours, driver = [], []
    for d in sd.get("diffs", []) or []:
        key = (d.get("before") or "").split("=", 1)[0].strip()
        (driver if key.startswith(DRIVER_OWNED_PREFIXES) else ours).append(d)
    return {
        "checked": sd.get("checked"),
        "n_lines": sd.get("n_lines"),
        "strict_identical": bool(sd.get("identical")),
        "strict_n_diff": sd.get("n_diff"),
        "ours_identical": not ours,
        "ours_diffs": ours,
        "driver_owned_diffs": driver,
    }


# ── 停充 (测功耗的前提) ──

def charge_suspend() -> str:
    """测功耗期间停充, 让整机真由电池供电。实现与判据照搬 hwcond.rs, 见脚本注释。

    失败不致命: 拿不到放电态就只是功耗这一项不进判定 (env_stats 会如实报原因),
    帧时与帧率照常测。绝不因为量不了功耗就不跑实验。
    """
    out = su(f"sh {DEV_TMP}/charge_suspend.sh suspend", timeout=90, want_status=True)
    log("停充: " + " | ".join(out.strip().splitlines()[-2:]))
    return out


def charge_restore() -> str:
    out = su(f"sh {DEV_TMP}/charge_suspend.sh restore", timeout=60, want_status=True)
    if "CHARGE_RESTORE_FAIL" in out:
        log(f"警告: 充电未能恢复, 保留 state 文件以便人工回滚:\n{out}")
    return out


# ── 单轮 ──

def run_one(label: str, outdir: str, lead: int = 6, capdur: int = 30) -> dict:
    """跑一轮负载 + ftrace 采集 + 功耗温度采样, 返回合并后的指标。"""
    os.makedirs(outdir, exist_ok=True)
    env = dict(os.environ)
    env.update({"SERIAL": SERIAL, "ROOT": ROOT, "LEAD": str(lead), "CAPDUR": str(capdur)})
    env["PATH"] = env.get("PATH", "") + ":" + os.path.dirname(ADB)

    # 功耗/温度采样与 ftrace 并行: 覆盖整段负载, 间隔 2s
    env_path = os.path.join(outdir, "env.txt")
    rc: int | None = None
    with open(env_path, "w") as fh:
        sampler = subprocess.Popen(
            [ADB, "-s", SERIAL, "shell",
             f"su -c 'sh {DEV_TMP}/sample_env.sh {lead + capdur + 4} 2'"],
            stdout=fh, stderr=subprocess.STDOUT)
        try:
            rc = subprocess.run(["bash", os.path.join(TOOLS, "run_once.sh"), label, outdir],
                                env=env, timeout=300, check=False).returncode
        except subprocess.TimeoutExpired:
            log(f"{label}: run_once.sh 超时 (>300s), 本轮作废")
        finally:
            try:
                sampler.wait(timeout=60)
            except subprocess.TimeoutExpired:
                sampler.kill()
                sampler.wait()

    m: dict = {"label": label, "run_once_rc": rc}
    sp = os.path.join(outdir, "summary.json")
    if os.path.exists(sp):
        try:
            with open(sp) as f:
                m.update(json.load(f))
        except json.JSONDecodeError:
            m["parse_error"] = True
    else:
        m["parse_error"] = True
    # run_once.sh 非零退出 = 负载或采集出过问题, 这一轮的数字不可信。如实标出来,
    # 由上层判 ABORT —— 不能让基础设施故障悄悄变成一个「淘汰」结论。
    if rc != 0:
        m["parse_error"] = True
    with open(env_path) as f:
        envm = V.env_stats(f.read())
    m.update(envm)
    with open(os.path.join(outdir, "metrics.json"), "w") as f:
        json.dump(m, f, ensure_ascii=False, indent=1, sort_keys=True)
    return m


# ── 一组候选参数的完整实验 ──

BAD_APPLY_TOKENS = ("KNOB_FAIL", "KNOB_REFUSE", "KNOB_PLAN_REJECTED",
                    "KNOB_ALREADY_APPLIED", "#adb_exit=")


def run_candidate(cand: dict, wl: dict, outdir: str, pairs: int, temp_cap_c: float,
                  power_available: bool, cool_c: float = COOL_C_FLOOR) -> dict:
    """等冷 → A/B 交替 pairs 轮 → 置换检验 → 判定 → 还原。

    A/B **交替**而不是先跑完一臂再跑另一臂: 温度、光照、内存压力这些外生变量都在
    单向漂移, 交替能让残余漂移对两臂等量影响 (loop_v1 README 里已验证过的做法)。
    而且每对内部的先后顺序逐对翻转 (ABBA): 固定「旋钮先、对照后」的话, 单向漂移
    会整体加到对照臂上, 变成一个偏向旋钮臂的系统误差 —— 那不是抵消, 是作弊。
    """
    os.makedirs(outdir, exist_ok=True)
    params = cand["params"]
    plan = WL.plan_text(params, wl)
    plan_local = os.path.join(outdir, "plan.txt")
    with open(plan_local, "w") as f:
        f.write(plan)
    # push 失败必须当场停: 设备上可能还躺着上一组的 plan, 那会让「旋钮臂」施加的是
    # 上一组参数, 而证据里记的是这一组 —— 账对不上比跑不成更糟。
    push_out = adb(["push", plan_local, f"{DEV_TMP}/loop_v1_sysparam.plan"], want_status=True)
    if "#adb_exit=" in push_out:
        return _abort(outdir, cand, f"plan 下发失败: {push_out.strip()}", temp_cap_c)

    snap_before = snapshot()
    with open(os.path.join(outdir, "snap_before.txt"), "w") as f:
        f.write(snap_before)

    # 等冷超时不作废本组: 组内 ABBA 交替已经让两臂承受同样的残余热漂移, 起跑温度
    # 偏高会同等地影响两臂, 不构成偏向。安全由温度上限单独把关。这里只如实记录,
    # 证据里看得出哪几组是热起跑的。
    cool = wait_cool(cool_c)
    if not cool.get("ok"):
        log(f"等冷未达标 ({cool.get('reason')}), 仍按 ABBA 交替继续, 已记进证据")

    charge_log = charge_suspend()

    knob_runs, ctrl_runs = [], []
    apply_logs: list[str] = []
    apply_ok = True
    aborted = None
    restore_fail = None

    def restore() -> str:
        """还原并**检查还原结果**。还原失败必须立刻中止本组: 旋钮还在生效状态下
        跑出来的「对照臂」根本不是对照, 而且下一次 apply 会因为 STATE 还在而
        变成空操作 (KNOB_ALREADY_APPLIED), 整组数据会悄悄变成两臂同构。"""
        nonlocal restore_fail
        out = su(f"sh {DEV_TMP}/knob_sysparam.sh restore", want_status=True)
        if ("KNOB_RESTORE_FAIL" in out or "#adb_exit=" in out) and restore_fail is None:
            restore_fail = out.strip()
            log(f"旋钮还原失败:\n{out}")
        return out

    def run_arm(arm: str, i: int) -> str | None:
        """跑一臂。返回 None = 正常, 否则返回中止原因。"""
        nonlocal apply_ok
        if arm == "knob":
            out = su(f"sh {DEV_TMP}/knob_sysparam.sh apply {DEV_TMP}/loop_v1_sysparam.plan",
                     want_status=True)
            apply_logs.append(out)
            if any(t in out for t in BAD_APPLY_TOKENS):
                apply_ok = False
                log(f"旋钮未全部生效, 停止本组:\n{out}")
                restore()
                return "旋钮未全部生效"
        try:
            m = run_one(f"sp_{arm}{i}", os.path.join(outdir, f"{arm}{i}"))
        finally:
            # 负载/采集抛异常也要先把旋钮摘掉, 绝不把设备留在施加态
            if arm == "knob":
                restore()
        (knob_runs if arm == "knob" else ctrl_runs).append((f"{arm}{i}", m))
        if restore_fail:
            return "旋钮还原失败, 后续数据不可信"
        if m.get("parse_error"):
            return f"{arm} 臂第 {i} 轮没拿到可用 summary.json (负载或采集失败)"
        t = m.get("soc_temp_max_c")
        if t is not None and t > temp_cap_c:
            return f"{arm} 臂第 {i} 轮 SoC 结温 {t}C 超过上限 {temp_cap_c}C"
        return None

    try:
        for i in range(1, pairs + 1):
            # ABBA: 奇数对旋钮先跑, 偶数对对照先跑, 让残余漂移在两臂之间对消
            for arm in (("knob", "ctrl") if i % 2 else ("ctrl", "knob")):
                aborted = run_arm(arm, i)
                if aborted:
                    log(aborted)
                    break
            if aborted:
                break
    except Exception as e:  # noqa: BLE001 — 任何异常都要落到「还原 + 归档」这条路上
        aborted = f"实验过程异常, 已强制还原: {type(e).__name__}: {e}"
        log(aborted)

    # 无论如何先还原 (旋钮 + 充电), 再核快照
    restore_log = restore()
    charge_restore_log = charge_restore()
    status_log = su(f"sh {DEV_TMP}/knob_sysparam.sh status")
    snap_after = snapshot()
    with open(os.path.join(outdir, "snap_after.txt"), "w") as f:
        f.write(snap_after)
    sd = classify_diff(snapshot_diff(os.path.join(outdir, "snap_before.txt"),
                                     os.path.join(outdir, "snap_after.txt")))

    temps = [m.get("soc_temp_max_c") for _, m in knob_runs + ctrl_runs
             if m.get("soc_temp_max_c") is not None]
    temp_max = max(temps) if temps else None

    metrics = [m for m, _ in V.PRIMARY_METRICS if power_available or m != "power_w_mean"]
    cmps: dict = {}
    if len(knob_runs) >= 2 and len(ctrl_runs) >= 2:
        for met in metrics:
            cmps[met] = compare(ctrl_runs, knob_runs, met)

    if aborted is None and restore_fail:
        aborted = "收尾还原失败, 本组作废"
    if aborted:
        dec = {"verdict": "ABORT", "reason": aborted, "temp_max_c": temp_max,
               "temp_cap_c": temp_cap_c}
    elif len(knob_runs) < pairs or len(ctrl_runs) < pairs:
        dec = {"verdict": "ABORT",
               "reason": f"轮数不足 (旋钮 {len(knob_runs)}/{pairs}, 对照 {len(ctrl_runs)}/{pairs})",
               "temp_max_c": temp_max, "temp_cap_c": temp_cap_c}
    else:
        dec = V.decide(cmps, temp_max_c=temp_max, temp_cap_c=temp_cap_c,
                       apply_ok=apply_ok, snapshot_identical=bool(sd.get("ours_identical")),
                       power_available=power_available)

    result = {
        "params": params,
        "why": cand.get("why", ""),
        "plan": plan,
        "pairs_completed": min(len(knob_runs), len(ctrl_runs)),
        "apply_ok": apply_ok,
        "apply_log": apply_logs[:2],
        "restore_ok": restore_fail is None,
        "restore_fail": restore_fail,
        "restore_log": restore_log.strip().splitlines(),
        "knob_state_after": status_log.strip().splitlines(),
        "cooldown": cool,
        "charge_suspend_log": charge_log.strip().splitlines(),
        "charge_restore_log": charge_restore_log.strip().splitlines(),
        "on_battery_uniform": len({m.get("on_battery") for _, m in knob_runs + ctrl_runs}) <= 1,
        "snapshot_check": sd,
        "temp_max_c": temp_max,
        "temp_cap_c": temp_cap_c,
        "arms": {
            "ctrl": describe(ctrl_runs, "ctrl") if ctrl_runs else None,
            "knob": describe(knob_runs, "knob") if knob_runs else None,
        },
        "env_per_run": {name: {k: m.get(k) for k in
                               ("power_w_mean", "power_w_median", "power_now_w_mean",
                                "power_now_plausible", "power_vi_w_mean",
                                "usb_input_w_mean", "current_now_ua_mean",
                                "voltage_now_uv_mean", "power_rail", "battery_status",
                                "battery_charging", "power_usable_reason",
                                "soc_temp_max_c", "soc_temp_mean_c", "n_samples",
                                "fan_state")}
                        for name, m in knob_runs + ctrl_runs},
        # 两臂的风扇状态必须一致, 否则功耗那一项比的是风扇不是参数
        "fan_state_uniform": len({m.get("fan_state") for _, m in knob_runs + ctrl_runs}) <= 1,
        "comparisons": cmps,
        **dec,
    }
    with open(os.path.join(outdir, "result.json"), "w") as f:
        json.dump(result, f, ensure_ascii=False, indent=1)
    return result


def _abort(outdir: str, cand: dict, reason: str, cap: float) -> dict:
    r = {"params": cand["params"], "why": cand.get("why", ""),
         "verdict": "ABORT", "reason": reason, "temp_cap_c": cap}
    with open(os.path.join(outdir, "result.json"), "w") as f:
        json.dump(r, f, ensure_ascii=False, indent=1)
    return r


# ── 主流程 ──

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, help="证据目录")
    ap.add_argument("--generations", type=int, default=2)
    ap.add_argument("--children", type=int, default=3)
    ap.add_argument("--pairs", type=int, default=5, help="每组候选的 A/B 对数")
    ap.add_argument("--no-llm", action="store_true", help="跳过大模型, 只用本地变异器")
    ap.add_argument("--probe-only", action="store_true", help="只探白名单, 不跑实验")
    ap.add_argument("--baseline-runs", type=int, default=2,
                    help="冻结温度上限用的基线轮数")
    ap.add_argument("--power-in-verdict", choices=("auto", "on", "off"), default="auto",
                    help="把整机功耗计入判定。**缺省关闭**: 本机插着 USB 时 USB 输入"
                         "help 见 README。auto (缺省): 基线轮真的拿到放电态 "
                         "(停充成功) 才计入; on: 强制计入; off: 强制不计入。"
                         "插着 USB 充电时测到的不是整机功耗 —— USB 输入里约 46%% 是在给"
                         "电池充电, 且充电电流随电量单调衰减。功耗数据无论如何照常逐轮记录。")
    a = ap.parse_args()

    out = os.path.abspath(a.out)
    os.makedirs(out, exist_ok=True)
    log(f"证据目录 {out}")

    if not os.path.exists(ADB):
        log(f"找不到 adb: {ADB}")
        return 2
    if "device" not in adb(["get-state"]):
        log("设备不在线")
        return 2

    push_tools()

    # 1) 进入前快照 —— 判据 4 的基准
    snap_before = snapshot()
    with open(os.path.join(out, "snap_before.txt"), "w") as f:
        f.write(snap_before)

    # 2) 真机探白名单
    log("探测可写系统参数 (含生效测试, 每项立刻还原) ...")
    probe_txt = su(f"sh {DEV_TMP}/probe_sysparam.sh", timeout=600)
    with open(os.path.join(out, "probe.txt"), "w") as f:
        f.write(probe_txt)
    probe = WL.parse_probe(probe_txt)
    wl = WL.build_whitelist(probe)
    with open(os.path.join(out, "whitelist.json"), "w") as f:
        json.dump(wl, f, ensure_ascii=False, indent=1, sort_keys=True)
    log(f"白名单 {len(wl)} 项: {', '.join(sorted(wl))}")
    if not wl:
        log("白名单为空, 没有任何参数通过「存在+可写+真生效」三关")
        return 1

    # 探测本身也要不留痕
    snap_after_probe = snapshot()
    with open(os.path.join(out, "snap_after_probe.txt"), "w") as f:
        f.write(snap_after_probe)
    probe_sd = classify_diff(snapshot_diff(os.path.join(out, "snap_before.txt"),
                                           os.path.join(out, "snap_after_probe.txt")))
    log(f"探测后快照一致 (我们写过的项): {probe_sd.get('ours_identical')}; "
        f"严格逐行一致: {probe_sd.get('strict_identical')} "
        f"(驱动自己动的 {len(probe_sd.get('driver_owned_diffs') or [])} 行)")

    if a.probe_only:
        with open(os.path.join(out, "report.json"), "w") as f:
            json.dump({"whitelist": wl, "probe_residue": probe_sd}, f,
                      ensure_ascii=False, indent=1)
        return 0

    # 3) 基线: 量基准温度与功耗可用性, 用来冻结温度上限
    log(f"跑 {a.baseline_runs} 轮基线 (不加任何参数), 用于冻结温度上限与确认功耗可测 ...")
    cool = wait_cool()
    log(f"基线前等冷: {cool}")
    base_start_c = cool.get("c")
    base_charge_log = charge_suspend()
    base_runs = []
    for i in range(1, a.baseline_runs + 1):
        m = run_one(f"sp_base{i}", os.path.join(out, "baseline", f"base{i}"))
        base_runs.append((f"base{i}", m))
        log(f"  base{i}: p95={m.get('frame_p95')}ms fps={m.get('fps_mean')} "
            f"power={m.get('power_w_mean')}W ({m.get('power_source')}) "
            f"temp={m.get('soc_temp_max_c')}C")
    base_charge_restore_log = charge_restore()
    base_temps = [m.get("soc_temp_max_c") for _, m in base_runs
                  if m.get("soc_temp_max_c") is not None]
    temp_cap = max(TEMP_CAP_FLOOR_C,
                   round(max(base_temps) + TEMP_CAP_MARGIN_C, 1)) if base_temps \
        else TEMP_CAP_FLOOR_C
    power_measurable = any(m.get("power_w_mean") is not None for _, m in base_runs)
    power_reasons = sorted({m.get("power_usable_reason") or "" for _, m in base_runs})
    # 功耗进不进判定是个**显式开关**, 不是"能测到就用"。充电态下测到的数看起来
    # 很正常, 但它不是整机功耗 —— 悄悄拿它判保留/淘汰, 比不测还糟。
    on_battery_all = all(m.get("on_battery") for _, m in base_runs) if base_runs else False
    if a.power_in_verdict == "on":
        power_available = True
    elif a.power_in_verdict == "off":
        power_available = False
    else:   # auto: 只有基线轮**每一轮**都真的在放电态才算数
        power_available = bool(power_measurable and on_battery_all)
    power_note = (
        f"功耗计入判定 (基线轮全程放电态; 停充: {base_charge_log.strip().splitlines()[0] if base_charge_log.strip() else '?'})"
        if power_available else
        "功耗**不计入判定**, 仅记录供参考。" + " / ".join(r for r in power_reasons if r))
    # 每组候选的等冷目标 = 「回到基线是在什么热态下量的」, 而不是一个绝对温度。
    # 取基线起跑温度 + 1C, 下界 40C, 上界比温度上限低 2C —— 也在看候选数据前冻结。
    cool_target = min(max(COOL_C_FLOOR, round((base_start_c or COOL_C_FLOOR) + 1.0, 1)),
                      temp_cap - 2.0)

    # 4) **在看到任何候选数据之前**冻结判定规则
    rule = V.rule_doc(temp_cap, a.pairs, power_available, power_note)
    rule["power_measurable"] = power_measurable
    rule["power_in_verdict_mode"] = a.power_in_verdict
    rule["baseline_on_battery"] = on_battery_all
    rule["charge_suspend_log"] = base_charge_log.strip().splitlines()
    rule["charge_restore_log"] = base_charge_restore_log.strip().splitlines()
    rule["power_reasons"] = [r for r in power_reasons if r]
    rule["temp_cap_derivation"] = {
        "floor_c": TEMP_CAP_FLOOR_C, "margin_c": TEMP_CAP_MARGIN_C,
        "baseline_max_c": max(base_temps) if base_temps else None,
        "formula": "max(floor, baseline_max + margin)",
    }
    rule["cooldown_target_c"] = cool_target
    rule["cooldown_derivation"] = {
        "floor_c": COOL_C_FLOOR, "baseline_start_c": base_start_c,
        "formula": "min(max(floor, baseline_start + 1), temp_cap - 2)",
        "on_timeout": "如实记录后继续 (组内 ABBA 交替已让两臂承受同样的残余热漂移), 不作废本组",
    }
    rule["baseline"] = describe(base_runs, "baseline")
    with open(os.path.join(out, "rule.json"), "w") as f:
        json.dump(rule, f, ensure_ascii=False, indent=1)
    log(f"判定规则已冻结: 温度上限 {temp_cap}C, 等冷目标 {cool_target}C, 指标 {rule['n_metrics']} 个, "
        f"alpha_win={rule['alpha_win_bonferroni']}, "
        f"{a.pairs}v{a.pairs} 最小可达 p={rule['min_reachable_p']}, "
        f"可达={rule['reachable']}; {power_note}")
    if not rule["reachable"]:
        log("警告: 轮数不足以达到 Bonferroni 收紧后的显著性门槛, 本次任何候选都不可能判保留")

    keys = {} if a.no_llm else LLM.load_keys(
        [os.path.join(ROOT, "secrets.env"), "/Users/mac/projects/phonefarm/secrets.env"])
    wl_desc = WL.describe_whitelist(wl)
    history: list[dict] = []
    all_results: list[dict] = []

    for gen in range(1, a.generations + 1):
        log(f"=== 第 {gen} 代 ===")
        gdir = os.path.join(out, f"gen{gen}")
        os.makedirs(gdir, exist_ok=True)

        # 5) 大模型挑参数 (失败降级到本地变异器)
        cands: list[dict] = []
        source = "local_mutate"
        if keys:
            prompt = LLM.build_prompt(wl_desc, history, a.children)
            with open(os.path.join(gdir, "prompt.txt"), "w") as f:
                f.write(prompt)
            got = LLM.chat(prompt, keys, log=log)
            if got:
                source, raw = got
                with open(os.path.join(gdir, "llm_raw.txt"), "w") as f:
                    f.write(raw)
                cands = LLM.parse_candidates(raw, a.children)
                log(f"{source} 给出 {len(cands)} 组")
        valid: list[dict] = []
        for c in cands:
            ok, why = WL.validate_candidate(c["params"], wl)
            if ok:
                valid.append(c)
            else:
                log(f"作废一组 (模型给的参数不合法): {why} — {c['params']}")
        if len(valid) < a.children:
            need = a.children - len(valid)
            log(f"用本地变异器补 {need} 组")
            for c in LLM.local_mutate(wl, history + [{"params": v["params"]} for v in valid],
                                      need, seed=gen * 1000 + len(valid)):
                ok, why = WL.validate_candidate(c["params"], wl)
                if ok:
                    valid.append(c)
        with open(os.path.join(gdir, "candidates.json"), "w") as f:
            json.dump({"source": source, "candidates": valid}, f, ensure_ascii=False, indent=1)

        # 6) 逐组上真机
        for ci, cand in enumerate(valid, 1):
            cdir = os.path.join(gdir, f"cand{ci}")
            log(f"[gen{gen}/cand{ci}] {json.dumps(cand['params'], ensure_ascii=False)}")
            res = run_candidate(cand, wl, cdir, a.pairs, temp_cap, power_available,
                                cool_c=cool_target)
            res["gen"], res["cand"] = gen, ci
            all_results.append(res)
            history.append({"params": cand["params"], "verdict": res["verdict"],
                            "reason": res.get("reason", ""),
                            "per_metric": res.get("per_metric", {})})
            log(f"[gen{gen}/cand{ci}] 判定 {res['verdict']}: {res.get('reason')}")

    # 7) 收尾: 强制还原 (旋钮 + 充电) + 全局快照比对
    charge_restore()
    fin_restore = su(f"sh {DEV_TMP}/knob_sysparam.sh restore", want_status=True)
    if "KNOB_RESTORE_FAIL" in fin_restore or "#adb_exit=" in fin_restore:
        # state 文件是「还原不成功时唯一的回滚依据」, 这时候删它等于把现场毁了
        log(f"警告: 收尾还原失败, 保留 state 文件以便人工回滚:\n{fin_restore}")
    else:
        su(f"rm -f {DEV_TMP}/loop_v1_sysparam.state {DEV_TMP}/loop_v1_knob_ddr.state")
    # 收尾快照前先等冷: 进入前那份快照是在冷机状态下采的, 刚跑完游戏就采会让
    # 驱动自己按温度改的那几项对不上 —— 那是热态差异, 不是留痕。等不下来也继续,
    # classify_diff 会把这类项单独归到 driver_owned 里如实报出来。
    fin_cool = wait_cool(cool_target)
    log(f"收尾等冷: {fin_cool}")
    snap_final = snapshot()
    with open(os.path.join(out, "snap_final.txt"), "w") as f:
        f.write(snap_final)
    final_sd = classify_diff(snapshot_diff(os.path.join(out, "snap_before.txt"),
                                           os.path.join(out, "snap_final.txt")))

    kept = [r for r in all_results if r["verdict"] == "KEEP"]
    fan_states = sorted({m.get("fan_state") for _, m in base_runs if m.get("fan_state")})
    report = {
        "goal": "系统参数全自动闭环 (原神实测)",
        # 测试条件: 主动散热风扇自己耗电, 会进功耗读数。风扇开关前后的数据不能混着比,
        # 所以把当次的风扇状态原样记进报告 —— 它不是被调的参数, 是本次实验的前提条件。
        "test_conditions": {
            "device": SERIAL,
            "fan_state_at_baseline": fan_states,
            "battery_status_at_baseline": sorted({m.get("battery_status")
                                                  for _, m in base_runs if m.get("battery_status")}),
            "power_in_verdict": power_available,
            "power_caveat": power_note,
            "power_source_at_baseline": sorted({m.get("power_source")
                                                for _, m in base_runs if m.get("power_source")}),
            "charge_suspended_during_measurement": "CHARGE_SUSPENDED" in base_charge_log,
            "fan_is_a_knob": False,
            "fan_note": "风扇转速是系统参数的一种, 但不进自动调参白名单 (DENY_KEYWORDS 含 fan)",
            "power_rail": "battery",
        },
        "rule": rule,
        "whitelist": sorted(wl),
        "whitelist_detail": wl,
        "probe_residue": probe_sd,
        "generations": a.generations,
        "children_per_generation": a.children,
        "pairs_per_candidate": a.pairs,
        "results": all_results,
        "kept": [{"params": r["params"], "reason": r.get("reason"),
                  "per_metric": r.get("per_metric")} for r in kept],
        "rejected": [{"params": r["params"], "verdict": r["verdict"],
                      "reason": r.get("reason")} for r in all_results
                     if r["verdict"] != "KEEP"],
        "final_restore_log": fin_restore.strip().splitlines(),
        "final_cooldown": fin_cool,
        "final_snapshot_identical": bool(final_sd.get("ours_identical")),
        "final_snapshot_strict_identical": bool(final_sd.get("strict_identical")),
        "final_snapshot_diff": final_sd,
    }
    with open(os.path.join(out, "report.json"), "w") as f:
        json.dump(report, f, ensure_ascii=False, indent=1)
    log(f"完成。保留 {len(kept)} 组 / 共 {len(all_results)} 组; "
        f"收尾快照一致 (我们写过的项): {final_sd.get('ours_identical')}, "
        f"严格逐行一致: {final_sd.get('strict_identical')}")
    return 0 if final_sd.get("ours_identical") else 1


if __name__ == "__main__":
    raise SystemExit(main())
