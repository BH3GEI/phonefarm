#!/usr/bin/env python3
"""crosscheck.py <run.log> <summary.json> [window.json] — 内核侧与应用侧的对账 (纯函数)

为什么要有这道校验
------------------
2026-09-23 本机实测: 样例的窗口不是一启动就被系统合成的，而且合成状态在一次运行里会
来回切。没被合成的那段, present 完全不被节流, 应用自己数出两千多 fps, 而 kgsl 侧的
提交速率对不上 —— 也就是"日志很好看, 其实画的不是那回事"。
只靠"等一会儿再开采"挡不住, 因为等多久是设备/系统版本相关的。

所以用两个**来源完全独立**的量互相对账:
  · 内核侧: kgsl tracepoint 的**提交速率**
  · 应用侧: 样例自己的帧计数器 (fps_logger 每 0.5s 落一行)

两个必须踩对的细节
------------------
1. **比提交速率, 不比 fps_mean。** phonefarm parse-trace 会自检"每帧几次提交"(spf) 再用
   提交速率/spf 得到 fps; 这个自检在本载体上实测会把 spf 认成 3, 于是 fps_mean 刚好
   是应用自报帧率的 1/3, 一轮完全正常的数据会被判成"对不上"。这里把 spf 乘回去还原成
   提交速率, 再问一个更弱也更诚实的问题: **提交速率是不是应用帧率的一个小整数倍**
   (一帧发 1~4 次命令提交都正常: 渲染 + UI + blit)。判的是**数量级**而不是整数精度:
   实测一轮完全干净的数据(应用线程占 96%、spf=1、无热事件)比值是 1.41 —— 应用侧
   fps_logger 的计数窗口和内核侧的提交节奏本来就不是同一个口径, 要求它贴近整数是苛求,
   会把好轮判掉。这道闸要拦的是"应用在数帧而 GPU 没活干"那种假数据, 那时比值会掉到 0
   附近; 所以判据是比值落在 [RATIO_MIN, RATIO_MAX] 这个带内。
2. **只取采集窗内的 FPS 样本。** trace 只有 12 秒, 而 run.log 覆盖整段运行。拿整段的
   中位数去比 12 秒的窗口, 遇上运行中途切换合成状态就会误杀好轮。window.json 里是
   run_vks.sh 记下的采集窗**设备墙钟**边界, 有它就只用窗内样本。

输出 JSON 里 in_band = 提交速率/应用帧率 是否落在 [0.5, 6.0]; 落在带内即认为两侧一致。
"""
from __future__ import annotations
import json
import re
import statistics
import sys

# 例: [2026-09-23 21:15:28.490] [logger] [info] FPS: 1922.7
LINE_RE = re.compile(r"^\[(?P<ts>\d{4}-\d\d-\d\d \d\d:\d\d:\d\d)\.\d+\].*FPS: (?P<fps>[\d.]+)")
FPS_RE = re.compile(r"FPS: ([\d.]+)")

# 提交速率 / 应用帧率 的可接受区间。下界拦"应用在数帧但 GPU 没活干";
# 上界拦"trace 里混进了别的进程的提交"。一帧 1~4 次提交都在带内。
RATIO_MIN = 0.5
RATIO_MAX = 6.0


def fps_in_window(log_text: str, window: dict | None) -> tuple[list[float], bool]:
    """返回 (采样, 是否真的按窗口过滤过)。窗口内一个样本都没有时退回全量, 并如实标注。"""
    if window and window.get("cap_start") and window.get("cap_end"):
        lo, hi = window["cap_start"], window["cap_end"]
        picked = [float(m.group("fps")) for line in log_text.splitlines()
                  if (m := LINE_RE.match(line.strip())) and lo <= m.group("ts") <= hi]
        if picked:
            return picked, True
    return [float(x) for x in FPS_RE.findall(log_text)], False


def crosscheck(log_text: str, summary: dict, window: dict | None = None) -> dict:
    fps, windowed = fps_in_window(log_text, window)
    trace_fps = summary.get("fps_mean")
    spf = summary.get("submits_per_frame") or 1
    submit_rate = trace_fps * spf if trace_fps is not None else None
    log_fps = statistics.median(fps) if fps else None

    ratio = None
    in_band = False
    if log_fps and submit_rate is not None:
        ratio = submit_rate / log_fps
        in_band = RATIO_MIN <= ratio <= RATIO_MAX
    return {
        "log_fps_median": log_fps,
        "windowed": windowed,
        "trace_fps_mean": trace_fps,
        "submits_per_frame": spf,
        "trace_submit_rate": round(submit_rate, 3) if submit_rate is not None else None,
        "ratio_to_log_fps": round(ratio, 4) if ratio is not None else None,
        "band": [RATIO_MIN, RATIO_MAX],
        "in_band": in_band,
        "n_fps_samples": len(fps),
    }


def main() -> int:
    if len(sys.argv) < 3:
        print("用法: crosscheck.py <run.log> <summary.json> [window.json]", file=sys.stderr)
        return 2
    with open(sys.argv[1], encoding="utf-8", errors="replace") as fh:
        log_text = fh.read()
    with open(sys.argv[2], encoding="utf-8") as fh:
        summary = json.load(fh)
    window = None
    if len(sys.argv) > 3:
        try:
            with open(sys.argv[3], encoding="utf-8") as fh:
                window = json.load(fh)
        except (OSError, json.JSONDecodeError):
            window = None
    print(json.dumps(crosscheck(log_text, summary, window), ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
