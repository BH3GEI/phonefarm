#!/usr/bin/env python3
"""frames_moving.py <raw1> <raw2> — 两张 adb screencap 裸帧的平均逐像素差 (%)。

为什么要它: 原神 7.1.0 上手柄注入已失效, 用手柄脚本跑出来是**静止画面** ——
而静止画面照样能采到帧率、功耗、温度, 报告看着一切正常, 只是量的根本不是
"定点转视角"那个负载。这个差异值是唯一能把"负载真的在动"钉死的证据。

裸帧格式 (adb exec-out screencap, 不带 -p): 小端 u32 宽、u32 高、u32 format,
Android 13+ 还多一个 u32 colorspace, 然后是 RGBA8888 像素。
头部长度按 "总字节数 - 宽*高*4" 反推, 不写死 —— 写死就会在换一版系统时整体错位。

零依赖 (不需要 PIL): 只按固定步长抽样求平均绝对差。
"""
import sys


def load(path):
    b = open(path, "rb").read()
    if len(b) < 16:
        raise SystemExit(f"{path}: 太短, 不像一张裸帧")
    w = int.from_bytes(b[0:4], "little")
    h = int.from_bytes(b[4:8], "little")
    if not (0 < w < 20000 and 0 < h < 20000):
        raise SystemExit(f"{path}: 读出的宽高不合理 {w}x{h}")
    head = len(b) - w * h * 4
    if head < 0:
        raise SystemExit(f"{path}: 字节数 {len(b)} 装不下 {w}x{h} 的 RGBA")
    return w, h, b[head:]


def main():
    if len(sys.argv) < 3:
        print("用法: frames_moving.py <raw1> <raw2>", file=sys.stderr)
        return 2
    w1, h1, p1 = load(sys.argv[1])
    w2, h2, p2 = load(sys.argv[2])
    if (w1, h1) != (w2, h2):
        raise SystemExit(f"两帧尺寸不同: {w1}x{h1} vs {w2}x{h2}")

    # 每隔 997 个像素取一个 (质数步长, 避开与屏幕宽度成整除关系导致只采到某几列)
    step = 997 * 4
    total = 0
    n = 0
    for i in range(0, min(len(p1), len(p2)) - 4, step):
        for c in range(3):  # 只看 RGB, 跳过 alpha
            total += abs(p1[i + c] - p2[i + c])
        n += 3
    if n == 0:
        raise SystemExit("没采到任何像素")
    pct = total / n / 255.0 * 100.0
    print(f"{pct:.3f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
