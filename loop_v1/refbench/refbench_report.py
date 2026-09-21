#!/usr/bin/env python3
"""refbench_report.py — refbench M0 六条判据汇总 (纯函数, 字节可复现)

与 loop_v1/tools/report.py 同一纪律: 不带时间戳、不带绝对路径、无随机数,
同样的输入每次产出逐字节相同的 report.json。回放自检直接 cmp 本文件的输出。

判定规则先于数据冻结: RULES 与本文件自身的 sha256 一并写进报告 —— 事后改规则
哈希对不上, 报告自判无效 (harness v2 判据 5 的种子)。

判据 4 的归因映射 (标准答案在 refbench 的 DESIGN.md §4):
  declared=bandwidth → 主因「GPU 计算受限」且 bus vote 显著高 (≥ frag 的 3 倍)
  declared=fragment  → 主因「GPU 计算受限」且 bus vote 显著低 (上式另一端)
  declared=none      → 主因「限帧器封顶 @面板刷新率」(vsync, 不得无中生有)
带宽维在 attribute.py 里是正交维不是主因 —— 访存停顿在 kgsl active 里同样计忙,
所以 bandwidth 与 fragment 靠 bus vote 分离, 不靠主因字符串。
"""
from __future__ import annotations
import argparse
import glob
import hashlib
import json
import os
import sys

_TOOLS = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tools"))
sys.path.insert(0, _TOOLS)
from analyze import dispersion, drift, mean, compare  # noqa: E402
from report import snapshot_diff                       # noqa: E402
from statistics import median                          # noqa: E402

RULES = {
    "primary_metric": "frame_p95",
    "dispersion_gate_pct": 0.5,     # 判据 1: (max-min)/median
    "monotonic_gate": 0.75,          # 判据 1: 相邻递增比例上限 (< 0.75)
    "intensity_min_step_pct": 10.0,  # 判据 2: 阶梯相邻档最小涨幅
    "alpha": 0.05,                   # 判据 3: 精确置换检验
    "bw_separation_x": 3.0,          # 判据 4: bandwidth/fragment 的 bus vote 分离倍数
    "expected_declared": {"bw_pingpong": "bandwidth", "frag_alu": "fragment", "idle_cap": "none"},
    "verdict_gpu": "GPU 计算受限",
    "verdict_cap_prefix": "限帧器封顶",
}


def rules_sha256() -> str:
    with open(os.path.abspath(__file__), "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def load_valid(root: str, pattern: str) -> list[dict]:
    """按目录名排序载入有效轮; 带 INVALID 标记的轮保留在盘但不进样本。"""
    out = []
    for p in sorted(glob.glob(os.path.join(root, pattern))):
        if not os.path.isdir(p) or os.path.exists(os.path.join(p, "INVALID")):
            continue
        need = [os.path.join(p, f) for f in ("summary.json", "refbench_out.json", "attribution.json")]
        if not all(os.path.exists(f) for f in need):
            continue
        with open(need[0]) as f:
            s = json.load(f)
        with open(need[1]) as f:
            r = json.load(f)
        with open(need[2]) as f:
            a = json.load(f)
        out.append({"label": os.path.basename(p), "summary": s, "rb": r, "attr": a})
    return out


def criterion_1(ctrl: list[dict]) -> dict:
    m = RULES["primary_metric"]
    xs = [r["summary"][m] for r in ctrl if isinstance(r["summary"].get(m), (int, float))]
    disp = dispersion(xs)
    d = drift(xs)
    disp_pct = round(disp * 100, 4) if disp is not None else None
    mono = d["monotonic_frac"]
    ok = (disp_pct is not None and disp_pct < RULES["dispersion_gate_pct"]
          and mono is not None and mono < RULES["monotonic_gate"])
    return {"pass": bool(ok), "metric": m, "values": xs, "dispersion_pct": disp_pct,
            "gate_pct": RULES["dispersion_gate_pct"], "drift": d,
            "monotonic_gate": RULES["monotonic_gate"],
            "runs": [r["label"] for r in ctrl]}


def criterion_2(steps: list[dict]) -> dict:
    """强度阶梯: 帧时间必须跟着负载走, 且没有一档被判「限帧器封顶」(钉死)。"""
    pts = [{"label": r["label"], "intensity": r["rb"]["params"]["intensity"],
            "frame_p50": r["summary"]["frame_p50"], "verdict": r["attr"]["verdict"]}
           for r in steps]
    pts.sort(key=lambda x: x["intensity"])
    increasing = all(
        pts[i + 1]["frame_p50"] >= pts[i]["frame_p50"] * (1 + RULES["intensity_min_step_pct"] / 100)
        for i in range(len(pts) - 1))
    pinned = [p["label"] for p in pts if p["verdict"].startswith(RULES["verdict_cap_prefix"])]
    ok = len(pts) >= 3 and increasing and not pinned
    return {"pass": bool(ok), "points": pts, "monotone_with_min_step": increasing,
            "min_step_pct": RULES["intensity_min_step_pct"], "pinned_runs": pinned}


def criterion_3(ctrl: list[dict], knob: list[dict]) -> dict:
    ctrl_knobs = sorted({json.dumps(r["rb"]["effective_knobs"], sort_keys=True) for r in ctrl})
    knob_knobs = sorted({json.dumps(r["rb"]["effective_knobs"], sort_keys=True) for r in knob})
    knobs_differ = (len(ctrl_knobs) == 1 and len(knob_knobs) == 1 and ctrl_knobs != knob_knobs)
    a = [(r["label"], r["summary"]) for r in ctrl]
    b = [(r["label"], r["summary"]) for r in knob]
    cmp = compare(a, b, RULES["primary_metric"])
    improved = isinstance(cmp.get("diff_mean"), (int, float)) and cmp["diff_mean"] < 0
    sig = isinstance(cmp.get("perm_p_two_sided"), (int, float)) and cmp["perm_p_two_sided"] < RULES["alpha"]
    return {"pass": bool(knobs_differ and improved and sig),
            "effective_knobs_ctrl": ctrl_knobs, "effective_knobs_knob": knob_knobs,
            "knobs_differ": knobs_differ, "comparison": cmp, "alpha": RULES["alpha"]}


def criterion_4(bw: list[dict], frag: list[dict], idle: list[dict]) -> dict:
    """归因可证伪: loop_v1 的结论必须与靶子自报的瓶颈类型按映射一致。"""
    def declared_ok(runs, scene):
        return all(r["rb"]["scene"] == scene
                   and r["rb"]["declared_bottleneck"] == RULES["expected_declared"][scene]
                   for r in runs)

    checks = {
        "declared_sanity": bool(declared_ok(bw, "bw_pingpong") and declared_ok(frag, "frag_alu")
                                 and declared_ok(idle, "idle_cap")),
        "bw_verdicts": [r["attr"]["verdict"] for r in bw],
        "frag_verdicts": [r["attr"]["verdict"] for r in frag],
        "idle_verdicts": [r["attr"]["verdict"] for r in idle],
    }
    checks["bw_all_gpu"] = all(v == RULES["verdict_gpu"] for v in checks["bw_verdicts"])
    checks["frag_all_gpu"] = all(v == RULES["verdict_gpu"] for v in checks["frag_verdicts"])
    checks["idle_all_cap"] = all(v.startswith(RULES["verdict_cap_prefix"]) for v in checks["idle_verdicts"])

    bw_votes = [r["summary"]["bw_median"] for r in bw if isinstance(r["summary"].get("bw_median"), (int, float))]
    fr_votes = [r["summary"]["bw_median"] for r in frag if isinstance(r["summary"].get("bw_median"), (int, float))]
    checks["bus_vote_median_bw"] = median(bw_votes) if bw_votes else None
    checks["bus_vote_median_frag"] = median(fr_votes) if fr_votes else None
    checks["bus_separation_x"] = (round(checks["bus_vote_median_bw"] / checks["bus_vote_median_frag"], 2)
                                  if checks["bus_vote_median_bw"] and checks["bus_vote_median_frag"] else None)
    checks["bus_separated"] = bool(checks["bus_separation_x"] is not None
                                   and checks["bus_separation_x"] >= RULES["bw_separation_x"])
    ok = (checks["declared_sanity"] and len(bw) >= 2 and len(frag) >= 2 and len(idle) >= 2
          and checks["bw_all_gpu"] and checks["frag_all_gpu"] and checks["idle_all_cap"]
          and checks["bus_separated"])
    mapping = {
        "bandwidth": "verdict==GPU 计算受限 且 bus vote ≥ frag 的 %.0f 倍" % RULES["bw_separation_x"],
        "fragment": "verdict==GPU 计算受限 且 bus vote 为分离的低端",
        "none": "verdict 以「限帧器封顶」开头",
    }
    return {"pass": bool(ok), "mapping": mapping, **checks,
            "n_runs": {"bw": len(bw), "frag": len(frag), "idle": len(idle)}}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=".")
    a = ap.parse_args()
    root = a.root

    ctrl = load_valid(root, "bw_ctrl*")
    knob = load_valid(root, "bw_knob*")
    steps = load_valid(root, "inten_i*")
    frag = load_valid(root, "frag*")
    idle = load_valid(root, "idle*")

    out: dict = {
        "goal": "refbench M0 — 白盒基准靶场首轮闭环",
        "rules": RULES,
        "rules_sha256": rules_sha256(),
        "criterion_1_repeatable": criterion_1(ctrl),
        "criterion_2_ceiling_real": criterion_2(steps),
        "criterion_3_knob_effective": criterion_3(ctrl, knob),
        "criterion_4_attribution_falsifiable": criterion_4(ctrl, frag, idle),
    }

    sd = snapshot_diff(os.path.join(root, "snap_before.txt"), os.path.join(root, "snap_final.txt"))
    out["criterion_5_no_residue"] = {"pass": bool(sd.get("identical")), **sd}

    replay_file = os.path.join(root, "replay_exit.txt")
    if os.path.exists(replay_file):
        with open(replay_file) as f:
            code = f.read().strip()
        out["criterion_6_offline_replay"] = {"pass": code == "0", "exit_code": code,
                                             "note": "逐轮 summary/attribution 重算字节一致; 报告自身的重放一致性由 replay_exit2.txt 另证"}
    else:
        out["criterion_6_offline_replay"] = {"pass": False, "reason": "replay_exit.txt 缺失"}

    crits = {k: v for k, v in out.items() if k.startswith("criterion_")}
    out["summary"] = {
        "criteria_total": 6,
        "criteria_passed": sum(1 for v in crits.values() if v.get("pass")),
        "all_pass": all(v.get("pass") for v in crits.values()),
        "per_criterion": {k: bool(v.get("pass")) for k, v in sorted(crits.items())},
    }
    print(json.dumps(out, ensure_ascii=False, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
