# n3_judge.py — 三臂实验的离线判定。纯离线, 只读 <out>/ 下的原始数据, 可以用别的 phonefarm 二进制重跑。
#
#   python3 n3_judge.py <out目录> [--pf <phonefarm>] [--perf-first]
#
# 统计口径与 phonefarm eval 的 judge_runs 相同 (不另造一套):
#   - 每个指标用 `phonefarm loopstat` 的 compare (精确置换, 双侧 p, 置换反演 95% CI);
#   - 判定指标表 = frame_p95 (小好)、fps_mean (大好)、功耗进得了表时加 power_w_mean (小好);
#     改善门槛 alpha_win = 0.05 / 表长 (Bonferroni), 变差门槛 alpha = 0.05;
#   - 样本量防护 (stat-guards): 每个指标的 n 在精确置换下最小可达 p 必须 < alpha_win, 否则这个指标记「判不出」。
#     最小可达 p 不在这里算公式, 而是把 n 对 n 完全分开的数据交给同一个 compare 算出来 (= 2/C(2n,n))。
#   - 功耗只认电池轨 (supply_mode = battery, 停充放电态), 一臂不合格只剔除那一对 (fix-D 的按对剔除)。
#   - 一臂失效 (设置没生效 / NPU 没在跑 / 帧口径不对 / 起跑温度超) → 只剔除这一块里涉及它的那一对。
# 帧时: 目标进程 ftrace (frame-source 修复), summary 里必须有 frame_attribution=target_tids。
# 功耗: sample_env v2 + env_stats_window, 按 trace 采集窗截取并丢第一个点 (fix-C)。
import argparse
import glob
import json
import os
import subprocess
import sys

ALPHA = 0.05
HERE = os.path.dirname(os.path.abspath(__file__))


def run_pf(pf, args, stdin=None, timeout=600):
    p = subprocess.run([pf] + args, input=stdin.encode() if stdin else None,
                       capture_output=True, timeout=timeout)
    return p.returncode, p.stdout.decode("utf-8", "replace"), p.stderr.decode("utf-8", "replace")


def loopstat(pf, req):
    rc, o, e = run_pf(pf, ["loopstat"], json.dumps(req, ensure_ascii=False))
    if rc != 0:
        raise RuntimeError(f"loopstat {req.get('op')} 失败: {e.strip()[:300]}")
    return json.loads(o)


def compare(pf, a, b, metric):
    """a、b: [(标签, 值)]。回 loopstat compare 的原样结果。"""
    req = {"op": "compare", "metric": metric,
           "a": [[l, {metric: v}] for l, v in a], "b": [[l, {metric: v}] for l, v in b]}
    return loopstat(pf, req)


def min_reachable_p(pf, n):
    """n 对 n 完全分开时 compare 给出的 p, 即这个样本量下精确置换检验最小能出多小的 p。"""
    if n < 2:
        return 1.0
    a = [(f"a{i}", float(i)) for i in range(n)]
    b = [(f"b{i}", float(100 + i)) for i in range(n)]
    return compare(pf, a, b, "x")["perm_p_two_sided"]


def load(path, default=None):
    try:
        return json.load(open(path, encoding="utf-8"))
    except (OSError, ValueError):
        return default


# ───────────────────────── 逐臂重算 ─────────────────────────

def analyze_arm(pf, d, cfg):
    """从原始 trace.txt / env.txt 重算这一臂的帧时与功耗, 写 summary.json / env.json, 回逐臂判据。"""
    arm = load(os.path.join(d, "arm.json"), {})
    res = {"label": arm.get("label", os.path.basename(d)), "block": arm.get("block"), "arm": arm.get("arm"),
           "problems": list(arm.get("invalid_reason", []))}
    if not arm.get("collected"):
        res["problems"].append("没采完")
    tr = os.path.join(d, "trace.txt")
    summ = None
    if os.path.exists(tr):
        rc, o, e = run_pf(pf, ["parse-trace", tr, "--comm", cfg["frame"]["render_comm"]])
        if rc == 0:
            summ = json.loads(o)
            open(os.path.join(d, "summary.json"), "w").write(o)
    if not summ:
        res["problems"].append("trace 解析失败")
    else:
        cap = cfg["frame"]["fps_cap"]
        if summ.get("frame_attribution") != "target_tids":
            res["problems"].append(f"帧口径不是目标进程 (frame_attribution={summ.get('frame_attribution')})")
        fps = summ.get("fps_mean")
        if fps is None or abs(fps - cap) > cap * cfg["frame"]["fps_tol_frac"]:
            res["problems"].append(f"fps_mean={fps} 与帧率上限 {cap} 对不上 (每帧提交数分组可能错了, 或帧率设置没生效)")
        res["frame_p95"] = summ.get("frame_p95")
        res["fps_mean"] = fps
        res["frame_p50"] = summ.get("frame_p50")
        res["submits_per_frame"] = summ.get("submits_per_frame")
        res["gpu_active_mean"] = summ.get("gpu_active_mean")
        # 超时帧占比: 帧间隔 > miss_ms (帧预算 33.3 ms + 一个 120 Hz vsync)。按帧预算判帧时用它, 不用 p95 的绝对变化
        rc, o, e = run_pf(pf, ["parse-trace", tr, "--comm", cfg["frame"]["render_comm"], "--full"])
        try:
            fm = json.loads(o).get("frame_ms") or []
        except ValueError:
            fm = []
        res["missed_frac"] = (sum(1 for x in fm if x > cfg["frame"]["miss_ms"]) / len(fm)) if fm else None
    envp = os.path.join(d, "env.txt")
    if os.path.exists(envp) and os.path.exists(tr):
        head = []
        with open(tr, encoding="utf-8", errors="replace") as f:
            for line in f:
                if not line.startswith("#"):
                    break
                head.append(line)
        env = loopstat(pf, {"op": "env_stats_window", "text": open(envp, encoding="utf-8", errors="replace").read(),
                            "trace_head": "".join(head)})
        json.dump(env, open(os.path.join(d, "env.json"), "w"), ensure_ascii=False, indent=1)
        res["supply_mode"] = env.get("supply_mode")
        res["power_w_mean"] = env.get("power_w_mean")
        res["n_power_samples_used"] = env.get("n_power_samples_used")
        res["power_sample_hz"] = env.get("power_sample_hz")
        res["soc_temp_max_c"] = env.get("soc_temp_max_c")
        res["gpu_max_pwrlevel_seen"] = env.get("gpu_max_pwrlevel_seen")
        res["gpu_clk_mhz_mean"] = env.get("gpu_clk_mhz_mean")
        res["fan_state"] = env.get("fan_state")
        ok = env.get("supply_mode") == "battery" and env.get("power_w_mean") is not None
        if ok and (env.get("n_power_samples_used") or 0) < cfg["power"]["min_samples"]:
            ok = False
            res["power_problem"] = f"窗口内功耗点只有 {env.get('n_power_samples_used')} 个"
        elif not ok:
            res["power_problem"] = f"不是电池轨: supply_mode={env.get('supply_mode')} ({env.get('power_usable_reason')})"
        res["power_ok"] = ok
    else:
        res["power_ok"] = False
        res["power_problem"] = "缺 env.txt"
    st = arm.get("start_temp") or {}
    th = cfg["thermal"]
    if st.get("batt_c") is None or st["batt_c"] > th["batt_start_c"] + th["tol_c"]:
        res["problems"].append(f"起跑电池温度 {st.get('batt_c')} °C 高于 {th['batt_start_c']}+{th['tol_c']}")
    res["start_temp"] = st
    res["npuop"] = arm.get("npuop")
    res["not_applied"] = bool(arm.get("not_applied"))
    res["setting_observed"] = arm.get("setting_observed")
    res["valid"] = not res["problems"]
    return res


# ───────────────────────── 两臂比较 (与 eval::judge_runs 同规则) ─────────────────────────

_MISS_MS = [40.0]


def budget_ms(_pairs):
    return _MISS_MS[0]


def pb_ok(pairs, blk, m):
    return all(pb.get(m) is not None for b, _, pb in pairs if b == blk)


def judge_pair(pf, arms, x, ref):
    """x 臂对 ref 臂 (ref 是对照 = loopstat 的 a 侧)。按块配对, 一块里任一臂失效只剔这一块。"""
    by = {}
    for r in arms:
        by.setdefault(r["block"], {})[r["arm"]] = r
    frame_pairs, power_pairs, dropped = [], [], []
    for blk in sorted(by):
        a, b = by[blk].get(ref), by[blk].get(x)
        if not a or not b:
            dropped.append({"block": blk, "why": "缺臂"})
            continue
        if not (a["valid"] and b["valid"]):
            dropped.append({"block": blk, "why": "; ".join(a["problems"] + b["problems"])})
            continue
        frame_pairs.append((blk, a, b))
        if a["power_ok"] and b["power_ok"]:
            power_pairs.append((blk, a, b))
        else:
            dropped.append({"block": blk, "why": "功耗不进判定: " + (a.get("power_problem") or b.get("power_problem") or "")})
    # 判定表 (占 Bonferroni 预算的): 同 judge_runs = frame_p95、fps_mean、(功耗进得了表时) power_w_mean。
    # 另加一项只守退化的 missed_frac (超时帧占比), 门槛 alpha, 不占改善预算。
    metrics = [("frame_p95", "lower", frame_pairs), ("fps_mean", "higher", frame_pairs)]
    if len(power_pairs) >= 2:
        metrics.append(("power_w_mean", "lower", power_pairs))
    alpha_win = ALPHA / len(metrics)
    metrics.append(("missed_frac", "lower", frame_pairs))
    miss_ms = None
    per, wins, regs = {}, [], []
    for m, better, pairs in metrics:
        n = len(pairs)
        a = [(f"{ref}{blk}", pa[m]) for blk, pa, _ in pairs if pa.get(m) is not None and pb_ok(pairs, blk, m)]
        b = [(f"{x}{blk}", pb[m]) for blk, pa, pb in pairs if pb.get(m) is not None and pa.get(m) is not None]
        n = min(len(a), len(b))
        if n < 2:
            per[m] = {"status": "缺数据", "n": n}
            continue
        pmin = min_reachable_p(pf, n)
        c = compare(pf, a, b, m)
        d, p = c.get("diff_mean"), c.get("perm_p_two_sided")
        improved = d is not None and ((d < 0) if better == "lower" else (d > 0))
        worse = not improved and d not in (None, 0) and p is not None and p < ALPHA
        can_win = m in ("frame_p95", "power_w_mean")
        if worse and m == "frame_p95" and (c.get("b_mean") or 0) <= budget_ms(pairs):
            status = "预算内变长, 不算退化"
        elif worse:
            status = "显著变差"
            regs.append(m)
        elif not can_win:
            status = "无显著退化"
        elif pmin >= alpha_win:
            status = "样本不足判不出改善"
        elif improved and p < alpha_win:
            status = "显著改善"
            wins.append(m)
        else:
            status = "无显著变化"
        per[m] = {"status": status, "n": n, "min_reachable_p": pmin, "a_mean": c.get("a_mean"),
                  "b_mean": c.get("b_mean"), "diff_mean": d, "diff_pct": c.get("diff_pct"), "p": p,
                  "ci95": c.get("ci95"), "hodges_lehmann": c.get("hodges_lehmann"),
                  "a_values": c.get("a_values"), "b_values": c.get("b_values")}
    # 帧率被锁在 30, fps_mean 只当退化守门, 不算「改善」(审计 #8); 但它仍占一份 Bonferroni 预算 (保守)
    gain_wins = wins
    return {"x": x, "ref": ref, "stat": "exact_permutation (phonefarm loopstat compare)",
            "alpha_win": alpha_win, "alpha_regression": ALPHA, "metrics_in_table": [m for m, _, _ in metrics],
            "n_frame_pairs": len(frame_pairs), "n_power_pairs": len(power_pairs),
            "wins": gain_wins, "regressions": regs, "per_metric": per, "dropped": dropped}


# ───────────────────────── 画质 ─────────────────────────

def iq(pf, ctrl, knob, excl, outp):
    args = ["iq-judge", "--ctrl", ",".join(ctrl), "--knob", ",".join(knob)]
    for e in excl:
        args += ["--exclude", ",".join(str(int(v)) for v in e)]
    rc, o, e = run_pf(pf, args)
    try:
        rep = json.loads(o)
    except ValueError:
        rep = {"verdict": "error", "stderr": e[-500:], "rc": rc}
    rep["_rc"] = rc
    json.dump(rep, open(outp, "w"), ensure_ascii=False, indent=1)
    return rep


def judge_images(pf, out, cfg):
    """两条来源:
    1) iq 会话 (同一次游戏会话内交错拍 C K C K C …, 见 npu3arm.py iq): 这是正式画质判据;
    2) 性能臂各自会话里拍的站定帧 (跨会话, 游戏内时辰不同): 只作参考。"""
    res = {}
    excl = load(os.path.join(out, "iq_exclude_px.json"), [])
    seq = load(os.path.join(out, "iq", "sequence.json"))
    if seq:
        for k in ("B", "A"):
            shots = [s for s in seq.get(f"seq_{k}", []) if s.get("ok")]
            ctrl = [os.path.join(out, "iq", s["file"]) for s in shots if s["arm"] == "C"]
            knob = [os.path.join(out, "iq", s["file"]) for s in shots if s["arm"] == k]
            if len(ctrl) >= 3 and knob:
                res[f"{k}_vs_C"] = iq(pf, ctrl, knob, excl, os.path.join(out, "iq", f"iq_{k}_vs_C.json"))
                # 空改动自检: 拿中间一张 C 当候选, 其余 C 当对照。应当 pass; 不 pass 说明这次采集本身不稳, 判据判不了
                if len(ctrl) >= 4:
                    mid = len(ctrl) // 2
                    res[f"null_{k}"] = iq(pf, ctrl[:mid] + ctrl[mid + 1:], [ctrl[mid]], excl,
                                          os.path.join(out, "iq", f"iq_null_{k}.json"))
            else:
                res[f"{k}_vs_C"] = {"verdict": "inconclusive", "why": f"iq 会话里 C {len(ctrl)} 张 / {k} {len(knob)} 张, 不够"}
        res["source"] = "iq_session"
        res["live_switch"] = seq.get("live_switch")
    xs = {}
    for d in sorted(glob.glob(os.path.join(out, "arms", "*"))):
        a = load(os.path.join(d, "arm.json"), {})
        for s in a.get("shots", []):
            p = os.path.join(d, s)
            if os.path.exists(p):
                xs.setdefault(a.get("arm"), []).append(p)
    cross = {}
    for k in ("B", "A"):
        if len(xs.get("C", [])) >= 3 and xs.get(k):
            cross[f"{k}_vs_C"] = iq(pf, xs["C"], xs[k], excl, os.path.join(out, f"iq_cross_session_{k}_vs_C.json"))
    res["cross_session_reference_only"] = {k: v.get("verdict") for k, v in cross.items()}
    return res


def fmt(v, nd=3):
    return "—" if v is None else (f"{v:.{nd}f}" if isinstance(v, float) else str(v))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--pf", default=os.environ.get("PF"))
    ap.add_argument("--config", default=None)
    ap.add_argument("--perf-first", action="store_true", default=None)
    a = ap.parse_args()
    out = a.out
    plan = load(os.path.join(out, "plan.json"), {})
    cfg = load(a.config) if a.config else plan.get("config")
    if not cfg:
        sys.exit("没有配置 (plan.json 里没有, 也没给 --config)")
    pf = a.pf or plan.get("pf")
    if not pf or not os.path.exists(pf):
        sys.exit(f"phonefarm 二进制不存在: {pf}")
    perf_first = plan.get("mode") == "perf-first" if a.perf_first is None else a.perf_first

    _MISS_MS[0] = cfg["frame"].get("miss_ms", 40.0)
    arms = [analyze_arm(pf, d, cfg) for d in sorted(glob.glob(os.path.join(out, "arms", "*"))) if os.path.isdir(d)]
    json.dump(arms, open(os.path.join(out, "arms_analysis.json"), "w"), ensure_ascii=False, indent=1)
    bc = judge_pair(pf, arms, "B", "C")
    ba = judge_pair(pf, arms, "B", "A")
    ac = judge_pair(pf, arms, "A", "C")
    img = judge_images(pf, out, cfg)

    need = next(n for n in range(2, 65) if min_reachable_p(pf, n) < ALPHA / 3)
    reasons = [f"运行中途停止: {plan['stopped']}"] if plan.get("stopped") else []
    not_applied = [r["label"] for r in arms if r.get("not_applied")]
    if not_applied:
        reasons.append(f"B 臂未按设计施加 {len(not_applied)} 臂 (NPU 回退到双线性或没起来, 已剔除): {not_applied}。"
                       f"这些臂说明的是「施加失败」, 不是「NPU 没用」")
    perf_gain = bool(bc["wins"]) and not bc["regressions"]
    if bc["n_frame_pairs"] < need:
        reasons.append(f"B 对 C 有效块只有 {bc['n_frame_pairs']} 个, 少于 {need} (stat-guards 最少轮数)")
        perf_gain = False
    if not bc["wins"]:
        reasons.append("功耗、帧时 p95 都没有显著改善")
    if bc["regressions"]:
        reasons.append("有指标显著变差: " + ",".join(bc["regressions"]))
    iv = (img.get("B_vs_C") or {}).get("verdict")
    null_v = (img.get("null_B") or {}).get("verdict")
    image_ok = iv == "pass" and null_v in (None, "pass")
    if iv != "pass":
        reasons.append(f"画质 iq-judge(B 对 C) = {iv or '没有 iq 会话数据'}")
    elif null_v not in (None, "pass"):
        reasons.append(f"画质空改动自检 = {null_v}, 这次画质采集本身不稳, 判不了")
    if bc["n_frame_pairs"] < need:
        gain = "判不了 (B 臂未按设计施加, 有效块不足)" if not_applied else "判不了 (有效块不足, 见原因)"
    elif perf_first:
        gain = "性能增益成立(画质未判定, 只作参考)" if perf_gain else "性能增益不成立"
    else:
        gain = "增益成立" if (perf_gain and image_ok) else "增益不成立"
    verdict = {"mode": "perf-first" if perf_first else "full", "gain": gain, "not_applied_arms": not_applied,
               "image_policy": {"B": "report" if perf_first else "gate"},
               "frame_rule": f"帧率守在上限 (fps_mean 不显著下降)、超时帧 (> {_MISS_MS[0]} ms) 占比不显著上升; 预算内 p95 变长不算退化", "perf_gain": perf_gain,
               "image_ok": image_ok, "reasons": reasons, "min_pairs": need,
               "B_vs_C": bc, "B_vs_A": ba, "A_vs_C": ac, "image": img,
               "n_arms": len(arms), "n_arms_valid": sum(r["valid"] for r in arms)}
    json.dump(verdict, open(os.path.join(out, "verdict.json"), "w"), ensure_ascii=False, indent=1)

    L = [f"# 三臂实验判定 ({verdict['mode']})", "", f"**结论：{gain}**", ""]
    if reasons:
        L += ["未成立/保留的原因：", ""] + [f"- {r}" for r in reasons] + [""]
    for name, j in (("B(低+NPU) 对 C(原生)", bc), ("B(低+NPU) 对 A(低+游戏放大)", ba), ("A(低+游戏放大) 对 C(原生)", ac)):
        L += [f"## {name}", "", f"有效块：帧 {j['n_frame_pairs']}，功耗 {j['n_power_pairs']}；alpha_win={j['alpha_win']:.4f}", "",
              "| 指标 | 对照均值 | 本臂均值 | 差 | 差% | 95% CI | p | 结论 |", "|---|---|---|---|---|---|---|---|"]
        for m, v in j["per_metric"].items():
            L.append(f"| {m} | {fmt(v.get('a_mean'))} | {fmt(v.get('b_mean'))} | {fmt(v.get('diff_mean'))} | "
                     f"{fmt(v.get('diff_pct'))} | {v.get('ci95')} | {fmt(v.get('p'), 4)} | {v['status']} |")
        if j["dropped"]:
            L += ["", "剔除：" + "；".join(f"块{d['block']} {d['why']}" for d in j["dropped"])]
        L.append("")
    L += ["## 画质", "", f"iq 会话：B 对 C = {iv}，空改动自检 = {null_v}，A 对 C = {(img.get('A_vs_C') or {}).get('verdict')}",
          f"跨会话站定帧（只作参考）：{img.get('cross_session_reference_only')}", ""]
    open(os.path.join(out, "verdict.md"), "w", encoding="utf-8").write("\n".join(L) + "\n")
    print("\n".join(L))


if __name__ == "__main__":
    main()
