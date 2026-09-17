#!/usr/bin/env python3
"""report.py — 把五条判据的证据凑成一份可复现的结论

用法:
  report.py --baseline 'runs/night*' --knob 'runs/knob*' \\
            --snap-before runs/snap_before.txt --snap-after runs/snap_final.txt \\
            [--primary frame_p95]

**刻意不带时间戳、不带绝对路径、不用随机数**, 所以同样的输入每次产出逐字节相同的
report.json —— 判据 5 的回放自检直接 cmp 这份文件。
"""
from __future__ import annotations
import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from analyze import load_runs, describe, compare, drift          # noqa: E402
from attribute import attribute                                   # noqa: E402

DISPERSION_GATE_PCT = 5.0      # 判据 1
MONOTONIC_GATE = 0.90          # 单调比超过这个值就认定为系统性漂移而非抖动
ALPHA = 0.05                   # 判据 3


def snapshot_diff(before: str, after: str) -> dict:
    """判据 4: 逐行比对进入前 / 退出后的设备快照。"""
    if not (os.path.exists(before) and os.path.exists(after)):
        return {"checked": False, "reason": "快照文件缺失"}
    with open(before) as f:
        a = f.read().splitlines()
    with open(after) as f:
        b = f.read().splitlines()
    diffs = []
    for i in range(max(len(a), len(b))):
        la = a[i] if i < len(a) else "<缺行>"
        lb = b[i] if i < len(b) else "<缺行>"
        if la != lb:
            diffs.append({"line": i + 1, "before": la, "after": lb})
    return {
        "checked": True,
        "n_lines": len(a),
        "n_diff": len(diffs),
        "identical": len(diffs) == 0,
        "diffs": diffs[:20],
    }


def criterion_1(base: dict, primary: str) -> dict:
    m = base["metrics"].get(primary)
    if not m:
        return {"pass": False, "reason": f"基线里没有指标 {primary}"}
    disp = m["dispersion_pct"]
    d = m["drift"]
    # 漂移单独判: 离散度合格但单调比贴近 1, 说明只是恰好没散开, 负载并不稳
    drift_flag = (d["monotonic_frac"] is not None and d["monotonic_frac"] >= MONOTONIC_GATE)
    return {
        "pass": bool(disp is not None and disp < DISPERSION_GATE_PCT and not drift_flag),
        "metric": primary,
        "dispersion_pct": disp,
        "gate_pct": DISPERSION_GATE_PCT,
        "values": m["values"],
        "drift": d,
        "systematic_drift_flagged": drift_flag,
        "note": ("离散度合格且无单调漂移" if disp is not None and disp < DISPERSION_GATE_PCT and not drift_flag
                 else ("存在逐轮单调漂移, 即使离散度达标也不算可重复" if drift_flag
                       else f"离散度 {disp}% 超过 {DISPERSION_GATE_PCT}% 门槛")),
    }


def criterion_2(base_runs: list) -> dict:
    """对基线每一轮独立归因, 要求结论一致 —— 一轮一个说法就不叫归因。"""
    attrs = [attribute(r[1]) for r in base_runs]
    verdicts = [a["verdict"] for a in attrs]
    consistent = len(set(verdicts)) == 1
    return {
        "pass": bool(consistent and verdicts and verdicts[0] != "无单一主因"),
        "verdicts_per_run": verdicts,
        "consistent": consistent,
        "verdict": verdicts[0] if consistent and verdicts else None,
        "evidence": attrs[0]["evidence"] if attrs else None,
        "actionable": attrs[0]["actionable"] if attrs else None,
        "measures_run1": attrs[0]["measures"] if attrs else None,
        "bandwidth_run1": attrs[0]["bandwidth"] if attrs else None,
    }


def criterion_3(cmp_primary: dict) -> dict:
    if "error" in cmp_primary:
        return {"pass": False, "reason": cmp_primary["error"]}
    improved = cmp_primary["diff_mean"] < 0          # 帧时间越小越好
    sig = cmp_primary["perm_p_two_sided"] < ALPHA
    ci = cmp_primary["ci95"]
    ci_excludes_zero = bool(ci and (ci[1] < 0 or ci[0] > 0))
    return {
        "pass": bool(improved and sig and ci_excludes_zero),
        "metric": cmp_primary["metric"],
        "baseline_mean": cmp_primary["a_mean"],
        "knob_mean": cmp_primary["b_mean"],
        "diff_mean": cmp_primary["diff_mean"],
        "diff_pct": cmp_primary["diff_pct"],
        "effect_size_cohens_d": cmp_primary["cohens_d"],
        "hodges_lehmann": cmp_primary["hodges_lehmann"],
        "ci95": ci,
        "ci_excludes_zero": ci_excludes_zero,
        "perm_p_two_sided": cmp_primary["perm_p_two_sided"],
        "perm_enumerated": cmp_primary["perm_enumerated"],
        "alpha": ALPHA,
        "direction": "改善" if improved else "变差或无变化",
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--baseline", required=True)
    ap.add_argument("--knob")
    ap.add_argument("--snap-before")
    ap.add_argument("--snap-after")
    ap.add_argument("--replay-result", help="replay_test.sh 的退出码文件, 0=通过")
    ap.add_argument("--primary", default="frame_p95")
    a = ap.parse_args()

    base_runs = load_runs(a.baseline)
    if not base_runs:
        print(f"基线里一轮都没有: {a.baseline}", file=sys.stderr)
        return 1
    base = describe(base_runs, a.baseline)

    out: dict = {
        "goal": "GOAL v1 — 最小可信闭环",
        "primary_metric": a.primary,
        "baseline": base,
        "criterion_1_workload_repeatable": criterion_1(base, a.primary),
        "criterion_2_attribution": criterion_2(base_runs),
    }

    if a.knob:
        knob_runs = load_runs(a.knob)
        if knob_runs:
            out["knob_arm"] = describe(knob_runs, a.knob)
            cmps = {m: compare(base_runs, knob_runs, m)
                    for m in ["frame_p95", "frame_p50", "frame_mean", "gpu_active_mean", "bw_median"]}
            out["comparison"] = cmps
            out["criterion_3_statistically_significant"] = criterion_3(cmps[a.primary])

    if a.snap_before and a.snap_after:
        sd = snapshot_diff(a.snap_before, a.snap_after)
        out["criterion_4_no_residue"] = {"pass": bool(sd.get("identical")), **sd}

    if a.replay_result and os.path.exists(a.replay_result):
        with open(a.replay_result) as f:
            code = f.read().strip()
        out["criterion_5_offline_replay"] = {"pass": code == "0", "exit_code": code}

    # 总判定
    crits = {k: v for k, v in out.items() if k.startswith("criterion_")}
    out["summary"] = {
        "criteria_total": 5,
        "criteria_evaluated": len(crits),
        "criteria_passed": sum(1 for v in crits.values() if v.get("pass")),
        "all_pass": len(crits) == 5 and all(v.get("pass") for v in crits.values()),
        "per_criterion": {k: bool(v.get("pass")) for k, v in sorted(crits.items())},
    }
    print(json.dumps(out, ensure_ascii=False, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
