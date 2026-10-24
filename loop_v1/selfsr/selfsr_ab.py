#!/usr/bin/env python3
"""selfsr_ab.py — 自研超分/插帧的两臂 ABBA (冷启动、进大世界、同一套测量)。不改游戏内任何设置。

    python3 -I loop_v1/selfsr/selfsr_ab.py run --serial <S> --out <目录> --a native --b layer [--rounds 6]

臂的种类 (--a / --b):
  native              原生: 不挂层, Game Mode 无干预
  layer               只挂透传层 (gray-layer session --mode probe), 第 2 步用
  hwc:<F>             Android Game Mode 降分辨率 F (cmd game mode custom + cmd game set --downscale F), 放大交给 HWC
  sr:<F>:<宽>x<高>[:<锐化>]  同上降分辨率 F, 再挂层并设 debug.knobs.selfsr=<宽>x<高>[:<锐化>]: 层在送显前把游戏的小图超分到 <宽>x<高>
每臂: (devlock 下) 设好 Game Mode 与 selfsr 属性 → 冷启动进大世界 → 静置 --settle 秒 → --window 秒窗口并行采
  - SurfaceFlinger timestats: 上屏帧率 / 刷新率 / 丢帧
  - ftrace + phonefarm parse-trace: 游戏 GPU 提交 → 游戏渲染帧率 / p95 帧时
  - 截图: 锐度 (拉普拉斯方差), 与本轮第一张 native 截图的 SSIM (跨冷启动场景会有云/时辰差异, 只作参考)
  - 游戏图层送显缓冲尺寸 (HWC 源); 挂层臂另读层 marker (层加载、窗口内帧数、selfsr 实际尺寸与计数)
  不挂层的臂整臂在 tools/devlock 下跑; 挂层的臂由 gray-layer session 持设备租约, 挂层并冷启动游戏,
  onboard --enter-only --no-cold 只导航不重启 (重启会丢层), 采完 stop 会话 (会话负责摘层)。
  会话自带的 sample 在本机 ftrace 子进程起不来 (worker exit 76), 所以测量不用它。
收尾 (含 Ctrl-C): Game Mode 恢复原模式并 reset 干预、清 debug.knobs.selfsr、screen_off_timeout 还原, 逐项回读核对。
统计: 每对 (B - A) 配对差, 精确符号翻转检验 + bootstrap 95% 区间。
"""
import argparse, json, os, shlex, signal, statistics, subprocess, sys, time, uuid

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(REPO, "loop_v1", "gfrc"))
from gfrc_ab import Dev, parse_timestats, layer_src, sharpness, signflip, boot_ci  # noqa: E402

PKG_DEFAULT = "com.miHoYo.Yuanshen"
DEV_TRACE = "/data/local/tmp/selfsr_ftrace_capture.sh"
SR_PROP = "debug.knobs.selfsr"


def parse_kind(s):
    p = s.split(":")
    if p[0] in ("native", "layer") and len(p) == 1:
        return {"kind": p[0], "downscale": None, "selfsr": None}
    if p[0] == "hwc" and len(p) == 2:
        return {"kind": "hwc", "downscale": float(p[1]), "selfsr": None}
    if p[0] == "sr" and len(p) in (3, 4) and "x" in p[2]:   # sr:<F>:<宽>x<高>[:<锐化>]
        return {"kind": "sr", "downscale": float(p[1]), "selfsr": ":".join(p[2:])}
    raise argparse.ArgumentTypeError(f"不认识的臂: {s}")


def uses_layer(k):
    return k["kind"] in ("layer", "sr")


def pf_json(pf, *args, inp=None, timeout=600):
    r = subprocess.run([pf, *args], input=inp, capture_output=True, text=True, timeout=timeout)
    try:
        return r.returncode, json.loads(r.stdout)
    except ValueError:
        return r.returncode, {"raw": r.stdout[-2000:], "err": r.stderr[-2000:]}


def session_action(pf, out, action, params=None):
    _, d = pf_json(pf, "gray-layer", "session-status", "--out", out, "--json")
    c = d.get("controller") or {}
    rid = f"selfsr-{action}-{uuid.uuid4().hex[:8]}"
    req = {"schema": "phonefarm/gray-control-action@1", "request_id": rid, "session_id": d.get("session_id"),
           "actor_id": c.get("actor_id"), "control_epoch": c.get("epoch"), "action": action, "params": params or {}}
    code, rec = pf_json(pf, "gray-layer", "session-action", "--out", out, "--request", "-", "--json",
                        inp=json.dumps(req))
    for _ in range(300):
        if code != 0 or rec.get("status") in ("completed", "failed", "rejected", "unknown"):
            break
        time.sleep(1)
        code, rec = pf_json(pf, "gray-layer", "session-status", "--out", out, "--request-id", rid, "--json")
    return rec


def ssim(png_a, png_b):
    """灰度 SSIM (8x8 不重叠块的均值), 两图尺寸不同时把 b 缩放到 a。"""
    from PIL import Image
    import numpy as np
    a = Image.open(png_a).convert("L")
    b = Image.open(png_b).convert("L")
    if b.size != a.size:
        b = b.resize(a.size, Image.BICUBIC)
    x = np.asarray(a, dtype=np.float64)
    y = np.asarray(b, dtype=np.float64)
    h, w = (x.shape[0] // 8) * 8, (x.shape[1] // 8) * 8
    x = x[:h, :w].reshape(h // 8, 8, w // 8, 8)
    y = y[:h, :w].reshape(h // 8, 8, w // 8, 8)
    mx, my = x.mean(axis=(1, 3)), y.mean(axis=(1, 3))
    vx, vy = x.var(axis=(1, 3)), y.var(axis=(1, 3))
    cxy = ((x - mx[:, None, :, None]) * (y - my[:, None, :, None])).mean(axis=(1, 3))
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    s = ((2 * mx * my + c1) * (2 * cxy + c2)) / ((mx ** 2 + my ** 2 + c1) * (vx + vy + c2))
    return float(s.mean())


def game_mode(dev, pkg, downscale, orig_mode):
    """设 Game Mode 干预; downscale=None 表示无干预。返回核对信息。"""
    if downscale is None:
        dev.shell(f"cmd game reset {pkg}")
        if orig_mode and orig_mode not in ("unknown", ""):
            dev.shell(f"cmd game mode {orig_mode} {pkg}")
        cfg = dev.shell(f"cmd game list-configs {pkg}")
        return {"downscale": None, "ok": "downscale" not in cfg.lower() or "no intervention" in cfg.lower(), "configs": cfg[-300:]}
    o1 = dev.shell(f"cmd game mode custom {pkg}")
    o2 = dev.shell(f"cmd game set --downscale {downscale} {pkg}")
    cfg = dev.shell(f"cmd game list-configs {pkg}")
    return {"downscale": downscale, "ok": f"{downscale}" in cfg, "configs": cfg[-300:], "out": (o1 + o2)[-200:]}


def current_mode(dev, pkg):
    t = dev.shell(f"cmd game list-modes {pkg}")
    return t.split("current mode:")[1].split(",")[0].strip() if "current mode:" in t else "unknown"


def measure(dev, a, name, out):
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
            "out_res": src, "sharp_full": sharpness(shot), "shot": shot}


def layer_absent(dev, pkg):
    lib = dev.su(f"ls -d /data/app/*/{pkg}-*/lib/arm64").strip().splitlines()
    has_so = any(dev.su(f"ls {shlex.quote(d)} | grep -c libVkLayer_refknobs").strip() not in ("0", "") for d in lib)
    prop = dev.prop("debug.vulkan.layers")
    return (not has_so) and prop == "", {"so_present": has_so, "debug.vulkan.layers": prop}


def onboard(a, out, name, cold):
    args = [a.phonefarm, "onboard", "--package", a.pkg, "--serial", a.serial, "--enter-only", "--no-guest",
            "--out", os.path.join(out, "onboard", name)]
    if not cold:
        # 挂层臂: 设备租约在 gray-layer session 手里, onboard 自己再拿锁会一直排队; 在会话的租约内导航
        args += ["--no-cold", "--no-devlock"]
    r = subprocess.run(args, capture_output=True, text=True, timeout=900)
    return r.returncode, r.stderr[-600:]


def prepare(a, k, orig_mode):
    """在设备锁下调用: Game Mode 与 selfsr 属性。"""
    dev = Dev(a.serial)
    dev.shell(f"am force-stop {a.pkg}")
    gm = game_mode(dev, a.pkg, k["downscale"], orig_mode)
    dev.su(f"setprop {SR_PROP} {shlex.quote(k['selfsr'] or '')}")
    return {"game_mode": gm, "selfsr_prop": dev.prop(SR_PROP)}


def arm_plain(a, out, name, k, orig_mode):
    """不挂层的臂 (整臂在 devlock 下被调用)。"""
    dev = Dev(a.serial)
    rec = {"prep": prepare(a, k, orig_mode)}
    code, err = onboard(a, out, name, cold=True)
    if code != 0:
        rec["error"] = f"onboard: {err}"
        return rec
    time.sleep(a.settle)
    rec.update(measure(dev, a, name, out))
    dev.shell(f"am force-stop {a.pkg}")
    return rec


def arm_layer(a, out, name, k, note, locked):
    dev = Dev(a.serial)
    rec = {}
    r = locked("prep", name, k)
    rec["prep"] = r.get("prep")
    if r.get("error"):
        rec["error"] = r["error"]
        return rec
    sdir = os.path.join(out, "sessions", name)
    log = open(sdir + ".log", "w")
    cmd = [a.phonefarm, "gray-layer", "session", "--mode", "probe", "--package", a.pkg, "--serial", a.serial,
           "--out", sdir, "--max-seconds", str(a.session_seconds)]
    if a.so:
        cmd += ["--so", a.so]
    proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT)
    st = None
    try:
        phase = None
        for _ in range(450):
            time.sleep(2)
            if proc.poll() is not None:
                break
            code, s = pf_json(a.phonefarm, "gray-layer", "session-status", "--out", sdir, "--json")
            phase = s.get("phase") if code == 0 else None
            if phase == "ready":
                break
        if phase != "ready":
            rec["error"] = f"session not ready (phase={phase}, exit={proc.poll()})"
            return rec
        code, err = onboard(a, out, name, cold=False)
        if code != 0:
            rec["error"] = f"onboard --no-cold: {err}"
            return rec
        time.sleep(a.settle)
        code, s = pf_json(a.phonefarm, "gray-layer", "session-status", "--out", sdir, "--json")
        if proc.poll() is not None or s.get("phase") != "ready":
            rec["error"] = f"会话在测量前已结束 (phase={s.get('phase')}, stop={s.get('stop_reason')})"
            return rec

        def marker():
            m = session_action(a.phonefarm, sdir, "marker")
            try:
                return json.load(open(os.path.join(sdir, m["result"]["path"])))
            except Exception:
                return {}
        m0 = marker()
        rec.update(measure(dev, a, name, out))
        m1 = marker()
        rec["layer_loaded"] = bool(m0.get("layer_loaded") and m1.get("layer_loaded"))
        f0 = (m0.get("readonly_stats") or {}).get("frames")
        f1 = (m1.get("readonly_stats") or {}).get("frames")
        rec["layer_frames_in_window"] = (f1 - f0) if (f0 is not None and f1 is not None) else None
        if k["kind"] == "sr":
            s0, s1 = m0.get("selfsr") or {}, m1.get("selfsr") or {}
            rec["selfsr"] = s1
            up = (s1.get("upscaled") or 0) - (s0.get("upscaled") or 0)
            rec["selfsr_upscaled_in_window"] = up
            if up <= 0:
                rec["error"] = f"层没在超分 (selfsr={s1})"
        if not rec["layer_loaded"]:
            rec["error"] = "层没加载 (marker layer_loaded 不为真)"
    finally:
        if proc.poll() is None:
            st = session_action(a.phonefarm, sdir, "stop")
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
    ap.add_argument("cmd", choices=["run", "_locked"])
    ap.add_argument("--serial", required=True)
    ap.add_argument("--pkg", default=PKG_DEFAULT)
    ap.add_argument("--out", required=True)
    ap.add_argument("--a", type=parse_kind, default=parse_kind("native"))
    ap.add_argument("--b", type=parse_kind, default=parse_kind("layer"))
    ap.add_argument("--rounds", type=int, default=6)
    ap.add_argument("--so", default=None, help="层 .so (挂层臂); 缺省用 gray-layer session 自己的")
    ap.add_argument("--settle", type=float, default=20)
    ap.add_argument("--window", type=int, default=10)
    ap.add_argument("--session-seconds", type=int, default=1200)
    ap.add_argument("--phonefarm", default=os.environ.get("PHONEFARM_BIN") or os.path.join(REPO, "phonefarm"))
    # _locked 内部用
    ap.add_argument("--what", default=None)
    ap.add_argument("--name", default=None)
    ap.add_argument("--kind", default=None)
    ap.add_argument("--orig-mode", default="standard")
    a = ap.parse_args()
    out = os.path.abspath(a.out)
    for d in ("shots", "traces", "sessions", "onboard"):
        os.makedirs(os.path.join(out, d), exist_ok=True)

    if a.cmd == "_locked":
        k = parse_kind(a.kind)
        if a.what == "plain":
            rec = arm_plain(a, out, a.name, k, a.orig_mode)
        elif a.what == "prep":
            rec = {"prep": prepare(a, k, a.orig_mode)}
            if not rec["prep"]["game_mode"]["ok"]:
                rec["error"] = f"Game Mode 没设上: {rec['prep']['game_mode']}"
        else:  # restore
            dev = Dev(a.serial)
            dev.shell(f"am force-stop {a.pkg}")
            gm = game_mode(dev, a.pkg, None, a.orig_mode)
            dev.su(f"setprop {SR_PROP} ''")
            rec = {"game_mode": gm, "mode_now": current_mode(dev, a.pkg), "selfsr_prop": dev.prop(SR_PROP)}
        print("ARM_JSON " + json.dumps(rec, ensure_ascii=False))
        return 0

    log = open(os.path.join(out, "run.log"), "a")

    def note(msg):
        line = f"{time.strftime('%H:%M:%S')} {msg}"
        print(line, flush=True)
        log.write(line + "\n")
        log.flush()

    raw = sys.argv
    ka = raw[raw.index("--a") + 1] if "--a" in raw else "native"
    kb = raw[raw.index("--b") + 1] if "--b" in raw else "layer"

    def locked(what, name, k_str):
        cmd = [os.path.join(REPO, "tools", "devlock"), "run", f"selfsr-{what}-{name}", "--", sys.executable, "-I",
               os.path.abspath(__file__), "_locked", "--what", what, "--name", name, "--kind", k_str,
               "--orig-mode", orig_mode, "--serial", a.serial, "--pkg", a.pkg, "--out", out,
               "--settle", str(a.settle), "--window", str(a.window), "--phonefarm", a.phonefarm]
        r = None
        for _ in range(90):   # devlock run 轮不到时立即返回 1 (不等), 有界重试
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=1800,
                               env={**os.environ, "PHONEFARM_BIN": a.phonefarm})
            if r.returncode == 0 or not any(s in r.stderr for s in ("not your turn", "busy:")):
                break
            time.sleep(10)
        line = next((l for l in r.stdout.splitlines() if l.startswith("ARM_JSON ")), None)
        return json.loads(line[9:]) if line else {"error": f"{what} exit {r.returncode}: {r.stderr[-400:]}"}

    dev = Dev(a.serial)
    ok, info = layer_absent(dev, a.pkg)
    if not ok:
        note(f"开跑前设备上已有层残留, 拒绝开始: {info}")
        return 2
    orig_mode = current_mode(dev, a.pkg)
    orig_cfg = dev.shell(f"cmd game list-configs {a.pkg}")
    orig_sr = dev.prop(SR_PROP)
    orig_timeout = dev.shell("settings get system screen_off_timeout").strip()
    json.dump({"game_mode": orig_mode, "game_configs": orig_cfg, "selfsr_prop": orig_sr,
               "screen_off_timeout": orig_timeout}, open(os.path.join(out, "orig.json"), "w"), ensure_ascii=False)
    if "no intervention" not in orig_cfg.lower() and "downscale" in orig_cfg.lower():
        note(f"开跑前本包已有 Game Mode 干预, 拒绝开始 (不覆盖别人的设置): {orig_cfg[-200:]}")
        return 2
    dev.shell("settings put system screen_off_timeout 1800000")
    dev.adb(f"push {shlex.quote(os.path.join(REPO, 'loop_v1/tools/ftrace_capture.sh'))} {DEV_TRACE}", check=True)
    stop = {"flag": False}
    signal.signal(signal.SIGINT, lambda *_: stop.update(flag=True))
    signal.signal(signal.SIGTERM, lambda *_: stop.update(flag=True))
    note(f"A={ka} B={kb} rounds={a.rounds} 原 Game Mode={orig_mode}")

    order = []
    for i in range(a.rounds):
        order += [("A", i), ("B", i)] if i % 2 == 0 else [("B", i), ("A", i)]
    arms = []
    ref_shot = None
    try:
        for seq, (arm, pair) in enumerate(order):
            if stop["flag"]:
                note("收到中止信号, 停在这里")
                break
            k, k_str = (a.a, ka) if arm == "A" else (a.b, kb)
            name = f"{arm}{pair + 1}"
            dev.shell("input keyevent KEYCODE_WAKEUP")
            if uses_layer(k):
                rec = arm_layer(a, out, name, k, note, lambda w, n, _k: locked(w, n, k_str))
            else:
                rec = locked("plain", name, k_str)
            rec = {"arm": name, "side": arm, "kind": k_str, "pair": pair + 1, "seq": seq + 1, **rec, "t": time.time()}
            if rec.get("shot") and os.path.exists(rec["shot"]):
                if k["kind"] == "native" and ref_shot is None and not rec.get("error"):
                    ref_shot = rec["shot"]
                rec["ssim_vs_native_ref"] = ssim(ref_shot, rec["shot"]) if ref_shot else None
            arms.append(rec)
            open(os.path.join(out, "arms.jsonl"), "a").write(json.dumps(rec, ensure_ascii=False) + "\n")
            if rec.get("error"):
                note(f"{name} ({k_str}): 作废 — {rec['error']}")
            else:
                note(f"{name} ({k_str}): 显示 {rec.get('display_fps')} fps @{rec.get('display_hz')}Hz 游戏 {rec.get('game_fps')} fps "
                     f"p95 {rec.get('game_frame_p95_ms')} ms 锐度 {rec.get('sharp_full', 0):.1f} 输出 {rec.get('out_res')}"
                     + (f" 层帧数 {rec.get('layer_frames_in_window')}" if uses_layer(k) else "")
                     + (f" 超分 {rec.get('selfsr_upscaled_in_window')} 次 {(rec.get('selfsr') or {}).get('game_extent')}→{(rec.get('selfsr') or {}).get('swap_extent')}" if k['kind'] == 'sr' else ""))
            if uses_layer(k) and not rec.get("layer_removed", False):
                note(f"{name}: 层没摘干净, 中止整轮 (先人工核对再继续)")
                break
    finally:
        r = locked("restore", "end", "native")
        dev.shell(f"settings put system screen_off_timeout {orig_timeout}")
        ok, info = layer_absent(dev, a.pkg)
        json.dump({**r, "layer_absent": ok, "layer_info": info}, open(os.path.join(out, "restore.json"), "w"), ensure_ascii=False)
        note(f"收尾核对: Game Mode {r.get('mode_now')} (原 {orig_mode}), selfsr 属性 '{r.get('selfsr_prop')}', "
             f"层已不在 {ok}, screen_off_timeout 还原为 {orig_timeout}")

    good = [r for r in arms if not r.get("error") and r.get("in_game")
            and (not uses_layer(parse_kind(r["kind"])) or (r.get("layer_loaded") and r.get("layer_removed")))]
    pairs = sorted({r["pair"] for r in good if sum(1 for x in good if x["pair"] == r["pair"]) == 2})
    metrics = ["display_fps", "game_fps", "game_frame_p95_ms", "sharp_full", "ssim_vs_native_ref", "missed_frames"]
    res = {}
    for m in metrics:
        av = [next(x.get(m) for x in good if x["pair"] == p and x["side"] == "A") for p in pairs]
        bv = [next(x.get(m) for x in good if x["pair"] == p and x["side"] == "B") for p in pairs]
        if not pairs or any(v is None for v in av + bv):
            res[m] = {"status": "not_measured"}
            continue
        d = [y - x for x, y in zip(av, bv)]
        ma = statistics.mean(av)
        res[m] = {"a_mean": round(ma, 4), "b_mean": round(statistics.mean(bv), 4), "diff_mean": round(statistics.mean(d), 4),
                  "diff_pct": round(100 * statistics.mean(d) / ma, 2) if ma else None,
                  "ci95": [round(x, 4) for x in boot_ci(d)] if len(d) > 1 else None, "p": signflip(d), "n_pairs": len(d)}
    report = {"pkg": a.pkg, "a": ka, "b": kb, "rounds": a.rounds, "valid_pairs": len(pairs),
              "dropped_arms": [r["arm"] for r in arms if r not in good],
              "out_res": {side: sorted({tuple(r["out_res"]) for r in good if r["side"] == side and r.get("out_res")}) for side in "AB"},
              "metrics": res, "min_p_possible": 2 / 2 ** len(pairs) if pairs else None}
    json.dump(report, open(os.path.join(out, "report.json"), "w"), ensure_ascii=False, indent=1)
    names = {"display_fps": "上屏帧率", "game_fps": "游戏渲染帧率 (GPU 提交)", "game_frame_p95_ms": "游戏渲染 p95 帧时 (ms)",
             "sharp_full": "锐度 (拉普拉斯方差)", "ssim_vs_native_ref": "与第一张原生截图的 SSIM", "missed_frames": "SF 丢帧 / 窗口"}
    md = [f"# {ka} (A) vs {kb} (B): {a.pkg}", "",
          f"每臂冷启动进大世界, 有效 {len(pairs)} 对, 精确符号翻转检验 (最小可能 p = {report['min_p_possible']})。"
          f"游戏内设置未改。作废臂: {report['dropped_arms'] or '无'}。送显缓冲: {report['out_res']}", "",
          "| 指标 | A | B | 差 (B-A) | 差% | 95% 区间 | p |", "|---|---|---|---|---|---|---|"]
    for m in metrics:
        r = res[m]
        if r.get("status"):
            md.append(f"| {names[m]} | - | - | - | - | - | 未测成 |")
            continue
        md.append(f"| {names[m]} | {r['a_mean']} | {r['b_mean']} | {r['diff_mean']:+} | {r['diff_pct']}% | {r['ci95']} | {r['p']:.4f} |")
    open(os.path.join(out, "report.md"), "w").write("\n".join(md) + "\n")
    print("\n".join(md))
    return 0


if __name__ == "__main__":
    sys.exit(main())
