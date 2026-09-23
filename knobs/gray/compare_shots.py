#!/usr/bin/env python3
# compare_shots.py — copyprobe 验收 ②: 截帧逐像素比对
#
# 用法: compare_shots.py <run目录>   (目录下有 <臂>/shot{1,2,3}.raw, adb exec-out screencap 的原始 RGBA)
#
# 判读逻辑: 场景永远有微小动画 (待机动作/草摆动), 所以"逐像素不变"的口径是
#   probe vs none 的差异 ≈ none vs none 的差异 (控制对基线)
# 而不是字面 0。差异显著大于控制对才算探针改了画面。
# UID 区域 (右下角) 与屏幕边缘一律排除。
import json
import struct
import sys
from pathlib import Path

THRESH = 8  # 任一通道差 > 8 记为"不同像素"


def load_raw(p: Path):
    d = p.read_bytes()
    w, h, fmt = struct.unpack("<III", d[:12])
    assert fmt == 1, f"{p}: 不是 RGBA_8888 (fmt={fmt})"
    px = d[12:12 + w * h * 4]
    assert len(px) == w * h * 4, f"{p}: 长度不符"
    return w, h, px


def diff(a: bytes, b: bytes, x0: int, y0: int, x1: int, y1: int, w: int,
         excl=()):
    """[x0,y0,x1,y1) 区域内差异像素占比; excl 是要排除的子矩形列表。"""
    total = diffn = 0
    for y in range(y0, y1):
        row = y * w * 4
        for x in range(x0, x1):
            skip = False
            for (ex0, ey0, ex1, ey1) in excl:
                if ex0 <= x < ex1 and ey0 <= y < ey1:
                    skip = True
                    break
            if skip:
                continue
            o = row + x * 4
            if (abs(a[o] - b[o]) > THRESH or abs(a[o+1] - b[o+1]) > THRESH
                    or abs(a[o+2] - b[o+2]) > THRESH):
                diffn += 1
            total += 1
    return diffn / total if total else 0.0


def main():
    run = Path(sys.argv[1])
    arms = sorted(p.name for p in run.iterdir() if p.is_dir() and (p / "shot2.raw").exists())
    if len(arms) < 2:
        sys.exit(f"{run} 里可用臂不足: {arms}")
    imgs = {}
    for a in arms:
        w, h, px = load_raw(run / a / "shot2.raw")
        imgs[a] = px
    print(f"臂: {arms}  分辨率: {w}x{h}")

    # 区域 (按 3200x1440 全屏; UID 在右下角约 (2700,1560)@(430,80) —— 按比例换算)
    sx, sy = w / 3200.0, h / 1440.0
    def R(a, b, c, d):
        return (int(a*sx), int(b*sy), int(c*sx), int(d*sy))
    uid = R(2600, 1330, 3200, 1440)
    regions = {
        "全屏(除UID)": (0, 0, w, h),
        "左上小地图": R(60, 60, 500, 330),
        "右上图标条": R(2300, 30, 3180, 140),
        "右下技能区": R(2100, 700, 3180, 1330),
        "中央3D场景": R(800, 400, 2400, 1000),
    }
    excl_full = [uid]

    none = [a for a in arms if a.startswith("none")]
    probe = [a for a in arms if a.startswith("probe")]
    report = {}
    pairs = []
    if len(none) >= 2:
        pairs.append(("控制对 none1~none2", none[0], none[1]))
    for p in probe:
        for n in none[:1]:
            pairs.append((f"{p}~{n}", p, n))
    for label, a, b in pairs:
        row = {}
        for rname, (x0, y0, x1, y1) in regions.items():
            row[rname] = round(diff(imgs[a], imgs[b], x0, y0, x1, y1, w,
                                    excl_full if rname.startswith("全屏") else ()) * 100, 3)
        report[label] = row
        print(f"{label}: " + "  ".join(f"{k}={v}%" for k, v in row.items()))
    (run / "shot_diff.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(f"落盘: {run/'shot_diff.json'}")


if __name__ == "__main__":
    main()
