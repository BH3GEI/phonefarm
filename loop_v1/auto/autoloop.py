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
ADB = os.environ.get("ADB", "/Users/mac/Library/Android/sdk/platform-tools/adb")
DEV_TMP = "/data/local/tmp"
DEV_SCRIPTS = ["device_snapshot.sh", "ftrace_capture.sh"]
AUTO_SCRIPTS = ["probe_sysparam.sh", "knob_sysparam.sh", "sample_env.sh"]

# 等冷门槛: 每组候选开跑前必须降到这个温度以下, 否则前一组的余热会污染这一组。
COOL_C = 40.0
COOL_TIMEOUT_S = 420
# 温度上限的下界。真正的上限 = max(这个值, 基线实测最高温 + 余量), 在看候选数据前冻结。
TEMP_CAP_FLOOR_C = 45.0
TEMP_CAP_MARGIN_C = 3.0


def log(msg: str) -> None:
    print(f"[autoloop] {msg}", flush=True)


def adb(args: list[str], timeout: int = 120) -> str:
    r = subprocess.run([ADB, "-s", SERIAL] + args, capture_output=True, text=True,
                       timeout=timeout)
    return r.stdout


def su(cmd: str, timeout: int = 180) -> str:
    """root shell。命令整体单引号包住, 与 phonefarm hwcond.rs 的 snapshot_cmd 同形。"""
    return adb(["shell", f"su -c '{cmd}'"], timeout=timeout)


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


def wait_cool(cool_c: float = COOL_C, timeout_s: int = COOL_TIMEOUT_S) -> dict:
    t0 = time.time()
    last = None
    while True:
        cur = read_soc_temp()
        if cur is None:
            return {"ok": False, "reason": "读不到任何 SoC 热区"}
        last = cur
        if cur[1] < cool_c:
            return {"ok": True, "zone": cur[0], "c": cur[1],
                    "waited_s": round(time.time() - t0, 1)}
        if time.time() - t0 > timeout_s:
            return {"ok": False, "reason": f"等冷超时: {cur[0]}={cur[1]}C 仍 >= {cool_c}C",
                    "zone": cur[0], "c": cur[1]}
        log(f"等冷: {cur[0]}={cur[1]}C >= {cool_c}C, 8s 后重测 "
            f"(已等 {int(time.time() - t0)}s)")
        time.sleep(8)


# ── 单轮 ──

def run_one(label: str, outdir: str, lead: int = 6, capdur: int = 30) -> dict:
    """跑一轮负载 + ftrace 采集 + 功耗温度采样, 返回合并后的指标。"""
    os.makedirs(outdir, exist_ok=True)
    env = dict(os.environ)
    env.update({"SERIAL": SERIAL, "ROOT": ROOT, "LEAD": str(lead), "CAPDUR": str(capdur)})
    env["PATH"] = env.get("PATH", "") + ":" + os.path.dirname(ADB)

    # 功耗/温度采样与 ftrace 并行: 覆盖整段负载, 间隔 2s
    env_path = os.path.join(outdir, "env.txt")
    with open(env_path, "w") as fh:
        sampler = subprocess.Popen(
            [ADB, "-s", SERIAL, "shell",
             f"su -c 'sh {DEV_TMP}/sample_env.sh {lead + capdur + 4} 2'"],
            stdout=fh, stderr=subprocess.STDOUT)
        try:
            subprocess.run(["bash", os.path.join(TOOLS, "run_once.sh"), label, outdir],
                           env=env, timeout=300, check=False)
        finally:
            try:
                sampler.wait(timeout=60)
            except subprocess.TimeoutExpired:
                sampler.kill()

    m: dict = {"label": label}
    sp = os.path.join(outdir, "summary.json")
    if os.path.exists(sp):
        try:
            with open(sp) as f:
                m.update(json.load(f))
        except json.JSONDecodeError:
            m["parse_error"] = True
    else:
        m["parse_error"] = True
    with open(env_path) as f:
        envm = V.env_stats(f.read())
    m.update(envm)
    with open(os.path.join(outdir, "metrics.json"), "w") as f:
        json.dump(m, f, ensure_ascii=False, indent=1, sort_keys=True)
    return m


# ── 一组候选参数的完整实验 ──

def run_candidate(cand: dict, wl: dict, outdir: str, pairs: int, temp_cap_c: float,
                  power_available: bool) -> dict:
    """等冷 → A/B 交替 pairs 轮 → 置换检验 → 判定 → 还原。

    A/B **交替**而不是先跑完一臂再跑另一臂: 温度、光照、内存压力这些外生变量都在
    单向漂移, 交替能让残余漂移对两臂等量影响 (loop_v1 README 里已验证过的做法)。
    """
    os.makedirs(outdir, exist_ok=True)
    params = cand["params"]
    plan = WL.plan_text(params, wl)
    plan_local = os.path.join(outdir, "plan.txt")
    with open(plan_local, "w") as f:
        f.write(plan)
    adb(["push", plan_local, f"{DEV_TMP}/loop_v1_sysparam.plan"])

    snap_before = snapshot()
    with open(os.path.join(outdir, "snap_before.txt"), "w") as f:
        f.write(snap_before)

    cool = wait_cool()
    if not cool.get("ok"):
        return _abort(outdir, cand, f"等冷失败: {cool.get('reason')}", temp_cap_c)

    knob_runs, ctrl_runs = [], []
    apply_logs = []
    apply_ok = True
    aborted = None

    for i in range(1, pairs + 1):
        # ── 旋钮臂 ──
        out = su(f"sh {DEV_TMP}/knob_sysparam.sh apply {DEV_TMP}/loop_v1_sysparam.plan")
        apply_logs.append(out)
        if "KNOB_FAIL" in out or "KNOB_REFUSE" in out or "KNOB_PLAN_REJECTED" in out:
            apply_ok = False
            log(f"旋钮未全部生效, 停止本组:\n{out}")
            su(f"sh {DEV_TMP}/knob_sysparam.sh restore")
            break
        m = run_one(f"sp_knob{i}", os.path.join(outdir, f"knob{i}"))
        knob_runs.append((f"knob{i}", m))
        su(f"sh {DEV_TMP}/knob_sysparam.sh restore")

        if m.get("soc_temp_max_c") is not None and m["soc_temp_max_c"] > temp_cap_c:
            aborted = f"旋钮臂第 {i} 轮 SoC 结温 {m['soc_temp_max_c']}C 超过上限 {temp_cap_c}C"
            log(aborted)
            break

        # ── 对照臂 (旋钮已还原) ──
        m = run_one(f"sp_ctrl{i}", os.path.join(outdir, f"ctrl{i}"))
        ctrl_runs.append((f"ctrl{i}", m))
        if m.get("soc_temp_max_c") is not None and m["soc_temp_max_c"] > temp_cap_c:
            aborted = f"对照臂第 {i} 轮 SoC 结温 {m['soc_temp_max_c']}C 超过上限 {temp_cap_c}C"
            log(aborted)
            break

    # 无论如何先还原, 再核快照
    restore_log = su(f"sh {DEV_TMP}/knob_sysparam.sh restore")
    status_log = su(f"sh {DEV_TMP}/knob_sysparam.sh status")
    snap_after = snapshot()
    with open(os.path.join(outdir, "snap_after.txt"), "w") as f:
        f.write(snap_after)
    sd = snapshot_diff(os.path.join(outdir, "snap_before.txt"),
                       os.path.join(outdir, "snap_after.txt"))

    temps = [m.get("soc_temp_max_c") for _, m in knob_runs + ctrl_runs
             if m.get("soc_temp_max_c") is not None]
    temp_max = max(temps) if temps else None

    metrics = [m for m, _ in V.PRIMARY_METRICS if power_available or m != "power_w_mean"]
    cmps: dict = {}
    if len(knob_runs) >= 2 and len(ctrl_runs) >= 2:
        for met in metrics:
            cmps[met] = compare(ctrl_runs, knob_runs, met)

    if aborted:
        dec = {"verdict": "ABORT", "reason": aborted, "temp_max_c": temp_max,
               "temp_cap_c": temp_cap_c}
    elif len(knob_runs) < pairs or len(ctrl_runs) < pairs:
        dec = {"verdict": "ABORT",
               "reason": f"轮数不足 (旋钮 {len(knob_runs)}/{pairs}, 对照 {len(ctrl_runs)}/{pairs})",
               "temp_max_c": temp_max, "temp_cap_c": temp_cap_c}
    else:
        dec = V.decide(cmps, temp_max_c=temp_max, temp_cap_c=temp_cap_c,
                       apply_ok=apply_ok, snapshot_identical=bool(sd.get("identical")),
                       power_available=power_available)

    result = {
        "params": params,
        "why": cand.get("why", ""),
        "plan": plan,
        "pairs_completed": min(len(knob_runs), len(ctrl_runs)),
        "apply_ok": apply_ok,
        "apply_log": apply_logs[:2],
        "restore_log": restore_log.strip().splitlines(),
        "knob_state_after": status_log.strip().splitlines(),
        "cooldown": cool,
        "snapshot_check": {"identical": bool(sd.get("identical")),
                           "n_diff": sd.get("n_diff"), "diffs": sd.get("diffs")},
        "temp_max_c": temp_max,
        "temp_cap_c": temp_cap_c,
        "arms": {
            "ctrl": describe(ctrl_runs, "ctrl") if ctrl_runs else None,
            "knob": describe(knob_runs, "knob") if knob_runs else None,
        },
        "env_per_run": {name: {k: m.get(k) for k in
                               ("power_w_mean", "power_w_median", "soc_temp_max_c",
                                "soc_temp_mean_c", "n_samples", "battery_charging")}
                        for name, m in knob_runs + ctrl_runs},
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
    probe_sd = snapshot_diff(os.path.join(out, "snap_before.txt"),
                             os.path.join(out, "snap_after_probe.txt"))
    log(f"探测后快照一致: {probe_sd.get('identical')} (差异 {probe_sd.get('n_diff')} 行)")

    if a.probe_only:
        with open(os.path.join(out, "report.json"), "w") as f:
            json.dump({"whitelist": wl, "probe_residue": probe_sd}, f,
                      ensure_ascii=False, indent=1)
        return 0

    # 3) 基线: 量基准温度与功耗可用性, 用来冻结温度上限
    log(f"跑 {a.baseline_runs} 轮基线 (不加任何参数), 用于冻结温度上限与确认功耗可测 ...")
    cool = wait_cool()
    log(f"等冷: {cool}")
    base_runs = []
    for i in range(1, a.baseline_runs + 1):
        m = run_one(f"sp_base{i}", os.path.join(out, "baseline", f"base{i}"))
        base_runs.append((f"base{i}", m))
        log(f"  base{i}: p95={m.get('frame_p95')}ms fps={m.get('fps_mean')} "
            f"power={m.get('power_w_mean')}W temp={m.get('soc_temp_max_c')}C")
    base_temps = [m.get("soc_temp_max_c") for _, m in base_runs
                  if m.get("soc_temp_max_c") is not None]
    temp_cap = max(TEMP_CAP_FLOOR_C,
                   round(max(base_temps) + TEMP_CAP_MARGIN_C, 1)) if base_temps \
        else TEMP_CAP_FLOOR_C
    power_available = any(m.get("power_w_mean") is not None for _, m in base_runs)

    # 4) **在看到任何候选数据之前**冻结判定规则
    rule = V.rule_doc(temp_cap, a.pairs, power_available)
    rule["temp_cap_derivation"] = {
        "floor_c": TEMP_CAP_FLOOR_C, "margin_c": TEMP_CAP_MARGIN_C,
        "baseline_max_c": max(base_temps) if base_temps else None,
        "formula": "max(floor, baseline_max + margin)",
    }
    rule["baseline"] = describe(base_runs, "baseline")
    with open(os.path.join(out, "rule.json"), "w") as f:
        json.dump(rule, f, ensure_ascii=False, indent=1)
    log(f"判定规则已冻结: 温度上限 {temp_cap}C, 指标 {rule['n_metrics']} 个, "
        f"alpha_win={rule['alpha_win_bonferroni']}, "
        f"{a.pairs}v{a.pairs} 最小可达 p={rule['min_reachable_p']}, "
        f"可达={rule['reachable']}, 功耗可测={power_available}")
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
            res = run_candidate(cand, wl, cdir, a.pairs, temp_cap, power_available)
            res["gen"], res["cand"] = gen, ci
            all_results.append(res)
            history.append({"params": cand["params"], "verdict": res["verdict"],
                            "reason": res.get("reason", ""),
                            "per_metric": res.get("per_metric", {})})
            log(f"[gen{gen}/cand{ci}] 判定 {res['verdict']}: {res.get('reason')}")

    # 7) 收尾: 强制还原 + 全局快照比对
    su(f"sh {DEV_TMP}/knob_sysparam.sh restore")
    su(f"rm -f {DEV_TMP}/loop_v1_sysparam.state {DEV_TMP}/loop_v1_knob_ddr.state")
    snap_final = snapshot()
    with open(os.path.join(out, "snap_final.txt"), "w") as f:
        f.write(snap_final)
    final_sd = snapshot_diff(os.path.join(out, "snap_before.txt"),
                             os.path.join(out, "snap_final.txt"))

    kept = [r for r in all_results if r["verdict"] == "KEEP"]
    report = {
        "goal": "系统参数全自动闭环 (原神实测)",
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
        "final_snapshot_identical": bool(final_sd.get("identical")),
        "final_snapshot_diff": final_sd,
    }
    with open(os.path.join(out, "report.json"), "w") as f:
        json.dump(report, f, ensure_ascii=False, indent=1)
    log(f"完成。保留 {len(kept)} 组 / 共 {len(all_results)} 组; "
        f"收尾快照一致: {final_sd.get('identical')}")
    return 0 if final_sd.get("identical") else 1


if __name__ == "__main__":
    raise SystemExit(main())
