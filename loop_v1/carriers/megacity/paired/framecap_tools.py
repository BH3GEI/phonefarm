#!/usr/bin/env python3
"""framecap_tools.py — Megacity 成对采集的离线工具 (节点和开发机都能跑; 只依赖 numpy + Pillow)

  convert <framecap目录>                 raw → png (删 raw), 读 index.csv 里的宽高/格式
  perf <性能遍目录>                       samples.txt → perf.json (SurfaceFlinger 上屏帧率 + 电池功耗)
  pairs <低清遍目录> <高清遍目录> <输出目录> [--floor <高清第二遍目录>]
                                         逐帧画质差 (PSNR / MAD) + 对齐误差 → pairs.json / pairs.csv / curve.svg
  stamp <视频> <输出.csv> [--swap-w 2068]  逐帧读录屏左上角的帧序号条码 → csv (需要 ffmpeg)
  video <低清遍目录> <高清遍目录> <输出.mp4> [--perf-lr d] [--perf-hr d] [--rate 30]
                                         按帧序号对齐合成左右并排视频 (需要 ffmpeg)

画质差口径 (pairs):
  - 两张图都是游戏交给系统的最终画面 (交换链图像, 系统合成器放大到屏幕之前), 同尺寸
    (Unity 的 Screen 尺寸, 本机 2068×935)。低清遍 = URP renderScale<1 渲染 + 引擎自带放大到这个尺寸。
  - PSNR: RGB 三通道 8 bit, 全部像素, MSE 取三通道平均, PSNR = 10·log10(255² / MSE), 单位 dB。
  - MAD: 同样像素与通道上 |a-b| 的平均, 0–255 刻度。
  - 条码在抓帧之后才盖, 抓到的帧里没有条码, 不用排除区域。
对齐误差 (pairs):
  - 每 600 帧的整数倍 k 处两遍都抓了 k-2..k+2。拿低清遍的第 k 帧, 和高清遍的 k+δ (δ=-2..2)
    都缩到 1/4 再比 MAD, 取最小的 δ —— δ=0 就是「按帧序号对齐」与「按画面内容对齐」一致。
"""
import csv
import json
import math
import os
import re
import subprocess
import sys

import numpy as np
from PIL import Image

FMT_BGRA = {44, 45, 46, 47, 48, 49, 50}
FMT_RGBA = {37, 38, 39, 40, 41, 42, 43, 51, 52, 53, 54, 55, 56, 57}
FMT_A2R10 = set(range(58, 64))
FMT_A2B10 = set(range(64, 70))

# 条码几何 —— 与 knobs/gray/layer/framecap.h 一致
FC_T, FC_X0, FC_Y0, FC_BITS = 24, 16, 16, 16
FC_NT = 2 + FC_BITS + 1 + 1


def raw_to_rgb(buf, w, h, fmt):
    a = np.frombuffer(buf, dtype=np.uint8, count=w * h * 4).reshape(h, w, 4)
    if fmt in FMT_BGRA:
        return a[:, :, [2, 1, 0]].copy()
    if fmt in FMT_RGBA:
        return a[:, :, :3].copy()
    v = np.frombuffer(buf, dtype=np.uint32, count=w * h).reshape(h, w)
    c0, c1, c2 = (v & 0x3FF), ((v >> 10) & 0x3FF), ((v >> 20) & 0x3FF)
    if fmt in FMT_A2B10:
        r, g, b = c0, c1, c2
    elif fmt in FMT_A2R10:
        b, g, r = c0, c1, c2
    else:
        raise ValueError(f"不认识的交换链格式 {fmt}")
    return (np.stack([r, g, b], axis=-1) * 255.0 / 1023.0 + 0.5).astype(np.uint8)


def cmd_convert(d):
    idx_path = os.path.join(d, "index.csv")
    if not os.path.exists(idx_path):
        print(f"{d} 里没有 index.csv (层没抓到帧?)")
        return 1
    rows = list(csv.reader(open(idx_path)))
    n = 0
    meta = []
    for r in rows:
        if len(r) < 6:
            continue
        i, w, h, fmt, tr, ns = int(r[0]), int(r[1]), int(r[2]), int(r[3]), int(r[4]), int(r[5])
        raw = os.path.join(d, f"f{i:06d}.raw")
        png = os.path.join(d, f"f{i:06d}.png")
        if os.path.exists(raw):
            buf = open(raw, "rb").read()
            if len(buf) < w * h * 4:
                print(f"{raw} 不完整 ({len(buf)} 字节)")
                continue
            Image.fromarray(raw_to_rgb(buf, w, h, fmt)).save(png, optimize=False, compress_level=6)
            os.remove(raw)
            n += 1
        meta.append({"idx": i, "w": w, "h": h, "fmt": fmt, "pre_transform": tr, "mono_ns": ns})
    json.dump(meta, open(os.path.join(d, "frames.json"), "w"), indent=1)
    print(f"convert {d}: {n} 张 raw → png, 共 {len(meta)} 条索引")
    return 0


# ---------- 性能遍 ----------

def cmd_perf(d):
    """samples.txt: 每秒一段 '---T <ts>' + SF --latency 输出 + '---BAT' + status/current/voltage/temp"""
    txt = open(os.path.join(d, "samples.txt")).read()
    blocks = txt.split("---T ")[1:]
    present = set()
    bats = []
    period = None
    for b in blocks:
        head, _, rest = b.partition("\n")
        sf, _, bat = rest.partition("---BAT")
        lines = [l.strip() for l in sf.strip().splitlines() if l.strip()]
        if lines and re.fullmatch(r"\d+", lines[0]):
            period = int(lines[0])
            for l in lines[1:]:
                p = l.split()
                if len(p) == 3:
                    act = int(p[1])
                    if 0 < act < (1 << 62):
                        present.add(act)
        bl = [l.strip() for l in bat.strip().splitlines() if l.strip()]
        if len(bl) >= 3:
            try:
                bats.append({"t": float(head.strip()), "status": bl[0], "current_ua": int(bl[1]), "voltage_uv": int(bl[2]),
                             "gpu_temp_mc": int(bl[3]) if len(bl) > 3 and bl[3].lstrip("-").isdigit() else None})
            except ValueError:
                pass
    ts = sorted(present)
    out = {"source": "dumpsys SurfaceFlinger --latency (actualPresentTime, 每秒取一次最近 128 帧, 去重拼接)",
           "refresh_period_ns": period, "frames_seen": len(ts)}
    if len(ts) > 10:
        dt = np.diff(np.array(ts, dtype=np.float64)) / 1e6
        # 拼接缝隙 (两次采样之间漏掉的帧) 会造成大间隔; >250ms 的当作缺口不计入
        ok = dt[dt < 250]
        out.update({"fps_mean": round(1000.0 / ok.mean(), 2), "frame_ms_p50": round(float(np.percentile(ok, 50)), 3),
                    "frame_ms_p95": round(float(np.percentile(ok, 95)), 3), "gaps_dropped": int((dt >= 250).sum()),
                    "span_s": round((ts[-1] - ts[0]) / 1e9, 2)})
    else:
        out.update({"fps_mean": None, "why_null": "SurfaceFlinger 没给出该图层的上屏时间"})
    on_batt = bats and all(not b["status"].lower().startswith("charging") and b["current_ua"] < 0 for b in bats)
    if bats and on_batt:
        p = [-b["current_ua"] * b["voltage_uv"] / 1e12 for b in bats]
        out["power_w"] = round(float(np.mean(p)), 3)
    else:
        st = sorted({b["status"] for b in bats})
        out["power_w"] = None
        out["power_why_null"] = (f"采样期间电池不在放电态 (status={st}), 没有停充 —— 读数是充电电流与整机耗电相抵后的余量, 不是功耗"
                                 if bats else "没有电池采样")
    out["battery_samples"] = len(bats)
    out["battery_status_seen"] = sorted({b["status"] for b in bats})
    temps = [b["gpu_temp_mc"] for b in bats if b.get("gpu_temp_mc")]
    if temps:
        out["gpu_temp_c"] = [round(min(temps) / 1000, 1), round(max(temps) / 1000, 1)]
    mo = os.path.join(d, "megacity_out.json")
    if os.path.exists(mo):
        try:
            m = json.load(open(mo))
            out["megacity"] = {"clean_exit": m.get("clean_exit"), "frames_rendered": m.get("frames_rendered"),
                               "render_scale": m.get("graphics", {}).get("render_scale"),
                               "screen": [m.get("graphics", {}).get("width"), m.get("graphics", {}).get("height")],
                               "streaming_ready": m.get("streaming", {}).get("ready")}
        except Exception as e:  # noqa
            out["megacity"] = {"error": str(e)}
    json.dump(out, open(os.path.join(d, "perf.json"), "w"), indent=1, ensure_ascii=False)
    print(json.dumps(out, ensure_ascii=False))
    return 0


# ---------- 成对画质 ----------

def load(d, i):
    p = os.path.join(d, "framecap", f"f{i:06d}.png")
    return np.asarray(Image.open(p).convert("RGB"), dtype=np.float64) if os.path.exists(p) else None


def frame_ids(d):
    fd = os.path.join(d, "framecap")
    return sorted(int(f[1:7]) for f in os.listdir(fd) if f.endswith(".png")) if os.path.isdir(fd) else []


def psnr_mad(a, b):
    diff = a - b
    mse = float((diff ** 2).mean())
    mad = float(np.abs(diff).mean())
    return (99.0 if mse == 0 else 10 * math.log10(255.0 ** 2 / mse)), mad


def down4(a):
    h, w = a.shape[0] // 4 * 4, a.shape[1] // 4 * 4
    return a[:h, :w].reshape(h // 4, 4, w // 4, 4, 3).mean(axis=(1, 3))


def align_check(da, db, ids_a, ids_b, every=600, r=2):
    res = []
    for k in sorted(i for i in ids_a if i % every == 0):
        cand = [k + dlt for dlt in range(-r, r + 1) if k + dlt in ids_b]
        if len(cand) < 2:
            continue
        a = down4(load(da, k))
        mads = {}
        for c in cand:
            mads[c - k] = round(float(np.abs(a - down4(load(db, c))).mean()), 3)
        best = min(mads, key=mads.get)
        res.append({"k": k, "best_delta": best, "mad_by_delta": mads})
    return res


def svg_curve(rows, path, floor_rows=None):
    W, H, L, R, T, B = 900, 360, 60, 20, 30, 50
    xs = [r["idx"] for r in rows]
    if not xs:
        return
    ys = [r["psnr_db"] for r in rows]
    allys = ys + ([r["psnr_db"] for r in floor_rows] if floor_rows else [])
    y0, y1 = math.floor(min(allys) - 1), math.ceil(max(allys) + 1)
    x0, x1 = 0, max(xs)
    fx = lambda x: L + (x - x0) / max(1, x1 - x0) * (W - L - R)
    fy = lambda y: T + (y1 - y) / max(1e-9, y1 - y0) * (H - T - B)
    def poly(rr, color, label, dy):
        pts = " ".join(f"{fx(r['idx']):.1f},{fy(r['psnr_db']):.1f}" for r in rr)
        return (f'<polyline fill="none" stroke="{color}" stroke-width="2" points="{pts}"/>'
                f'<text x="{W - R - 330}" y="{T + dy}" fill="{color}" font-size="13">{label}</text>')
    g = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="sans-serif">',
         f'<rect width="{W}" height="{H}" fill="white"/>']
    for k in range(6):
        y = y0 + (y1 - y0) * k / 5
        g.append(f'<line x1="{L}" x2="{W - R}" y1="{fy(y):.1f}" y2="{fy(y):.1f}" stroke="#ddd"/>'
                 f'<text x="{L - 8}" y="{fy(y) + 4:.1f}" text-anchor="end" font-size="11">{y:.1f}</text>')
    for k in range(7):
        x = x0 + (x1 - x0) * k / 6
        g.append(f'<text x="{fx(x):.1f}" y="{H - B + 18}" text-anchor="middle" font-size="11">{int(x)}</text>')
    g.append(f'<text x="{(L + W - R) / 2}" y="{H - 12}" text-anchor="middle" font-size="12">帧序号 (路线进度)</text>')
    g.append(f'<text x="14" y="{(T + H - B) / 2}" font-size="12" transform="rotate(-90 14 {(T + H - B) / 2})" text-anchor="middle">PSNR dB</text>')
    g.append(poly(rows, "#2a6fdb", "低清+放大 vs 原生高清", 0))
    if floor_rows:
        g.append(poly(floor_rows, "#999", "原生高清 两遍之间 (本底)", 18))
    g.append("</svg>")
    open(path, "w").write("\n".join(g))


def cmd_pairs(da, db, out, floor=None):
    os.makedirs(out, exist_ok=True)
    ia, ib = set(frame_ids(da)), set(frame_ids(db))
    common = sorted(i for i in ia & ib if i % 60 == 0)
    rows = []
    for i in common:
        a, b = load(da, i), load(db, i)
        if a.shape != b.shape:
            rows.append({"idx": i, "error": f"尺寸不同 {a.shape} vs {b.shape}"})
            continue
        p, m = psnr_mad(a, b)
        rows.append({"idx": i, "psnr_db": round(p, 3), "mad": round(m, 3)})
    floor_rows = None
    if floor:
        ic = set(frame_ids(floor))
        floor_rows = []
        for i in sorted(i for i in ib & ic if i % 60 == 0):
            p, m = psnr_mad(load(db, i), load(floor, i))
            floor_rows.append({"idx": i, "psnr_db": round(p, 3), "mad": round(m, 3)})
    good = [r for r in rows if "psnr_db" in r]
    al = align_check(da, db, ia, ib)
    al_floor = align_check(floor, db, set(frame_ids(floor)), ib) if floor else None

    def stats(rr):
        if not rr:
            return None
        ps = np.array([r["psnr_db"] for r in rr]); ms = np.array([r["mad"] for r in rr])
        return {"n": len(rr), "psnr_db_mean": round(float(ps.mean()), 3), "psnr_db_median": round(float(np.median(ps)), 3),
                "psnr_db_min": round(float(ps.min()), 3), "psnr_db_max": round(float(ps.max()), 3),
                "mad_mean": round(float(ms.mean()), 3), "mad_median": round(float(np.median(ms)), 3),
                "mad_max": round(float(ms.max()), 3)}

    summary = {
        "lr_dir": da, "hr_dir": db, "floor_dir": floor,
        "metric": "PSNR=10log10(255^2/MSE), MSE/MAD 取 RGB 三通道 8bit 全部像素平均; 两图都是交换链最终画面, 同尺寸",
        "lr_vs_hr": stats(good), "hr_vs_hr_floor": stats(floor_rows),
        "alignment": {"method": "每 600 帧整倍数 k: 低清第 k 帧 vs 高清 k+δ (δ=-2..2), 1/4 缩图 MAD 最小的 δ",
                      "checks": al, "deltas": [c["best_delta"] for c in al],
                      "max_abs_delta": max((abs(c["best_delta"]) for c in al), default=None)},
        "alignment_floor": ({"checks": al_floor, "deltas": [c["best_delta"] for c in al_floor]} if al_floor else None),
        "pairs": rows, "floor_pairs": floor_rows,
    }
    json.dump(summary, open(os.path.join(out, "pairs.json"), "w"), indent=1, ensure_ascii=False)
    with open(os.path.join(out, "pairs.csv"), "w") as f:
        f.write("idx,psnr_db,mad,floor_psnr_db,floor_mad\n")
        fl = {r["idx"]: r for r in (floor_rows or [])}
        for r in good:
            q = fl.get(r["idx"], {})
            f.write(f"{r['idx']},{r['psnr_db']},{r['mad']},{q.get('psnr_db', '')},{q.get('mad', '')}\n")
    svg_curve(good, os.path.join(out, "curve.svg"), floor_rows)
    print(json.dumps({k: summary[k] for k in ("lr_vs_hr", "hr_vs_hr_floor")}, ensure_ascii=False))
    print("alignment deltas:", summary["alignment"]["deltas"], "floor:", summary["alignment_floor"] and summary["alignment_floor"]["deltas"])
    return 0


# ---------- 录屏条码与并排视频 (开发机, 需要 ffmpeg) ----------

FFMPEG = os.environ.get("FFMPEG", "ffmpeg")   # 节点上没装 ffmpeg 时指到 imageio-ffmpeg 自带的那个


def probe(path):
    """只靠 ffmpeg (节点上没有 ffprobe): 从 `ffmpeg -i` 的 stderr 里读视频流宽高"""
    e = subprocess.run([FFMPEG, "-hide_banner", "-i", path], capture_output=True, text=True).stderr
    m = re.search(r"Video: .*?(\d{2,5})x(\d{2,5})", e)
    if not m:
        raise RuntimeError(f"读不到 {path} 的视频尺寸: {e[-300:]}")
    return int(m.group(1)), int(m.group(2))


def frames(path, w, h, scale=None):
    """逐帧产出 (序号, RGB 数组)。scale=(w2,h2) 时让 ffmpeg 先缩放。"""
    vf = ["-vf", f"scale={scale[0]}:{scale[1]}:flags=area"] if scale else []
    ow, oh = scale if scale else (w, h)
    p = subprocess.Popen([FFMPEG, "-v", "error", "-i", path, *vf, "-vsync", "0", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    n = ow * oh * 3
    k = 0
    try:
        while True:
            buf = p.stdout.read(n)
            if len(buf) < n:
                break
            yield k, np.frombuffer(buf, dtype=np.uint8).reshape(oh, ow, 3)
            k += 1
    finally:   # 提前不读了 (另一边录屏先到头) 就直接收掉解码进程
        p.stdout.close()
        p.kill()
        p.wait()


def decode_stamp(img, sx, sy):
    """img: 全幅 RGB; sx/sy: 交换链像素 → 视频像素的比例。返回帧序号或 None。"""
    y = int((FC_Y0 + FC_T / 2) * sy)
    bits = []
    for t in range(FC_NT):
        x = int((FC_X0 + t * FC_T + FC_T / 2) * sx)
        r = max(1, int(FC_T * min(sx, sy) * 0.25))
        patch = img[max(0, y - r):y + r + 1, max(0, x - r):x + r + 1].astype(np.float64)
        lum = patch.mean()
        bits.append(1 if lum > 128 else 0)
    if bits[0] != 1 or bits[1] != 0 or bits[-1] != 1:
        return None
    v = 0
    par = 0
    for b in bits[2:2 + FC_BITS]:
        v = (v << 1) | b
        par ^= b
    if par != bits[2 + FC_BITS]:
        return None
    return v


def read_stamps(video, swap_w, swap_h):
    w, h = probe(video)
    # 交换链 (swap_w×swap_h) 被系统合成器等比铺满屏幕, 录屏再按屏幕尺寸出; 横屏时宽对宽
    sx, sy = w / swap_w, h / swap_h
    out = []
    for k, img in frames(video, w, h):
        out.append((k, decode_stamp(img, sx, sy)))
    return out, (w, h)


def cmd_stamp(video, out_csv, swap_w=2068, swap_h=935):
    st, (w, h) = read_stamps(video, swap_w, swap_h)
    with open(out_csv, "w") as f:
        f.write("video_frame,idx\n")
        for k, v in st:
            f.write(f"{k},{'' if v is None else v}\n")
    ok = [v for _, v in st if v is not None]
    print(f"{video}: {w}x{h} {len(st)} 帧, 读出条码 {len(ok)} 帧, 序号 {min(ok) if ok else None}..{max(ok) if ok else None}")
    return 0


def _font(size):
    from PIL import ImageFont
    for p in ["/System/Library/Fonts/PingFang.ttc", "/System/Library/Fonts/STHeiti Medium.ttc",
              "/System/Library/Fonts/Hiragino Sans GB.ttc", "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc"]:
        if os.path.exists(p):
            try:
                return ImageFont.truetype(p, size)
            except Exception:  # noqa
                pass
    return ImageFont.load_default()


def cmd_video(dlr, dhr, out_mp4, perf_lr=None, perf_hr=None, rate=30, half=True, label_lr=None, label_hr=None):
    from PIL import ImageDraw
    vl, vh = os.path.join(dlr, "screen.mp4"), os.path.join(dhr, "screen.mp4")
    sw, sh = 2068, 935
    for d in (dlr, dhr):
        mo = os.path.join(d, "megacity_out.json")
        if os.path.exists(mo):
            g = json.load(open(mo)).get("graphics", {})
            sw, sh = g.get("width", sw), g.get("height", sh)
    stl, (w, h) = read_stamps(vl, sw, sh)
    sth, _ = read_stamps(vh, sw, sh)
    for d, st in ((dlr, stl), (dhr, sth)):
        with open(os.path.join(d, "screen_stamps.csv"), "w") as f:
            f.write("video_frame,idx\n")
            f.writelines(f"{k},{'' if v is None else v}\n" for k, v in st)
    # 每个序号 → 录屏里第一次出现它的视频帧
    def first_of(st):
        m = {}
        for k, v in st:
            if v is not None and v not in m:
                m[v] = k
        return m
    ml, mh = first_of(stl), first_of(sth)
    lo = max(min(ml), min(mh))
    hi = min(max(ml), max(mh))
    # 输出时间轴: 每个输出帧推进一个帧序号 (rate 帧序号/秒); 每边取「≤ 该序号的最近一次出现」
    def pick(m, i):
        j = i
        while j >= lo and j not in m:
            j -= 1
        return j if j in m else None
    plan = []
    exact_l = exact_h = 0
    for i in range(lo, hi + 1):
        jl, jh = pick(ml, i), pick(mh, i)
        if jl is None or jh is None:
            continue
        exact_l += jl == i
        exact_h += jh == i
        plan.append((i, ml[jl], mh[jh], jl, jh))
    ow, oh = (w // 2, h // 2) if half else (w, h)
    ow -= ow % 2; oh -= oh % 2
    bar = 64
    W, H = ow * 2, oh + bar
    pl = json.load(open(perf_lr)) if perf_lr and os.path.exists(perf_lr) else {}
    ph = json.load(open(perf_hr)) if perf_hr and os.path.exists(perf_hr) else {}
    def perf_txt(p):
        f = p.get("fps_mean")
        pw = p.get("power_w")
        return f"性能遍 {f if f is not None else '无'} fps · 功耗 {('%.2f W' % pw) if pw is not None else '无数据(未停充)'}"
    ml_lab = label_lr or "左: 低清渲染 (renderScale 0.5) + 引擎放大"
    mh_lab = label_hr or "右: 原生高清 (renderScale 1.0)"
    font, small = _font(26), _font(20)
    enc = subprocess.Popen([FFMPEG, "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(rate),
                            "-i", "-", "-c:v", "libx264", "-preset", "medium", "-crf", "22", "-pix_fmt", "yuv420p",
                            "-movflags", "+faststart", out_mp4], stdin=subprocess.PIPE)
    gl = frames(vl, w, h, (ow, oh))
    gh = frames(vh, w, h, (ow, oh))
    cl = ch = (-1, None)
    for i, kl, kh, jl, jh in plan:
        while cl[0] < kl:
            cl = next(gl)
        while ch[0] < kh:
            ch = next(gh)
        canvas = np.zeros((H, W, 3), dtype=np.uint8)
        canvas[bar:, :ow] = cl[1]
        canvas[bar:, ow:] = ch[1]
        im = Image.fromarray(canvas)
        dr = ImageDraw.Draw(im)
        dr.text((12, 4), ml_lab, fill=(255, 255, 255), font=font)
        dr.text((ow + 12, 4), mh_lab, fill=(255, 255, 255), font=font)
        dr.text((12, 36), perf_txt(pl), fill=(200, 200, 200), font=small)
        dr.text((ow + 12, 36), perf_txt(ph), fill=(200, 200, 200), font=small)
        tag = f"帧序号 {i}" + ("" if jl == i and jh == i else f" (左{jl} 右{jh})")
        tw = dr.textlength(tag, font=font)
        dr.rectangle([W / 2 - tw / 2 - 10, bar + 8, W / 2 + tw / 2 + 10, bar + 44], fill=(0, 0, 0))
        dr.text((W / 2 - tw / 2, bar + 10), tag, fill=(255, 230, 80), font=font)
        enc.stdin.write(np.asarray(im).tobytes())
    enc.stdin.close()
    enc.wait()
    gl.close()
    gh.close()
    rep = {"out": out_mp4, "frames": len(plan), "idx_range": [lo, hi], "rate_idx_per_s": rate,
           "exact_match_lr": round(exact_l / max(1, len(plan)), 4), "exact_match_hr": round(exact_h / max(1, len(plan)), 4),
           "stamped_frames_lr": len(ml), "stamped_frames_hr": len(mh), "video_size": [W, H]}
    json.dump(rep, open(out_mp4 + ".json", "w"), indent=1, ensure_ascii=False)
    print(json.dumps(rep, ensure_ascii=False))
    return 0


def main(a):
    if not a:
        print(__doc__)
        return 2
    c = a[0]
    if c == "convert":
        return cmd_convert(a[1])
    if c == "perf":
        return cmd_perf(a[1])
    if c == "pairs":
        floor = a[a.index("--floor") + 1] if "--floor" in a else None
        return cmd_pairs(a[1], a[2], a[3], floor)
    if c == "stamp":
        sw = int(a[a.index("--swap-w") + 1]) if "--swap-w" in a else 2068
        sh = int(a[a.index("--swap-h") + 1]) if "--swap-h" in a else 935
        return cmd_stamp(a[1], a[2], sw, sh)
    if c == "video":
        kw = {}
        for k in ("perf-lr", "perf-hr", "rate", "label-lr", "label-hr"):
            if f"--{k}" in a:
                kw[k.replace("-", "_")] = a[a.index(f"--{k}") + 1]
        if "rate" in kw:
            kw["rate"] = int(kw["rate"])
        return cmd_video(a[1], a[2], a[3], **kw)
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
