#!/usr/bin/env python3
"""ready.py <run.log> — 判断样例是否已经进入"真实渲染"稳态 (纯函数, 不碰设备)

退出码: 0=已就绪  1=还没就绪  2=用法错

背景 (2026-09-23 本机实测)
--------------------------
应用启动后窗口要约 9.6s 才被系统真正合成。在那之前 present 不被节流, 样例自己数出
约 2080 fps, 而 kgsl 里一条 GPU 提交都没有。之后帧率一步跌到约 340 fps 才是真的在画。

所以"等够多少秒"不是个稳定的判据(换机器/换系统就变), 帧率序列里的那一**级阶跃**才是。
判据两条同时成立:
  1. 出现过阶跃下降: max(全部样本) / median(最后 3 个) >= STEP (缺省 1.8)
  2. 已经稳下来:     最后 3 个样本的 (max-min)/median <= FLAT (缺省 0.15)

从没空转过的样例 (例如开着 vsync 跑的 hello_triangle) 永远不满足第 1 条, 所以调用方
必须给这个轮询一个上界, 超时就照常往下走 —— 采完还有 crosscheck.py 那道数据侧兜底。
"""
from __future__ import annotations
import re
import sys
from statistics import median

FPS_RE = re.compile(r"FPS: ([\d.]+)")
STEP = 1.8
FLAT = 0.15
MIN_SAMPLES = 5


def ready(fps: list[float]) -> bool:
    if len(fps) < MIN_SAMPLES:
        return False
    tail = fps[-3:]
    m = median(tail)
    if m <= 0:
        return False
    if (max(tail) - min(tail)) / m > FLAT:
        return False
    return max(fps) / m >= STEP


def main() -> int:
    if len(sys.argv) < 2:
        print("用法: ready.py <run.log>", file=sys.stderr)
        return 2
    try:
        with open(sys.argv[1], encoding="utf-8", errors="replace") as fh:
            fps = [float(x) for x in FPS_RE.findall(fh.read())]
    except FileNotFoundError:
        return 1
    return 0 if ready(fps) else 1


if __name__ == "__main__":
    raise SystemExit(main())
