#!/usr/bin/env python3
"""crosscheck.py <run.log> <summary.json> — 内核侧与应用侧帧率的交叉校验 (纯函数)

为什么要有这道校验
------------------
2026-09-23 本机实测: 应用启动后窗口约 9.6s 才被系统真正合成。在那之前 present 完全
不被节流, 样例自己数出 2080 fps, 而 kgsl 里一条 GPU 提交都没有 —— 也就是"日志很好看,
其实什么都没画"。只靠"等一会儿再开采"挡不住这种轮, 因为等多久是设备/系统版本相关的。

所以这里用两个**来源完全独立**的量互相对账:
  · trace_fps  : 内核 kgsl tracepoint 的提交节奏 (parse_trace.py 算出的 fps_mean)
  · log_fps    : 样例自己的帧计数器 (fps_logger 每 0.5s 落一行)
两者本该指向同一件事。差一个数量级 = 这一轮的采集窗没落在真实渲染上, 当场判无效。

输出 JSON: {"log_fps_median", "trace_fps_mean", "rel_diff", "n_fps_samples"}
rel_diff = |trace - log| / log; 无法计算时为 null。
"""
from __future__ import annotations
import json
import re
import statistics
import sys

FPS_RE = re.compile(r"FPS: ([\d.]+)")


def crosscheck(log_text: str, summary: dict) -> dict:
    fps = [float(x) for x in FPS_RE.findall(log_text)]
    trace_fps = summary.get("fps_mean")
    log_fps = statistics.median(fps) if fps else None
    rel = None
    if log_fps and trace_fps is not None:
        rel = abs(trace_fps - log_fps) / log_fps
    return {
        "log_fps_median": log_fps,
        "trace_fps_mean": trace_fps,
        "rel_diff": round(rel, 4) if rel is not None else None,
        "n_fps_samples": len(fps),
    }


def main() -> int:
    if len(sys.argv) < 3:
        print("用法: crosscheck.py <run.log> <summary.json>", file=sys.stderr)
        return 2
    with open(sys.argv[1], encoding="utf-8", errors="replace") as fh:
        log_text = fh.read()
    with open(sys.argv[2], encoding="utf-8") as fh:
        summary = json.load(fh)
    print(json.dumps(crosscheck(log_text, summary), ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
