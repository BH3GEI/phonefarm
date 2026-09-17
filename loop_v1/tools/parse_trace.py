#!/usr/bin/env python3
"""parse_trace.py — ftrace 文本 → 帧时序 + GPU 归因指标 (纯函数, 无副作用)

输入是 ftrace_capture.sh 落的文本, 输出一份 JSON。**不碰设备**, 因此可对着
归档的 trace 离线重跑, 结果逐字节一致 (判据 5 的可回放性就靠这一点)。

指标口径
--------
frame_ms      : 一帧的墙钟时长。**一帧不等于一次 GPU 提交** —— 原神在本机每帧发
                2 次 cmdbatch (实测间隔严格交替 ~5ms / ~28ms, 每对之和 33.3ms = 30fps)。
                所以先由 detect_submits_per_frame() 从数据自检出每帧几次提交, 再按组算。
                自检方式是提交间隔序列的自相关 (细节与踩过的坑见该函数注释)。
                真实负载下每帧提交数可能变, 所以不写死, 每次解析都重新自检并记录。
gpu_active_ms : 一帧内所有提交的 active 字段之和, 单位换算自 19.2MHz GPU tick。
                = GPU 真正在跑这一帧命令的时间, 不含排队等待。
queue_ms      : 提交 → 退役的墙钟延迟减去 gpu_active, 即排队 + 同步开销。
avg_bw        : kgsl_buslevel 事件的 avg_bw 字段, GPU 侧总线带宽投票 (MB/s 量级)。
                这是直接观测量, 不是从频率反推的。

为什么用提交节奏而不是 vsync
----------------------------
vsync (encoder_vblank_callback) 是显示刷新节奏, 恒定跟着面板刷新率跳, 游戏 30fps
锁帧时它照样 60/120 次每秒; 提交节奏才反映游戏实际出帧速度。
"""
from __future__ import annotations
import json
import re
import sys
from statistics import median

# ftrace 行首: "  UnityGfxDeviceW-32074   [005] ..... 1044964.694371: event_name: rest"
LINE_RE = re.compile(
    r"^\s*(?P<comm>.+?)-(?P<tid>\d+)\s+\[(?P<cpu>\d+)\]\s+\S+\s+(?P<ts>\d+\.\d+):\s+(?P<event>\w+):\s*(?P<rest>.*)$"
)
KV_RE = re.compile(r"(\w+)=(-?\d+)")

GPU_TICK_HZ = 19_200_000  # 实测: Δticks / Δ墙钟 = 19.21e6, 即 XO 19.2MHz


def _kv(rest: str) -> dict[str, int]:
    """把 'ctx=41 ts=7151 active=3146' 这类尾串抽成字典 (只取整数字段)。"""
    return {k: int(v) for k, v in KV_RE.findall(rest)}


def _cv(xs: list[float]) -> float:
    """变异系数 std/mean。空或均值为 0 时返回正无穷, 让它在择优里自然出局。"""
    if len(xs) < 2:
        return float("inf")
    mu = sum(xs) / len(xs)
    if mu <= 0:
        return float("inf")
    var = sum((x - mu) ** 2 for x in xs) / (len(xs) - 1)
    return (var ** 0.5) / mu


def _acf(xs: list[float], lag: int) -> float:
    """滞后 lag 的样本自相关 (Pearson)。样本不足或方差为 0 时返回 0。"""
    n = len(xs) - lag
    if n < 3:
        return 0.0
    a, b = xs[:n], xs[lag:lag + n]
    ma, mb = sum(a) / n, sum(b) / n
    num = sum((a[i] - ma) * (b[i] - mb) for i in range(n))
    da = sum((v - ma) ** 2 for v in a) ** 0.5
    db = sum((v - mb) ** 2 for v in b) ** 0.5
    return num / (da * db) if da > 0 and db > 0 else 0.0


ACF_POSITIVE_GATE = 0.30


def detect_submits_per_frame(gaps: list[float], max_n: int = 6) -> tuple[int, dict[str, dict[int, float]]]:
    """从提交间隔序列自检"每帧几次提交"——用自相关, 不用方差。

    为什么不用"哪个 N 的方差最小"
    ------------------------------
    试过, 不稳。把 N 个间隔求和当一帧, 均值按 N 涨而标准差只按 sqrt(N) 涨, 所以裸 CV
    天然随 N 单调下降, 永远选出最大的 N。按 sqrt(N) 归一化能缓解, 但帧内间隔本身是
    相关的 (短-长-短-长), 不满足独立假设, 残余偏置仍在: 实测 night1 的归一化 CV
    N=2 是 0.155 / N=6 是 0.150, 只差 3.5% 能靠容差救回来; 到了 night4 变成
    N=2 是 0.174 / N=6 是 0.147, 差 18%, 容差救不回来, 于是把周期判成 6,
    帧时间整整虚高 3 倍 (100.6ms 而不是 33.5ms)。

    自相关直接量周期性
    ------------------
    交替的短-长序列, 滞后 1 是强负相关、滞后 2 是强正相关。这是周期本身的性质,
    与"求和平均掉多少方差"无关, 所以不存在上面那种偏置。实测四个 run 全部给出
    lag1 ≈ -0.93, lag2 ≈ +0.90 —— 判别余量极大, 不靠任何容差。

    判据: 取**最小**的、自相关为正且超过阈值的滞后。周期的倍数也会是正相关
    (lag4/lag6 同样正), 取最小的那个才是真周期。

    返回 (N, {"acf": {lag: r}, "cv_norm": {N: 归一化CV}}) —— 两套评分都留在报告里,
    自相关是判据, 归一化 CV 作旁证。
    """
    acf = {L: _acf(gaps, L) for L in range(1, max_n + 1) if len(gaps) >= L + 3}
    cv_norm: dict[int, float] = {}
    for n in range(1, max_n + 1):
        if len(gaps) < n * 3:
            continue
        groups = [sum(gaps[i:i + n]) for i in range(0, len(gaps) - n + 1, n)]
        cv_norm[n] = _cv(groups) * (n ** 0.5)

    best = 1
    for L in sorted(acf):
        if acf[L] >= ACF_POSITIVE_GATE:
            best = L
            break
    else:
        # 一个正峰都没有 = 看不出周期性, 退回"一次提交一帧"并如实记录
        best = 1
    return best, {"acf": {k: round(v, 5) for k, v in sorted(acf.items())},
                  "cv_norm": {k: round(v, 5) for k, v in sorted(cv_norm.items())}}


def parse(text: str, render_comm: str = "UnityGfxDeviceW") -> dict:
    meta: dict[str, str] = {}
    submits: list[tuple[float, int, int]] = []   # (wall_s, ctx, ts)
    retires: dict[tuple[int, int], dict] = {}    # (ctx, ts) -> {wall_s, active, start, retire}
    buslevels: list[tuple[float, int, int]] = [] # (wall_s, avg_bw, pwrlevel)
    pwrlevels: list[tuple[float, int]] = []      # (wall_s, pwrlevel)
    gpufreqs: list[tuple[float, int]] = []       # (wall_s, freq)
    thermal_events: list[tuple[float, str]] = []
    render_ctxs: set[int] = set()

    for raw in text.splitlines():
        if raw.startswith("#loop_v1_meta"):
            for tok in raw.split()[1:]:
                if "=" in tok:
                    k, _, v = tok.partition("=")
                    meta[k] = v
            continue
        if raw.startswith("#"):
            continue
        m = LINE_RE.match(raw)
        if not m:
            continue
        ev = m.group("event")
        if ev not in ("adreno_cmdbatch_submitted", "adreno_cmdbatch_retired",
                      "kgsl_buslevel", "kgsl_pwrlevel", "kgsl_gpu_frequency",
                      "kgsl_thermal_constraint", "kgsl_clock_throttling",
                      "kgsl_bcl_clock_throttling"):
            continue
        wall = float(m.group("ts"))
        rest = m.group("rest")
        kv = _kv(rest)

        if ev == "adreno_cmdbatch_submitted":
            if m.group("comm").strip() == render_comm:
                ctx, ts = kv.get("ctx"), kv.get("ts")
                if ctx is not None and ts is not None:
                    render_ctxs.add(ctx)
                    submits.append((wall, ctx, ts))
        elif ev == "adreno_cmdbatch_retired":
            ctx, ts = kv.get("ctx"), kv.get("ts")
            if ctx is not None and ts is not None:
                retires[(ctx, ts)] = {
                    "wall": wall,
                    "active": kv.get("active", 0),
                    "start": kv.get("start", 0),
                    "retire": kv.get("retire", 0),
                }
        elif ev == "kgsl_buslevel":
            buslevels.append((wall, kv.get("avg_bw", 0), kv.get("pwrlevel", -1)))
        elif ev == "kgsl_pwrlevel":
            pwrlevels.append((wall, kv.get("pwrlevel", -1)))
        elif ev == "kgsl_gpu_frequency":
            # 字段名各内核版本不一, 取第一个像频率的值
            f = kv.get("gpu_freq") or kv.get("freq") or kv.get("new_freq") or 0
            gpufreqs.append((wall, f))
        else:
            thermal_events.append((wall, ev))

    submits.sort()

    # ── 提交间隔 → 自检每帧提交数 → 按帧分组 ──
    gaps = [
        (submits[i][0] - submits[i - 1][0]) * 1000.0
        for i in range(1, len(submits))
    ]
    spf, spf_scores = detect_submits_per_frame(gaps)

    # 帧时长 = 每 spf 个间隔一组求和。组边界同时用来聚合该帧的 GPU 活跃时长。
    frame_ms: list[float] = []
    gpu_active_ms: list[float] = []
    queue_ms: list[float] = []
    for i in range(0, len(gaps) - spf + 1, spf):
        frame_ms.append(sum(gaps[i:i + spf]))
        # 该帧覆盖 submits[i] .. submits[i+spf] (含右端: 下一帧的起点前一次提交)
        act_sum = 0.0
        q_sum = 0.0
        matched = 0
        for j in range(i, min(i + spf, len(submits))):
            wall, ctx, ts = submits[j]
            r = retires.get((ctx, ts))
            if not r:
                continue
            a = r["active"] / GPU_TICK_HZ * 1000.0
            act_sum += a
            q_sum += max(0.0, (r["wall"] - wall) * 1000.0 - a)
            matched += 1
        if matched:
            gpu_active_ms.append(act_sum)
            queue_ms.append(q_sum)

    span = (submits[-1][0] - submits[0][0]) if len(submits) >= 2 else 0.0

    return {
        "meta": meta,
        "render_comm": render_comm,
        "render_ctxs": sorted(render_ctxs),
        "span_s": round(span, 4),
        "n_submits": len(submits),
        "submits_per_frame": spf,
        "spf_scores": spf_scores,
        "n_matched_retires": len(gpu_active_ms),
        "fps_mean": round(len(frame_ms) / span, 3) if span > 0 else None,
        "frame_ms": frame_ms,
        "gpu_active_ms": gpu_active_ms,
        "queue_ms": queue_ms,
        "buslevel": [{"t": round(t, 4), "avg_bw": bw, "pwrlevel": pl} for t, bw, pl in buslevels],
        "pwrlevel": [{"t": round(t, 4), "pwrlevel": pl} for t, pl in pwrlevels],
        "gpufreq": [{"t": round(t, 4), "freq": f} for t, f in gpufreqs],
        "thermal_events": [{"t": round(t, 4), "event": e} for t, e in thermal_events],
    }


def pct(xs: list[float], p: float) -> float | None:
    """线性插值分位数 (与 numpy 默认口径一致), 空列表返回 None。"""
    if not xs:
        return None
    s = sorted(xs)
    if len(s) == 1:
        return s[0]
    k = (len(s) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


def summarize(d: dict) -> dict:
    """把逐帧序列压成一行可比的账。每个字段都是确定性函数, 无随机。"""
    f, g, q = d["frame_ms"], d["gpu_active_ms"], d["queue_ms"]
    bw = [b["avg_bw"] for b in d["buslevel"]]
    return {
        "span_s": d["span_s"],
        "n_frames": len(f),
        "n_submits": d["n_submits"],
        "submits_per_frame": d["submits_per_frame"],
        "spf_scores": d["spf_scores"],
        "fps_mean": d["fps_mean"],
        "frame_p50": round(pct(f, 0.50), 4) if f else None,
        "frame_p95": round(pct(f, 0.95), 4) if f else None,
        "frame_p99": round(pct(f, 0.99), 4) if f else None,
        "frame_mean": round(sum(f) / len(f), 4) if f else None,
        "gpu_active_p50": round(pct(g, 0.50), 4) if g else None,
        "gpu_active_p95": round(pct(g, 0.95), 4) if g else None,
        "gpu_active_mean": round(sum(g) / len(g), 4) if g else None,
        "queue_p50": round(pct(q, 0.50), 4) if q else None,
        "queue_p95": round(pct(q, 0.95), 4) if q else None,
        "bw_median": median(bw) if bw else None,
        "bw_max": max(bw) if bw else None,
        "n_buslevel_events": len(bw),
        "n_thermal_events": len(d["thermal_events"]),
        "ddr_cur_khz": d["meta"].get("ddr_cur_khz"),
        "ddr_boost_khz": d["meta"].get("ddr_boost_khz"),
        "llcc_boost_khz": d["meta"].get("llcc_boost_khz"),
        "thermal_pwrlevel": d["meta"].get("thermal_pwrlevel"),
    }


def main() -> int:
    if len(sys.argv) < 2:
        print("用法: parse_trace.py <trace.txt> [--full] [--comm 线程名]", file=sys.stderr)
        return 2
    path = sys.argv[1]
    comm = "UnityGfxDeviceW"
    if "--comm" in sys.argv:
        comm = sys.argv[sys.argv.index("--comm") + 1]
    with open(path, "r", errors="replace") as fh:
        d = parse(fh.read(), render_comm=comm)
    out = d if "--full" in sys.argv else summarize(d)
    print(json.dumps(out, ensure_ascii=False, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
