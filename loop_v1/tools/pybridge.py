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
    # 显式 utf-8: 结果里有中文 ("样本不足" / "<缺行>"), text=True 会跟着 locale 走,
    # LC_ALL=C 下就是 UnicodeDecodeError
    out = subprocess.run([pf_bin(), "loopstat"],
                         input=json.dumps(req, ensure_ascii=False),
                         capture_output=True, text=True, encoding="utf-8", check=True)
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


# ── 系统参数闭环 (src/sysparam.rs) ──
#
# 白名单与候选参数都按「有序 pair 列表」过桥, 不用 JSON 对象: Rust 那边的
# serde_json 一解析对象就按字典序排好了, 而 Python dict 是插入序 —— 候选参数的
# 遍历顺序决定了多处违规时先报哪一条, 不能丢。

def _wl_pairs(wl: dict) -> list:
    return [[pid, spec] for pid, spec in wl.items()]


def build_whitelist(probe_text: str) -> dict:
    """probe_sysparam.sh 的文本 → {param_id: spec}。"""
    return _call({"op": "build_whitelist", "probe_text": probe_text})


def describe_whitelist(wl: dict) -> str:
    return _call({"op": "describe_whitelist", "wl": _wl_pairs(wl)})


def validate_candidate(cand: dict, wl: dict) -> tuple:
    ok, why = _call({"op": "validate_candidate",
                     "cand": [[k, str(v)] for k, v in cand.items()],
                     "wl": _wl_pairs(wl)})
    return ok, why


def plan_text(cand: dict, wl: dict) -> str:
    return _call({"op": "plan_text",
                  "cand": [[k, str(v)] for k, v in cand.items()],
                  "wl": _wl_pairs(wl)})


def rule_doc(temp_cap_c: float, pairs: int, power_available: bool,
             power_note: str = "") -> dict:
    return _call({"op": "rule_doc", "temp_cap_c": temp_cap_c, "pairs": pairs,
                  "power_available": power_available, "power_note": power_note})


# 下面两张表由二进制那边定义 (src/sysparam.rs), 这里照抄一份只为调用方好写:
# 真正的拦截与判定都在二进制里做, 改口径要改那边, 这里跟着改。
DENY_KEYWORDS = ("thermal", "trip_point", "cooling", "fan", "tsens", "bcl", "throttl")
# (指标, 方向) —— 方向 "lower" = 越小越好
PRIMARY_METRICS = [
    ("frame_p95", "lower"),
    ("fps_mean", "higher"),
    ("power_w_mean", "lower"),
]


def spin_verdict(frame_a: str, frame_b: str) -> dict:
    """两张裸帧 → 「视角是否在转」。传路径而不是字节: 一张 13MB, base64 过桥不划算。"""
    return _call({"op": "spin_verdict", "frame_a": frame_a, "frame_b": frame_b})


def env_stats(text: str) -> dict:
    return _call({"op": "env_stats", "text": text})


def decide(comparisons: dict, *, temp_max_c, temp_cap_c: float, apply_ok: bool,
           snapshot_identical: bool, power_available: bool) -> dict:
    return _call({"op": "decide", "comparisons": comparisons,
                  "temp_max_c": temp_max_c, "temp_cap_c": temp_cap_c,
                  "apply_ok": apply_ok, "snapshot_identical": snapshot_identical,
                  "power_available": power_available})
