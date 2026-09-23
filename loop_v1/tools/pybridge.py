#!/usr/bin/env python3
"""pybridge.py — 还没搬走的 Python 拿统计口径的唯一通道 (迁移期专用)

离散度、漂移、精确置换检验、置换反演 CI、快照比对这些已经搬进 phonefarm 二进制
(`src/loopstat.rs` / `src/loopreport.rs`)。`refbench_report.py` 与 `auto/autoloop.py`
还在把它们当库用, 于是统一从这里转调。

**不要在这个文件里重新实现任何统计量。** 口径只能有一份: 两份实现哪怕只差一个舍入位,
report.json 的字节就对不上了, 判据 5 的回放自检会直接把它报成失败。

等 refbench_report 与 auto/ 各自搬完, 这个文件连同 `phonefarm loopstat` 子命令一起删掉。
"""
from __future__ import annotations
import json
import os
import subprocess

_HERE = os.path.dirname(os.path.abspath(__file__))
_ROOT = os.path.dirname(os.path.dirname(_HERE))


def pf_bin() -> str:
    """phonefarm 二进制路径, 与 pf_bin.sh 同一套顺序 (PF_BIN > 仓库根 > cargo 产物)。"""
    override = os.environ.get("PF_BIN")
    if override:
        if not os.access(override, os.X_OK):
            raise SystemExit(f"PF_BIN 指向的不是可执行文件: {override}")
        return override
    for c in (os.path.join(_ROOT, "phonefarm"),
              os.path.join(_ROOT, "src/target/release/phonefarm"),
              os.path.join(_ROOT, "src/target/debug/phonefarm")):
        if os.access(c, os.X_OK):
            return c
    raise SystemExit(
        f"找不到 phonefarm 二进制: 先 (cd {_ROOT}/src && cargo build --release), 或设 PF_BIN=<路径>")


def _call(req: dict):
    out = subprocess.run([pf_bin(), "loopstat"],
                         input=json.dumps(req, ensure_ascii=False),
                         capture_output=True, text=True, check=True)
    return json.loads(out.stdout)


def mean(xs: list) -> float:
    return _call({"op": "mean", "xs": xs})


def dispersion(xs: list):
    return _call({"op": "dispersion", "xs": xs})


def drift(xs: list) -> dict:
    return _call({"op": "drift", "xs": xs})


def load_runs(pattern: str) -> list:
    """返回 [(标签, summary 字典), ...] —— 与旧 analyze.load_runs 同形。"""
    return [(label, summary) for label, summary in _call({"op": "load_runs", "pattern": pattern})]


def describe(runs: list, title: str) -> dict:
    return _call({"op": "describe", "runs": [list(r) for r in runs], "title": title})


def compare(a_runs: list, b_runs: list, metric: str) -> dict:
    return _call({"op": "compare",
                  "a": [list(r) for r in a_runs],
                  "b": [list(r) for r in b_runs],
                  "metric": metric})


def snapshot_diff(before: str, after: str) -> dict:
    return _call({"op": "snapshot_diff", "before": before, "after": after})
