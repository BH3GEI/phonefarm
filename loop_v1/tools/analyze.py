#!/usr/bin/env python3
"""analyze.py — 多轮 summary.json → 离散度 (判据 1) 与两臂对比 (判据 3)

全程纯标准库、无随机数、无分布假设, 因此同一批 summary.json 每次跑出的数逐字节一致
(判据 5 要的可回放性)。

两个判据各自的口径
------------------
判据 1 离散度 : (max - min) / median, 与 phonefarm bench.rs 的 DISPERSION_LIMIT_PCT
                口径一致 —— 复用既有约定, 不另造一套。
判据 3 显著性 : 不比大小, 给三样东西
                · 效应量  : Cohen's d (合并标准差归一) + Hodges-Lehmann 位移估计
                · p 值    : 精确置换检验。5v5 共 C(10,5)=252 种分组, 全枚举, 不抽样,
                            所以没有随机种子问题, 也不依赖正态性。
                · 置信区间: 置换检验反演。对一系列位移 δ, 把 B 臂整体减去 δ 后重做
                            置换检验, 收集"不被拒绝"的 δ 区间 —— 这就是精确 95% CI。
                            样本只有 5 个时, 这比套 t 分布诚实得多。
"""
from __future__ import annotations
import glob
import itertools
import json
import os
import sys
from statistics import median


# ── 基础统计 ──

def dispersion(xs: list[float]) -> float | None:
    """(max-min)/median, 返回比例 (0.05 = 5%)。中位数为 0 或样本不足时返回 None。"""
    if len(xs) < 2:
        return None
    m = median(xs)
    if m == 0:
        return None
    return (max(xs) - min(xs)) / m


def mean(xs: list[float]) -> float:
    return sum(xs) / len(xs)


def drift(xs: list[float]) -> dict:
    """检测逐轮单调漂移 —— 把"系统性漂移"和"随机抖动"分开。

    离散度只说"散得有多开", 说不出"是不是一直往一个方向走"。可重复负载要求后者为零:
    5 轮里如果每轮都比上一轮高, 那不是噪声, 是有个外生变量在动 (光照、温度、内存压力)。

    给两个量:
      slope_per_run : 最小二乘斜率 (单位/轮), 以及它占均值的百分比
      monotonic     : 相邻递增的比例。5 个点 4 个间隔, 全增 = 1.0, 全减 = 0.0,
                      纯噪声期望 0.5。偏离 0.5 越远越像系统性漂移。
    """
    n = len(xs)
    if n < 3:
        return {"slope_per_run": None, "slope_pct_per_run": None, "monotonic_frac": None}
    idx = list(range(n))
    mx, my = mean([float(i) for i in idx]), mean(xs)
    den = sum((i - mx) ** 2 for i in idx)
    slope = sum((idx[i] - mx) * (xs[i] - my) for i in range(n)) / den if den else 0.0
    ups = sum(1 for i in range(1, n) if xs[i] > xs[i - 1])
    return {
        "slope_per_run": round(slope, 5),
        "slope_pct_per_run": round(slope / my * 100, 4) if my else None,
        "monotonic_frac": round(ups / (n - 1), 3),
    }


def var(xs: list[float]) -> float:
    if len(xs) < 2:
        return 0.0
    m = mean(xs)
    return sum((x - m) ** 2 for x in xs) / (len(xs) - 1)


def cohens_d(a: list[float], b: list[float]) -> float | None:
    """(mean_b - mean_a) / 合并标准差。合并标准差为 0 时无定义。"""
    na, nb = len(a), len(b)
    if na < 2 or nb < 2:
        return None
    sp2 = ((na - 1) * var(a) + (nb - 1) * var(b)) / (na + nb - 2)
    if sp2 <= 0:
        return None
    return (mean(b) - mean(a)) / (sp2 ** 0.5)


def hodges_lehmann(a: list[float], b: list[float]) -> float:
    """两样本 HL 估计 = 所有跨组差 (b_j - a_i) 的中位数。抗离群, 与置换检验同族。"""
    return median([y - x for x in a for y in b])


# ── 精确置换检验 ──

def perm_p_two_sided(a: list[float], b: list[float]) -> tuple[float, int]:
    """均值差的精确双侧置换 p 值。

    把 a+b 合并后穷举所有"取 len(a) 个作为 A 组"的分法, 统计 |均值差| 不小于
    实测值的比例。返回 (p, 枚举总数)。
    """
    pool = list(a) + list(b)
    na = len(a)
    obs = abs(mean(b) - mean(a))
    idx = range(len(pool))
    total = 0
    hit = 0
    for combo in itertools.combinations(idx, na):
        cs = set(combo)
        ga = [pool[i] for i in idx if i in cs]
        gb = [pool[i] for i in idx if i not in cs]
        total += 1
        # 浮点容差: 等于观测值的那些分法应当计入 (置换检验惯例)
        if abs(mean(gb) - mean(ga)) >= obs - 1e-12:
            hit += 1
    return hit / total, total


def perm_ci(a: list[float], b: list[float], alpha: float = 0.05,
            steps: int = 400) -> tuple[float, float] | None:
    """置换检验反演求 (mean_b - mean_a) 的 (1-alpha) 置信区间。

    对候选位移 δ, 检验 "b - δ 与 a 同分布"; 所有不被拒绝的 δ 构成 CI。
    在一个足够宽的网格上扫描 (覆盖观测差的 ±4 倍全距), 取首尾。
    网格是固定的等分点, 因此结果确定, 不含随机。
    """
    obs = mean(b) - mean(a)
    spread = (max(list(a) + list(b)) - min(list(a) + list(b))) or 1.0
    lo_bound, hi_bound = obs - 4 * spread, obs + 4 * spread
    accepted: list[float] = []
    for i in range(steps + 1):
        d = lo_bound + (hi_bound - lo_bound) * i / steps
        shifted = [y - d for y in b]
        p, _ = perm_p_two_sided(list(a), shifted)
        if p > alpha:
            accepted.append(d)
    if not accepted:
        return None
    return (min(accepted), max(accepted))


# ── 载入 ──

def load_runs(pattern: str) -> list[tuple[str, dict]]:
    out = []
    for p in sorted(glob.glob(pattern)):
        f = os.path.join(p, "summary.json") if os.path.isdir(p) else p
        if os.path.exists(f):
            with open(f) as fh:
                out.append((os.path.basename(os.path.dirname(f)), json.load(fh)))
    return out


METRICS = ["frame_p50", "frame_p95", "frame_p99", "frame_mean",
           "gpu_active_mean", "gpu_active_p95", "queue_p50", "fps_mean", "bw_median"]


def describe(runs: list[tuple[str, dict]], title: str) -> dict:
    rep = {"title": title, "n_runs": len(runs), "runs": [r[0] for r in runs], "metrics": {}}
    for m in METRICS:
        xs = [r[1].get(m) for r in runs]
        xs = [x for x in xs if isinstance(x, (int, float))]
        if not xs:
            continue
        rep["metrics"][m] = {
            "values": xs,
            "median": round(median(xs), 4),
            "mean": round(mean(xs), 4),
            "min": round(min(xs), 4),
            "max": round(max(xs), 4),
            "dispersion_pct": round(dispersion(xs) * 100, 3) if dispersion(xs) is not None else None,
            "drift": drift(xs),
        }
    return rep


def compare(a_runs, b_runs, metric: str) -> dict:
    a = [r[1][metric] for r in a_runs if isinstance(r[1].get(metric), (int, float))]
    b = [r[1][metric] for r in b_runs if isinstance(r[1].get(metric), (int, float))]
    if len(a) < 2 or len(b) < 2:
        return {"metric": metric, "error": "样本不足"}
    p, total = perm_p_two_sided(a, b)
    ci = perm_ci(a, b)
    return {
        "metric": metric,
        "a_values": a, "b_values": b,
        "a_mean": round(mean(a), 4), "b_mean": round(mean(b), 4),
        "diff_mean": round(mean(b) - mean(a), 4),
        "diff_pct": round((mean(b) - mean(a)) / mean(a) * 100, 3) if mean(a) else None,
        "hodges_lehmann": round(hodges_lehmann(a, b), 4),
        "cohens_d": round(cohens_d(a, b), 4) if cohens_d(a, b) is not None else None,
        "perm_p_two_sided": round(p, 6),
        "perm_enumerated": total,
        "ci95": [round(ci[0], 4), round(ci[1], 4)] if ci else None,
        "significant_at_0.05": p < 0.05,
    }


def main() -> int:
    if len(sys.argv) < 2:
        print("用法: analyze.py <A臂glob> [B臂glob] [--metric M]", file=sys.stderr)
        return 2
    metric = "frame_p95"
    if "--metric" in sys.argv:
        metric = sys.argv[sys.argv.index("--metric") + 1]

    a_runs = load_runs(sys.argv[1])
    if not a_runs:
        print(f"没找到任何 summary.json: {sys.argv[1]}", file=sys.stderr)
        return 1
    out = {"arm_a": describe(a_runs, sys.argv[1])}

    pos = [x for x in sys.argv[2:] if not x.startswith("--")]
    if pos:
        b_runs = load_runs(pos[0])
        if b_runs:
            out["arm_b"] = describe(b_runs, pos[0])
            out["comparison"] = {m: compare(a_runs, b_runs, m)
                                 for m in ["frame_p95", "frame_p50", "frame_mean",
                                           "gpu_active_mean", "bw_median"]}
            out["primary_metric"] = metric
    print(json.dumps(out, ensure_ascii=False, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
