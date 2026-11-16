#!/usr/bin/env python3
"""离线自测: 合成 raw 帧 / 合成带条码的录屏, 走一遍 convert → pairs → stamp → video。
python3 loop_v1/carriers/megacity/paired/test_framecap_tools.py"""
import json, os, shutil, subprocess, sys, tempfile
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import framecap_tools as ft

W, H = 256, 116


def scene(i, w=W, h=H):
    """位姿只由帧序号决定的「场景」: 平移的条纹 + 斜坡"""
    y, x = np.mgrid[0:h, 0:w]
    v = 128 + 60 * np.sin((x + 3 * i) / 9.0) + 40 * np.cos(y / 7.0)
    return np.clip(np.stack([v, v * 0.8, 255 - v], -1), 0, 255).astype(np.uint8)


def lowres(img):
    s = img[::2, ::2].astype(np.float64)
    return np.repeat(np.repeat(s, 2, 0), 2, 1)[:img.shape[0], :img.shape[1]].astype(np.uint8)


def write_cap(d, ids, lr=False, fmt=44, shift=0):
    fc = os.path.join(d, "framecap"); os.makedirs(fc)
    with open(os.path.join(fc, "index.csv"), "w") as f:
        for i in ids:
            img = scene(i + shift)
            if lr:
                img = lowres(img)
            rgba = np.dstack([img, np.full(img.shape[:2], 255, np.uint8)])
            if fmt == 44:
                rgba = rgba[:, :, [2, 1, 0, 3]]
            open(os.path.join(fc, f"f{i:06d}.raw"), "wb").write(np.ascontiguousarray(rgba).tobytes())
            f.write(f"{i},{W},{H},{fmt},1,{i * 1000}\n")


def stamp(img, idx, s):
    out = img.copy()
    bits = [1, 0] + [(idx >> b) & 1 for b in range(15, -1, -1)]
    par = 0
    for b in bits[2:]:
        par ^= b
    bits += [par, 1]
    for t, b in enumerate(bits):
        x0, y0 = int((16 + t * 24) * s), int(16 * s)
        out[y0:y0 + int(24 * s), x0:x0 + int(24 * s)] = 255 if b else 0
    return out


def make_video(path, idxs, s=1.3, lr=False):
    w, h = int(2068 * s) // 2 * 2, int(935 * s) // 2 * 2
    p = subprocess.Popen([ft.FFMPEG, "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{w}x{h}", "-r", "60", "-i", "-",
                          "-c:v", "libx264", "-crf", "28", "-pix_fmt", "yuv420p", path], stdin=subprocess.PIPE)
    for i in idxs:
        base = np.kron(scene(i, 64, 29), np.ones((h // 29 + 1, w // 64 + 1, 1), np.uint8))[:h, :w]
        p.stdin.write(stamp(base, i, s).tobytes())
    p.stdin.close(); p.wait()


def main():
    t = tempfile.mkdtemp()
    try:
        ids = sorted(set(range(0, 1801, 60)) | {598, 599, 600, 601, 602, 1198, 1199, 1200, 1201, 1202, 1, 2})
        a, b, c = (os.path.join(t, n) for n in ("lr", "hr", "hr2"))
        write_cap(a, ids, lr=True); write_cap(b, ids, fmt=37); write_cap(c, ids, shift=0)
        for d in (a, b, c):
            assert ft.cmd_convert(os.path.join(d, "framecap")) == 0
            assert not [f for f in os.listdir(os.path.join(d, "framecap")) if f.endswith(".raw")]
        # 通道顺序: BGRA 与 RGBA 换算后应一致
        x = ft.load(b, 600); y = ft.load(c, 600)
        assert np.array_equal(x, y), "BGRA/RGBA 换算不一致"
        out = os.path.join(t, "pairs")
        ft.cmd_pairs(a, b, out, floor=c)
        s = json.load(open(os.path.join(out, "pairs.json")))
        assert s["alignment"]["deltas"] and all(d == 0 for d in s["alignment"]["deltas"]), s["alignment"]
        assert s["hr_vs_hr_floor"]["psnr_db_mean"] == 99.0
        assert 15 < s["lr_vs_hr"]["psnr_db_mean"] < 60, s["lr_vs_hr"]
        # 故意错一帧: 对齐检查应报 δ=+1
        d2 = os.path.join(t, "hr_shift"); write_cap(d2, ids, shift=-1); ft.cmd_convert(os.path.join(d2, "framecap"))
        s2 = ft.align_check(c, d2, set(ft.frame_ids(c)), set(ft.frame_ids(d2)))
        assert all(r["best_delta"] == 1 for r in s2 if r["k"] > 0), s2
        assert os.path.exists(os.path.join(out, "curve.svg"))
        # 录屏条码 + 并排视频
        if shutil.which(ft.FFMPEG):
            for d, seq in ((a, [i for i in range(100, 220) for _ in (0, 1)]), (b, list(range(105, 230)))):
                make_video(os.path.join(d, "screen.mp4"), seq)
            st, _ = ft.read_stamps(os.path.join(b, "screen.mp4"), 2068, 935)
            got = [v for _, v in st]
            assert got == list(range(105, 230)), got[:10]
            om = os.path.join(t, "sbs.mp4")
            ft.cmd_video(a, b, om, rate=30)
            r = json.load(open(om + ".json"))
            assert r["idx_range"] == [105, 219] and r["exact_match_lr"] == 1.0 and r["exact_match_hr"] == 1.0, r
        else:
            print("没有 ffmpeg, 跳过视频部分")
        print("OK")
    finally:
        shutil.rmtree(t)


if __name__ == "__main__":
    main()
