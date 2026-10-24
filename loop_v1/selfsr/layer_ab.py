#!/usr/bin/env python3
"""layer_ab.py — 自研超分/插帧第 2 步: 透传 Vulkan 层 (gray-layer probe 模式) 挂/不挂的 ABBA。不改游戏任何设置。

    python3 -I loop_v1/selfsr/layer_ab.py run --serial <S> --out <目录> [--rounds 6] [--mode probe] [--so 层.so]

每臂都是冷启动 → 进大世界 → 静置 --settle 秒 → 同一个 --window 秒窗口并行采 (与 loop_v1/gfrc/gfrc_ab.py 同一套口径):
  - SurfaceFlinger timestats: 上屏帧率 / 刷新率 / 丢帧
  - ftrace (loop_v1/tools/ftrace_capture.sh) + phonefarm parse-trace: 游戏 GPU 提交 → 游戏渲染帧率 / p95 帧时
  - 截图锐度 (拉普拉斯方差)
  无层臂 (off): 在 tools/devlock 下 force-stop + phonefarm onboard --enter-only 冷启动进世界。
  有层臂 (on):  phonefarm gray-layer session --mode <mode> 挂层并冷启动游戏 (会话自己持设备租约),
                onboard --enter-only --no-cold 只做导航不重启 (重启会丢层), 采完读层 marker, 再 stop 会话 (会话负责摘层还原)。
  每个有层臂结束后核对: 游戏 lib 目录里没有层 .so、debug.vulkan.layers 为空。核对不过立即中止。
会话自带的 sample 动作在本机 (2026-10-24) ftrace 子进程起不来 (worker exit 76), 所以不用它, 只用会话做挂/摘层。

统计: 每对 (on - off) 配对差, 精确符号翻转检验 + bootstrap 95% 区间。第 2 步的通过口径是「没有显著差别」,
所以报告同时给出差值区间, 让人看清「没测出差别」的分辨力有多大。
"""
import argparse, json, os, shlex, signal, statistics, subprocess, sys, time, uuid

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(REPO, "loop_v1", "gfrc"))
from gfrc_ab import Dev, parse_timestats, layer_src, sharpness, signflip, boot_ci  # noqa: E402

PKG_DEFAULT = "com.miHoYo.Yuanshen"
DEV_TRACE = "/data/local/tmp/selfsr_ftrace_capture.sh"


def pf_json(pf, *args, inp=None, timeout=600):
    r = subprocess.run([pf, *args], input=inp, capture_output=True, text=True, timeout=timeout)
    try:
        return r.returncode, json.loads(r.stdout)
    except ValueError:
        return r.returncode, {"raw": r.stdout[-2000:], "err": r.stderr[-2000:]}


def session_action(pf, out, action, params=None):
    _, d = pf_json(pf, "gray-layer", "session-status", "--out", out, "--json")
    c = d["controller"]
    rid = f"selfsr-{action}-{uuid.uuid4().hex[:8]}"
    req = {"schema": "phonefarm/gray-control-action@1", "request_id": rid, "session_id": d["session_id"],
           "actor_id": c["actor_id"], "control_epoch": c["epoch"], "action": action, "params": params or {}}
    code, rec = pf_json(pf, "gray-layer", "session-action", "--out", out, "--request", "-", "--json",
                        inp=json.dumps(req))
    for _ in range(300):
        if code != 0 or rec.get("status") in ("completed", "failed", "rejected", "unknown"):
            break
        time.sleep(1)
        code, rec = pf_json(pf, "gray-layer", "session-status", "--out", out, "--request-id", rid, "--json")
    return rec


def measure(dev, a, name, out):
    """在当前画面采一个窗口, 返回指标 dict。"""
    dev_trace = f"/data/local/tmp/selfsr_trace_{name}.txt"
    ts_cmd = ("dumpsys SurfaceFlinger --timestats -disable -clear >/dev/null; "
              "dumpsys SurfaceFlinger --timestats -enable >/dev/null; "
              f"sleep {a.window}; dumpsys SurfaceFlinger --timestats -dump -maxlayers 8; "
              "dumpsys SurfaceFlinger --timestats -disable >/dev/null")
    tr = subprocess.Popen(f"adb -s {shlex.quote(a.serial)} shell su -c "
                          + shlex.quote(shlex.quote(f"sh {DEV_TRACE} {a.window} {dev_trace} {a.pkg}")),
                          shell=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ts = parse_timestats(dev.shell(ts_cmd, timeout=a.window + 60), a.pkg)
    tr.wait(timeout=a.window + 90)
    local_trace = os.path.join(out, "traces", f"{name}.txt")
    dev.adb(f"exec-out su -c {shlex.quote('cat ' + dev_trace)} > {shlex.quote(local_trace)}")
    dev.su(f"rm -f {dev_trace}")
    pt = subprocess.run([a.phonefarm, "parse-trace", local_trace], capture_output=True, text=True)
    try:
        game = json.loads(pt.stdout)
    except ValueError:
        game = {}
    fg = dev.shell("dumpsys window | grep mCurrentFocus")
    src = layer_src(dev.shell("dumpsys SurfaceFlinger"), a.pkg)
    shot = os.path.join(out, "shots", f"{name}.png")
    dev.adb(f"exec-out screencap -p > {shlex.quote(shot)}")
    span = (ts.get("displayOnTime") or a.window * 1000) / 1000
    return {"in_game": a.pkg in fg,
            "display_fps": round(ts["layer_frames"] / span, 2) if ts.get("layer_frames") else None,
            "display_hz": ts.get("display_hz"), "missed_frames": ts.get("missedFrames"),
            "game_fps": game.get("fps_mean"), "game_frame_p95_ms": game.get("frame_p95"),
            "out_res": src, "sharp_full": sharpness(shot)}


def layer_absent(dev, pkg):
    lib = dev.su(f"pm path {pkg} >/dev/null; ls -d /data/app/*/{pkg}-*/lib/arm64").strip().splitlines()
    has_so = any(dev.su(f"ls {shlex.quote(d)} | grep -c libVkLayer_refknobs").strip() not in ("0", "") for d in lib)
    prop = dev.prop("debug.vulkan.layers")
    return (not has_so) and prop == "", {"lib_dirs": lib, "so_present": has_so, "debug.vulkan.layers": prop}


def onboard(a, out, name, cold):
    args = [a.phonefarm, "onboard", "--package", a.pkg, "--serial", a.serial, "--enter-only", "--no-guest",
            "--out", os.path.join(out, "onboard", name)]
    if not cold:
        args.append("--no-cold")
    r = subprocess.run(args, capture_output=True, text=True, timeout=900)
    return r.returncode, r.stderr[-600:]


def arm_off(a, out, name):
    """无层臂 (在 devlock 下被调用)。"""
    dev = Dev(a.serial)
    dev.shell(f"am force-stop {a.pkg}")
    code, err = onboard(a, out, name, cold=True)
    if code != 0:
        return {"error": f"onboard: {err}"}
    time.sleep(a.settle)
    rec = measure(dev, a, name, out)
    dev.shell(f"am force-stop {a.pkg}")
    return rec


def arm_on(a, out, name, note):
    dev = Dev(a.serial)
    sdir = os.path.join(out, "sessions", name)
    log = open(sdir + ".log", "w")
    cmd = [a.phonefarm, "gray-layer", "session", "--mode", a.mode, "--package", a.pkg, "--serial", a.serial,
           "--out", sdir, "--max-seconds", str(a.session_seconds)]
    if a.so:
        cmd += ["--so", a.so]
    proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT)
    rec = {}
    try:
        phase = None
        for _ in range(240):
            time.sleep(2)
            if proc.poll() is not None:
                break
            code, st = pf_json(a.phonefarm, "gray-layer", "session-status", "--out", sdir, "--json")
            phase = st.get("phase") if code == 0 else None
            if phase == "ready":
                break
        if phase != "ready":
            return {"error": f"session not ready (phase={phase}, exit={proc.poll()})"}
        code, err = onboard(a, out, name, cold=False)
        if code != 0:
            return {"error": f"onboard --no-cold: {err}"}
        time.sleep(a.settle)
        code, st = pf_json(a.phonefarm, "gray-layer", "session-status", "--out", sdir, "--json")
        if proc.poll() is not None or st.get("phase") != "ready":
            return {"error": f"会话在测量前已结束 (phase={st.get('phase')}, stop={st.get('stop_reason')})"}
        m0 = session_action(a.phonefarm, sdir, "marker")
        rec = measure(dev, a, name, out)
        m1 = session_action(a.phonefarm, sdir, "marker")

        def frames(m):
            try:
                p = os.path.join(sdir, m["result"]["path"])
                j = json.load(open(p))
                return j.get("layer_loaded"), (j.get("readonly_stats") or {}).get("frames")
            except Exception:
                return None, None
        (l0, f0), (l1, f1) = frames(m0), frames(m1)
        if not l0:
            return {**rec, "error": "层没加载 (marker layer_loaded 不为真)", "marker0": m0}
        rec["layer_loaded"] = bool(l0 and l1)
        rec["layer_frames_in_window"] = (f1 - f0) if (f0 is not None and f1 is not None) else None
    finally:
        st = session_action(a.phonefarm, sdir, "stop") if proc.poll() is None else None
        try:
            proc.wait(timeout=300)
        except subprocess.TimeoutExpired:
            proc.terminate()
            proc.wait(timeout=60)
        rec["session_exit"] = proc.returncode
        rec["session_stop"] = (st or {}).get("status")
        dev.shell(f"am force-stop {a.pkg}")
        ok, info = layer_absent(dev, a.pkg)
        rec["layer_removed"] = ok
        rec["layer_removed_info"] = info
        note(f"{name}: 会话退出 {proc.returncode}, 层已摘除 {ok} {info}")
    return rec


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd", choices=["run", "arm-off"])
    ap.add_argument("--serial", required=True)
    ap.add_argument("--pkg", default=PKG_DEFAULT)
    ap.add_argument("--out", required=True)
    ap.add_argument("--rounds", type=int, default=6)
    ap.add_argument("--mode", default="probe")
    ap.add_argument("--so", default=None)
    ap.add_argument("--settle", type=float, default=20)
    ap.add_argument("--window", type=int, default=10)
    ap.add_argument("--session-seconds", type=int, default=900)
    ap.add_argument("--name", default=None, help="arm-off 用: 臂名")
    ap.add_argument("--phonefarm", default=os.environ.get("PHONEFARM_BIN") or os.path.join(REPO, "phonefarm"))
    a = ap.parse_args()
    out = os.path.abspath(a.out)
    for d in ("shots", "traces", "sessions", "onboard"):
        os.makedirs(os.path.join(out, d), exist_ok=True)

    if a.cmd == "arm-off":
        rec = arm_off(a, out, a.name)
        print("ARM_JSON " + json.dumps(rec, ensure_ascii=False))
        return 0

    log = open(os.path.join(out, "run.log"), "a")

    def note(msg):
        line = f"{time.strftime('%H:%M:%S')} {msg}"
        print(line, flush=True)
        log.write(line + "\n")
        log.flush()

    dev = Dev(a.serial)
    ok, info = layer_absent(dev, a.pkg)
    if not ok:
        note(f"开跑前设备上已有层残留, 拒绝开始: {info}")
        return 2
    orig_timeout = dev.shell("settings get system screen_off_timeout").strip()
    dev.shell("settings put system screen_off_timeout 1800000")
    dev.adb(f"push {shlex.quote(os.path.join(REPO, 'loop_v1/tools/ftrace_capture.sh'))} {DEV_TRACE}", check=True)
    stop = {"flag": False}
    signal.signal(signal.SIGINT, lambda *_: stop.update(flag=True))
    signal.signal(signal.SIGTERM, lambda *_: stop.update(flag=True))

    order = []
    for i in range(a.rounds):
        order += [("off", i), ("on", i)] if i % 2 == 0 else [("on", i), ("off", i)]
    arms = []
    try:
        for k, (state, pair) in enumerate(order):
            if stop["flag"]:
                note("收到中止信号, 停在这里")
                break
            name = f"{state}{pair + 1}"
            dev.shell("input keyevent KEYCODE_WAKEUP")
            if state == "off":
                cmd = [os.path.join(REPO, "tools", "devlock"), "run", f"selfsr-{name}", "--", sys.executable, "-I",
                       os.path.abspath(__file__), "arm-off", "--serial", a.serial, "--pkg", a.pkg, "--out", out,
                       "--name", name, "--settle", str(a.settle), "--window", str(a.window), "--phonefarm", a.phonefarm]
                # devlock run 轮不到时立即返回 1 (不等), 这里有界重试
                for _ in range(60):
                    r = subprocess.run(cmd, capture_output=True, text=True, timeout=1800,
                                       env={**os.environ, "PHONEFARM_BIN": a.phonefarm})
                    if r.returncode == 0 or not any(s in r.stderr for s in ("not your turn", "busy:")):
                        break
                    time.sleep(10)
                line = next((l for l in r.stdout.splitlines() if l.startswith("ARM_JSON ")), None)
                rec = json.loads(line[9:]) if line else {"error": f"arm-off exit {r.returncode}: {r.stderr[-400:]}"}
            else:
                rec = arm_on(a, out, name, note)
                if not rec.get("layer_removed", False):
                    arms.append({"arm": name, "state": state, "pair": pair + 1, "seq": k + 1, **rec})
                    note(f"{name}: 层没摘干净, 中止整轮 (先人工核对再继续)")
                    break
            rec = {"arm": name, "state": state, "pair": pair + 1, "seq": k + 1, **rec, "t": time.time()}
            arms.append(rec)
            open(os.path.join(out, "arms.jsonl"), "a").write(json.dumps(rec, ensure_ascii=False) + "\n")
            if rec.get("error"):
                note(f"{name}: 作废 — {rec['error']}")
            else:
                note(f"{name}: 显示 {rec['display_fps']} fps @{rec['display_hz']}Hz 游戏 {rec['game_fps']} fps "
                     f"p95 {rec['game_frame_p95_ms']} ms 锐度 {rec['sharp_full']:.1f} 输出 {rec['out_res']}"
                     + (f" 层帧数 {rec.get('layer_frames_in_window')}" if state == "on" else ""))
    finally:
        dev.shell(f"settings put system screen_off_timeout {orig_timeout}")
        ok, info = layer_absent(dev, a.pkg)
        note(f"收尾核对: 层已不在 {ok} {info}; screen_off_timeout 还原为 {orig_timeout}")

    def valid(r):
        if r.get("error") or not r.get("in_game"):
            return False
        return r["state"] == "off" or (r.get("layer_loaded") and r.get("layer_removed"))
    good = [r for r in arms if valid(r)]
    pairs = sorted({r["pair"] for r in good if sum(1 for x in good if x["pair"] == r["pair"]) == 2})
    metrics = ["display_fps", "game_fps", "game_frame_p95_ms", "sharp_full", "missed_frames"]
    res = {}
    for m in metrics:
        offs = [next(x[m] for x in good if x["pair"] == p and x["state"] == "off") for p in pairs]
        ons = [next(x[m] for x in good if x["pair"] == p and x["state"] == "on") for p in pairs]
        if not pairs or any(v is None for v in offs + ons):
            res[m] = {"status": "not_measured"}
            continue
        d = [b - c for b, c in zip(ons, offs)]
        mo = statistics.mean(offs)
        res[m] = {"off_mean": round(mo, 3), "on_mean": round(statistics.mean(ons), 3),
                  "diff_mean": round(statistics.mean(d), 3),
                  "diff_pct": round(100 * statistics.mean(d) / mo, 2) if mo else None,
                  "ci95": [round(x, 3) for x in boot_ci(d)] if len(d) > 1 else None,
                  "p": signflip(d), "n_pairs": len(d)}
    report = {"pkg": a.pkg, "mode": a.mode, "rounds": a.rounds, "valid_pairs": len(pairs),
              "dropped_arms": [r["arm"] for r in arms if r not in good],
              "layer_frames_in_window": [r.get("layer_frames_in_window") for r in good if r["state"] == "on"],
              "metrics": res, "min_p_possible": 2 / 2 ** len(pairs) if pairs else None}
    json.dump(report, open(os.path.join(out, "report.json"), "w"), ensure_ascii=False, indent=1)
    names = {"display_fps": "上屏帧率", "game_fps": "游戏渲染帧率 (GPU 提交)",
             "game_frame_p95_ms": "游戏渲染 p95 帧时 (ms)", "sharp_full": "锐度 (拉普拉斯方差)",
             "missed_frames": "SF 丢帧 / 窗口"}
    md = [f"# 透传层挂/不挂 ABBA: {a.pkg} (gray-layer {a.mode})", "",
          f"每臂冷启动进大世界, 有效 {len(pairs)} 对, 精确符号翻转检验 (最小可能 p = {report['min_p_possible']})。"
          f"游戏设置未改。作废臂: {report['dropped_arms'] or '无'}。", "",
          f"有层臂每个采样窗口里层自己数到的帧: {report['layer_frames_in_window']}", "",
          "| 指标 | 不挂层 | 挂层 | 差 | 差% | 95% 区间 | p |", "|---|---|---|---|---|---|---|"]
    for m in metrics:
        r = res[m]
        if r.get("status"):
            md.append(f"| {names[m]} | - | - | - | - | - | 未测成 |")
            continue
        md.append(f"| {names[m]} | {r['off_mean']} | {r['on_mean']} | {r['diff_mean']:+} | {r['diff_pct']}% "
                  f"| {r['ci95']} | {r['p']:.4f} |")
    open(os.path.join(out, "report.md"), "w").write("\n".join(md) + "\n")
    print("\n".join(md))
    return 0


if __name__ == "__main__":
    sys.exit(main())
