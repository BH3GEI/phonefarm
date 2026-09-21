#!/usr/bin/env python3
"""attribute.py — 从一轮 summary.json 判定"这一帧的时间花在哪类开销上" (判据 2)

本机没有 Perfetto 的 gpu.renderstages (高通未在该驱动注册 GPU producer), 所以做不到
render pass 级归因。目标允许的另一半口径是"哪类开销", 本模块给的就是这个, 并且每条
结论都挂着产生它的那个实测量, 不做无据推断。

判定树 (顺序即优先级, 先命中先返回)
-----------------------------------
1. 热降频        : trace 里出现 kgsl_thermal_constraint / kgsl_clock_throttling
                   → 再快的 GPU 也没用, 先解热
2. GPU 计算受限  : gpu_active / frame_period > 0.90
                   → GPU 几乎整帧都在跑, 减少 GPU 工作量才有收益
3. 限帧器封顶    : frame_period 贴着某个常见帧率上限 (30/45/60/90/120) 的 ±2%,
                   且 gpu_active/frame_period < 0.90
                   → 帧时间是被游戏自己的限帧器钉住的, 不是被硬件顶住的。
                     **此时 p50 在物理上不可能降低**, 能动的只有两样:
                     抖动 (p95-p50) 和 每帧 GPU 工作量 (省下的是功耗与热预算)
4. 提交/同步受限 : queue / frame_period > 0.25
5. 其余          : 无单一主因

带宽压力是正交的一维, 单独给, 不进上面的判定树
-----------------------------------------------
GPU 侧对总线的带宽投票 (kgsl_buslevel.avg_bw) 与 DDR 实际运行频率一起看:
DDR 跑在上限的比例越高, 说明访存越吃紧。这一维决定"带宽旋钮值不值得拧"。
"""
from __future__ import annotations
import json
import sys

FPS_CAPS = [30, 45, 60, 90, 120, 144]
GPU_BOUND_RATIO = 0.90
QUEUE_BOUND_RATIO = 0.25
CAP_TOLERANCE = 0.02


def detect_fps_cap(frame_p50_ms: float) -> int | None:
    """帧周期是否贴着某个常见限帧上限。贴得上说明是软件限帧, 不是硬件跑不动。"""
    for cap in FPS_CAPS:
        target = 1000.0 / cap
        if abs(frame_p50_ms - target) / target <= CAP_TOLERANCE:
            return cap
    return None


def attribute(s: dict) -> dict:
    fp50 = s.get("frame_p50") or 0.0
    fp95 = s.get("frame_p95") or 0.0
    gpu = s.get("gpu_active_mean") or 0.0
    queue = s.get("queue_p50") or 0.0
    n_thermal = s.get("n_thermal_events") or 0

    gpu_share = gpu / fp50 if fp50 else 0.0
    queue_share = queue / fp50 if fp50 else 0.0
    jitter_ms = fp95 - fp50
    cap = detect_fps_cap(fp50)

    # ── 带宽维 (正交) ──
    ddr_cur = float(s.get("ddr_cur_khz") or 0)
    ddr_boost = float(s.get("ddr_boost_khz") or 0)
    bw_med = s.get("bw_median")
    bw_max = s.get("bw_max")
    bw = {
        "ddr_cur_khz": ddr_cur,
        "ddr_boost_khz": ddr_boost,
        "gpu_bus_vote_median": bw_med,
        "gpu_bus_vote_max": bw_max,
        "bus_vote_headroom_pct": (round((1 - bw_med / bw_max) * 100, 2)
                                  if bw_med and bw_max else None),
    }

    # ── 主因判定 ──
    if n_thermal > 0:
        verdict, why = "热降频受限", f"trace 内出现 {n_thermal} 次 kgsl 热/降频事件"
        actionable = "先解热: 降低每帧工作量或放宽散热, 提频无效"
    elif gpu_share > GPU_BOUND_RATIO:
        verdict = "GPU 计算受限"
        why = f"GPU 活跃 {gpu:.2f}ms 占帧周期 {fp50:.2f}ms 的 {gpu_share*100:.1f}% (>{GPU_BOUND_RATIO*100:.0f}%)"
        actionable = "减少每帧 GPU 工作量 (分辨率/着色/overdraw) 才有收益"
    elif cap is not None:
        verdict = f"限帧器封顶 @{cap}fps"
        why = (f"帧周期 {fp50:.2f}ms 贴着 {cap}fps 的 {1000/cap:.2f}ms (±{CAP_TOLERANCE*100:.0f}%), "
               f"而 GPU 只用掉 {gpu_share*100:.1f}%, 余量 {fp50-gpu:.2f}ms")
        actionable = ("p50 被软件限帧钉死, 物理上降不下去。可动的只有: "
                      f"抖动 p95-p50={jitter_ms:.2f}ms, 以及每帧 GPU 工作量 {gpu:.2f}ms (省功耗/热预算)")
    elif queue_share > QUEUE_BOUND_RATIO:
        verdict = "提交/同步受限"
        why = f"排队开销 {queue:.2f}ms 占帧周期 {queue_share*100:.1f}% (>{QUEUE_BOUND_RATIO*100:.0f}%)"
        actionable = "查 CPU 侧提交节奏与 fence 等待"
    else:
        verdict = "无单一主因"
        why = (f"GPU 占比 {gpu_share*100:.1f}%, 排队占比 {queue_share*100:.1f}%, "
               f"无热事件, 帧周期不贴任何常见限帧上限")
        actionable = "需要更细的归因面才能定位"

    return {
        "verdict": verdict,
        "evidence": why,
        "actionable": actionable,
        "measures": {
            "frame_p50_ms": round(fp50, 4),
            "frame_p95_ms": round(fp95, 4),
            "jitter_p95_minus_p50_ms": round(jitter_ms, 4),
            "gpu_active_mean_ms": round(gpu, 4),
            "gpu_share_pct": round(gpu_share * 100, 2),
            "queue_p50_ms": round(queue, 4),
            "queue_share_pct": round(queue_share * 100, 2),
            "n_thermal_events": n_thermal,
            "detected_fps_cap": cap,
        },
        "bandwidth": bw,
    }


def main() -> int:
    if len(sys.argv) < 2:
        print("用法: attribute.py <summary.json>", file=sys.stderr)
        return 2
    with open(sys.argv[1]) as fh:
        s = json.load(fh)
    print(json.dumps(attribute(s), ensure_ascii=False, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
