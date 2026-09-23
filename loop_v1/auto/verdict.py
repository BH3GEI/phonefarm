#!/usr/bin/env python3
"""verdict.py — 「这组参数保留还是淘汰」的判定规则 (纯函数)。

**规则在看到任何候选数据之前就落盘** (autoloop.py 开跑时把 RULE 写进 rule.json)。
之后无论数字多难看都不改口径 —— 这是 loop_v1 三条纪律的第一条。

判定看三样, 不是只看省电
--------------------------
| 指标            | 方向   | 来源                        |
|-----------------|--------|-----------------------------|
| frame_p95       | 越小越好 | ftrace 帧时序 (帧时稳定性)   |
| fps_mean        | 越大越好 | ftrace 帧时序 (帧率)         |
| power_w_mean    | 越小越好 | power_supply 轨 (整机功耗)   |

系统参数不动画面, 所以画质天然不变, 不必量。温度不作为「更好」的加分项,
只作为**上限**: 超过 temp_cap 这一轮直接作废并还原, 不参与比较。

保留 / 淘汰
-----------
- 任一指标**显著改善** 且 **没有任何指标显著变差** → 保留
- 否则 → 淘汰

多重比较: 一次看 3 个指标, 按 0.05 判「显著改善」会把假阳性抬到约 14%。
所以「改善」一侧用 Bonferroni 收紧到 alpha/K, 「变差」一侧仍用 0.05 不收紧
—— 两侧故意不对称: 宁可漏掉一个真改善, 也不要把一个真退化放过去。
K=3 时 alpha_win=0.0167, 需要每臂至少 5 轮才够得着 (5v5 置换全枚举 252 种,
最小可能 p=0.0079)。轮数不足时任何候选都不可能判「保留」, 规则里明写这一条。
"""
from __future__ import annotations
from statistics import median

ALPHA = 0.05

# (指标, 方向) —— 方向 "lower" = 越小越好
PRIMARY_METRICS = [
    ("frame_p95", "lower"),
    ("fps_mean", "higher"),
    ("power_w_mean", "lower"),
]

RULE_VERSION = "sysparam_v1"


def rule_doc(temp_cap_c: float, pairs: int, power_available: bool) -> dict:
    """落盘用的规则快照。autoloop 在采第一组候选数据之前写它。"""
    metrics = [m for m, _ in PRIMARY_METRICS if power_available or m != "power_w_mean"]
    k = len(metrics)
    return {
        "version": RULE_VERSION,
        "alpha_regression": ALPHA,
        "alpha_win_bonferroni": round(ALPHA / k, 6) if k else None,
        "n_metrics": k,
        "metrics": [{"id": m, "better": d} for m, d in PRIMARY_METRICS
                    if power_available or m != "power_w_mean"],
        "pairs_per_candidate": pairs,
        "min_reachable_p": _min_reachable_p(pairs),
        "reachable": (_min_reachable_p(pairs) or 1.0) < (ALPHA / k if k else 0),
        "temp_cap_c": temp_cap_c,
        "power_available": power_available,
        "keep_condition": "至少一个指标显著改善 (p < alpha_win) 且没有任何指标显著变差 (p < 0.05)",
        "abort_conditions": ["旋钮未全部生效", "SoC 结温超过 temp_cap_c", "轮末快照与轮前不一致"],
    }


def _min_reachable_p(pairs: int) -> float | None:
    """n v n 精确置换检验能取到的最小双侧 p 值 = 2 / C(2n, n)。"""
    if pairs < 2:
        return None
    from math import comb
    return 2.0 / comb(2 * pairs, pairs)


# ── 设备遥测解析 (纯函数) ──

def env_stats(text: str) -> dict:
    """sample_env.sh 输出 → 功耗与温度统计。

    功耗优先用设备直报的 power_now (微瓦), 没有就用 |V x I| 折算,
    与 phonefarm hwcond.rs 的 PowerSample::watt 同口径。
    电池轨在充电态读出来的是流入功率, 量不到整机开销 —— 这种情况
    power_rail 报 "battery(charging)" 且 power_w_mean 为 None, 由上层决定降级。
    """
    watts: list[float] = []
    temps: list[float] = []
    charging = False
    n = 0
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("#battery_status="):
            charging = line.split("=", 1)[1].strip().lower() in ("charging", "full")
            continue
        if not line.startswith("ENV "):
            continue
        # ENV <uptime> <batt_uv> <batt_ua> <batt_uw|NA> <usb_uv> <usb_ua> <热区> <毫摄氏度>
        parts = line.split()
        if len(parts) < 9:
            continue
        n += 1
        try:
            bv, bi = int(parts[2]), int(parts[3])
            bp = None if parts[4] == "NA" else int(parts[4])
            t = int(parts[8])
        except ValueError:
            continue
        if bp is not None and bp != 0:
            watts.append(abs(bp) / 1e6)
        elif bv and bi:
            watts.append(abs(bv / 1e6 * bi / 1e6))
        if 0 < t < 100000:
            temps.append(t / 1000.0)

    out: dict = {
        "n_samples": n,
        "battery_charging": charging,
        "power_rail": "battery(charging)" if charging else "battery",
        "power_w_mean": None,
        "power_w_median": None,
        "soc_temp_max_c": max(temps) if temps else None,
        "soc_temp_mean_c": round(sum(temps) / len(temps), 3) if temps else None,
    }
    if watts and not charging:
        out["power_w_mean"] = round(sum(watts) / len(watts), 4)
        out["power_w_median"] = round(median(watts), 4)
    return out


# ── 判定 ──

def _significant(cmp_one: dict, alpha: float) -> bool:
    p = cmp_one.get("perm_p_two_sided")
    return p is not None and p < alpha


def decide(comparisons: dict, *, temp_max_c: float | None, temp_cap_c: float,
           apply_ok: bool, snapshot_identical: bool, power_available: bool) -> dict:
    """comparisons: {指标: analyze.compare 的输出}。返回判定结论。

    comparisons 里 a 臂是对照 (旋钮已还原), b 臂是旋钮臂 —— 与 analyze.compare
    的参数顺序一致, diff = b - a。
    """
    metrics = [(m, d) for m, d in PRIMARY_METRICS if power_available or m != "power_w_mean"]
    k = len(metrics) or 1
    alpha_win = ALPHA / k

    if not apply_ok:
        return {"verdict": "ABORT", "reason": "旋钮未全部生效, 本轮数据不算数",
                "alpha_win": alpha_win}
    if temp_max_c is not None and temp_max_c > temp_cap_c:
        return {"verdict": "ABORT", "reason": f"SoC 结温 {temp_max_c}C 超过上限 {temp_cap_c}C",
                "temp_max_c": temp_max_c, "alpha_win": alpha_win}
    if not snapshot_identical:
        return {"verdict": "ABORT", "reason": "轮末设备快照与轮前不一致, 留痕",
                "alpha_win": alpha_win}

    wins, regressions, detail = [], [], {}
    for m, direction in metrics:
        c = comparisons.get(m)
        if not c or "error" in c or c.get("diff_mean") is None:
            detail[m] = {"status": "缺数据"}
            continue
        diff = c["diff_mean"]
        improved = diff < 0 if direction == "lower" else diff > 0
        p = c["perm_p_two_sided"]
        if improved and _significant(c, alpha_win):
            wins.append(m)
            status = "显著改善"
        elif (not improved) and diff != 0 and _significant(c, ALPHA):
            regressions.append(m)
            status = "显著变差"
        else:
            status = "无显著变化"
        detail[m] = {"status": status, "diff_mean": diff, "diff_pct": c.get("diff_pct"),
                     "p": p, "a_mean": c.get("a_mean"), "b_mean": c.get("b_mean"),
                     "ci95": c.get("ci95")}

    keep = bool(wins) and not regressions
    return {
        "verdict": "KEEP" if keep else "REJECT",
        "wins": wins,
        "regressions": regressions,
        "alpha_win": round(alpha_win, 6),
        "alpha_regression": ALPHA,
        "temp_max_c": temp_max_c,
        "temp_cap_c": temp_cap_c,
        "per_metric": detail,
        "reason": ("改善 " + ",".join(wins) + " 且无显著退化") if keep
                  else ("存在显著退化: " + ",".join(regressions) if regressions
                        else "没有任何指标显著改善"),
    }
