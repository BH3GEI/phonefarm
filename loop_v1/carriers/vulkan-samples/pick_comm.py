#!/usr/bin/env python3
"""pick_comm.py — 从一段 ftrace 文本里挑出发 GPU 提交的那个线程名 (纯函数, 无副作用)。

为什么需要它
------------
refbench 是我们自己的靶子, 可以把提交线程改名成稳定契约 (`RefbenchDrv`), 所以
`phonefarm parse-trace --comm RefbenchDrv` 直接写死就行。Vulkan-Samples 是上游第三方工程,
提交线程名由 GameActivity / Adreno 驱动决定, 不同样例、不同 Android 版本都可能不一样,
写死等于埋雷。

所以这里不猜: 数一遍 `adreno_cmdbatch_submitted` 事件按 comm 的分布, 取最多的那个,
并把完整分布一起打出来 —— 如果第一名不是压倒性的 (比如占比 < 80%), 说明这个负载的
提交是多线程发的, 调用方应当停下来看清楚, 而不是默默按第一名解析。

输出 JSON: {"comm": <第一名>, "share": <占比>, "dist": {comm: n, ...}}
"""
from __future__ import annotations
import json
import re
import sys
from collections import Counter

LINE_RE = re.compile(
    r"^\s*(?P<comm>.+?)-(?P<tid>\d+)\s+\[(?P<cpu>\d+)\]\s+\S+\s+(?P<ts>\d+\.\d+):\s+(?P<event>\w+):"
)


def pick(text: str, event: str = "adreno_cmdbatch_submitted") -> dict:
    counts: Counter[str] = Counter()
    for line in text.splitlines():
        m = LINE_RE.match(line)
        if m and m.group("event") == event:
            counts[m.group("comm").strip()] += 1
    total = sum(counts.values())
    if total == 0:
        return {"comm": None, "share": 0.0, "dist": {}, "total": 0}
    comm, n = counts.most_common(1)[0]
    return {
        "comm": comm,
        "share": round(n / total, 4),
        "dist": dict(counts.most_common(8)),
        "total": total,
    }


def main() -> int:
    if len(sys.argv) < 2:
        print("用法: pick_comm.py <trace.txt>", file=sys.stderr)
        return 2
    with open(sys.argv[1], encoding="utf-8", errors="replace") as fh:
        out = pick(fh.read())
    print(json.dumps(out, ensure_ascii=False, indent=2))
    return 0 if out["comm"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
