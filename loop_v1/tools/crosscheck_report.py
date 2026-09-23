#!/usr/bin/env python3
"""crosscheck_report.py <轮目录> — 把同一窗口里两条采集通路的产物并排成一张表。

纯函数: 只读目录里已有的 JSON/文本, 不读时钟、不碰设备。同一份输入每次重算逐字节一致。

两条通路量的不是同一个东西, 所以这张表的重点不是「谁更准」, 是**哪些格子本该对上、
哪些格子本来就不可比**:

  对得上才合理 —— 功耗与温度: 两边读的是同一批 sysfs 节点
    (/sys/class/power_supply/battery/{current_now,voltage_now}, /sys/class/thermal),
    差异只该来自采样时刻与平均窗口, 差得多就是哪边算错了;
  本来就不可比 —— 帧时 p95: 我们走 ftrace kgsl 逐帧事件, HiSmartPerf 安卓侧的
    实时流每秒只有一个整数 fps, 根本没有逐帧间隔。
"""
import json
import os
import sys


def load(path):
    try:
        with open(path) as fh:
            return json.load(fh)
    except Exception:
        return None


def fmt(v, unit="", nd=3):
    if v is None:
        return "未测到"
    if isinstance(v, (int, float)):
        return f"{v:.{nd}f}{unit}"
    return str(v)


def delta(a, b):
    """两个读数的差与相对差。任一侧缺失就没有差可言 —— 不拿 0 当缺失值的替身。"""
    if a is None or b is None:
        return "—", "—"
    d = a - b
    if b == 0:
        return f"{d:+.3f}", "—"
    return f"{d:+.3f}", f"{100.0 * d / b:+.1f}%"


def thermal_zones(path):
    """窗口中点直读的 /sys/class/thermal → {类型: 摄氏度}。

    内核这里的单位是毫摄氏度; 但不同热区偶有直接给摄氏度的, 故按量级判:
    大于 1000 视为毫摄氏度。**0 不是 0 摄氏度**, 是这台机器没有这个传感器, 一律丢掉。
    """
    out = {}
    try:
        with open(path, errors="replace") as fh:
            for line in fh:
                parts = line.split()
                if len(parts) != 2:
                    continue
                name, raw = parts
                try:
                    v = int(raw)
                except ValueError:
                    continue
                if v == 0:
                    continue
                out.setdefault(name, v / 1000.0 if abs(v) > 1000 else float(v))
    except OSError:
        pass
    return out


def unavailable_lines(snap, tag):
    if not snap:
        return [f"  {tag}: 这条通路整份产物都没读到"]
    return [f"  {tag} {u['field']}: {u['reason']}" for u in snap.get("unavailable", [])]


def main() -> int:
    if len(sys.argv) < 2:
        print("用法: crosscheck_report.py <轮目录>", file=sys.stderr)
        return 2
    d = sys.argv[1]

    ft = load(os.path.join(d, "summary.json"))          # 我们的: ftrace 逐帧
    sysfs = load(os.path.join(d, "perf_sysfs.json"))    # 我们的: sysfs 电源轨
    sp = load(os.path.join(d, "perf_smartperf.json"))   # HiSmartPerf 安卓通路
    zones = thermal_zones(os.path.join(d, "thermal_mid.txt"))

    ft = ft or {}
    sysfs = sysfs or {}
    sp = sp or {}

    L = []
    L.append("═══ 两通路并排 ═══")
    cond = os.path.join(d, "test_conditions.txt")
    if os.path.exists(cond):
        L.append("  测试条件 (两条通路共享同一套, 因为是同一窗口并排采的):")
        with open(cond, errors="replace") as fh:
            for line in fh:
                if line.strip():
                    L.append(f"    {line.rstrip()}")
    L.append(f"  HiSmartPerf 采样点 {sp.get('sample_count', 0)} 条 (每秒一条)")
    L.append(f"  sysfs 电源轨采样点 {sysfs.get('sample_count', 0)} 条 (每 200ms 一条)")
    L.append(f"  ftrace 帧数 {ft.get('n_frames', '—')} 帧")
    L.append("")

    rows = []

    # ── 帧率: 两边都给得出, 但口径不同 (kgsl 提交 vs SurfaceFlinger 图层) ──
    rows.append(("帧率 fps", sp.get("fps"), ft.get("fps_mean"), "fps", "口径不同: 见下"))

    # ── 帧时均值 ──
    rows.append((
        "帧时均值 ms",
        sp.get("frame_time_mean_ms"),
        ft.get("frame_mean"),
        "ms",
        "HiSmartPerf 侧是 1000/每秒fps 反推, 非逐帧测量",
    ))

    # ── 帧时 p95: HiSmartPerf 安卓侧给不出 ──
    rows.append((
        "帧时 p95 ms",
        sp.get("fps_p95_ms"),
        ft.get("frame_p95"),
        "ms",
        "不可比: 安卓侧实时流没有逐帧间隔",
    ))

    # ── 功耗: 两边读同一对 sysfs 节点, 本该对上 ──
    rows.append((
        "整机功耗 W",
        sp.get("power_watt"),
        sysfs.get("power_watt"),
        "W",
        "同源 (battery/current_now x voltage_now), 本该对上",
    ))

    # ── 温度: HiSmartPerf 报的 vs 窗口中点直读 ──
    for field, label, cands in [
        ("soc_temp_c", "SoC 温度 C", ("soc_thermal", "soc-thermal", "soc")),
        ("gpu_temp_c", "GPU 温度 C", ("gpu", "gpuss-0", "gpu-thermal", "gpuss")),
        ("battery_temp_c", "电池温度 C", ("Battery", "battery", "batt_therm")),
    ]:
        ours = next((zones[c] for c in cands if c in zones), None)
        rows.append((label, sp.get(field), ours, "C", "同源 (/sys/class/thermal), 本该对上"))

    w = max(len(r[0]) for r in rows)
    L.append(f"{'指标'.ljust(w)} │ {'HiSmartPerf':>14} │ {'我们的':>14} │ {'差':>10} │ {'相对':>8} │ 说明")
    L.append("─" * (w + 78))
    for name, a, b, unit, note in rows:
        dv, dp = delta(a, b)
        L.append(
            f"{name.ljust(w)} │ {fmt(a, unit):>14} │ {fmt(b, unit):>14} │ {dv:>10} │ {dp:>8} │ {note}"
        )

    L.append("")
    L.append("── 各通路自报的「这个字段为什么没有」 ──")
    L += unavailable_lines(sp, "HiSmartPerf")
    L += unavailable_lines(sysfs, "sysfs")

    if zones:
        L.append("")
        L.append("── 窗口中点直读的热区 (非零者) ──")
        for k in sorted(zones):
            L.append(f"  {k}: {zones[k]:.1f} C")

    raw = os.path.join(d, "gp_realtime.txt")
    if os.path.exists(raw):
        fps = []
        with open(raw, errors="replace") as fh:
            for rec in fh.read().split("}"):
                i = rec.find("fps:")
                if i < 0:
                    continue
                v = rec[i + 4:].split(";")[0].strip()
                if v.lstrip("-").isdigit():
                    fps.append(int(v))
        if fps:
            L.append("")
            L.append("── HiSmartPerf 逐秒 fps 原始序列 ──")
            L.append("  " + " ".join(str(v) for v in fps))
            body = fps[1:-1] if len(fps) > 2 else fps
            L.append(
                f"  首尾两秒是不完整的秒, 照样各算一条样本 —— 去掉之后均值 "
                f"{sum(body) / len(body):.3f} (全量 {sum(fps) / len(fps):.3f})"
            )

    meta = sp.get("meta") or {}
    if meta:
        L.append("")
        L.append("── HiSmartPerf 侧采集元数据 ──")
        for k in sorted(meta):
            L.append(f"  {k}: {meta[k]}")

    print("\n".join(L))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
