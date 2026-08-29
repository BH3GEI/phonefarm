#!/usr/bin/env python3
"""从一局的 log.jsonl 抽一行汇总: stop<TAB>achieved<TAB>steps<TAB>calls"""
import json, sys

stop, achieved, steps, calls = "done", "-", 0, 0
for line in open(sys.argv[1], encoding="utf-8"):
    try:
        v = json.loads(line)
    except Exception:
        continue
    if v.get("r") == "act":
        steps = max(steps, v.get("n", 0))
        if "ms" in v:  # 计划首动作才带 ms → 一次决策调用
            calls += 1
    elif v.get("r") == "hook":
        if v.get("kind") == "budget":
            stop = v.get("stop", "?")
        elif v.get("kind") == "verdict":
            achieved = "true" if v.get("achieved") else "false"
print(f"{stop}\t{achieved}\t{steps}\t{calls}")
