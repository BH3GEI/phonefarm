#!/usr/bin/env python3
"""farm: 多 agent 共用手机农场时的四件事 —— 设备租约、设备命令守卫、现场快照与恢复校验、任务文件协议。

  farm lease acquire|renew|release|status|queue <serial> [label…]   每台设备一把锁 (复用 tools/devlock)
  farm guard adb|hdc <argv…>        adb/hdc 垫片调用: 查租约、拦危险命令、记下改了哪些设置
  farm job begin <serial> --label L [--task DIR] [--keys K…]   开工前快照
  farm job end   <serial> --label L [--task DIR] [--no-restore]  写回原值 + 重读校验, 写 restore.json
  farm health <serial>              在线 + 温度; 只有离线 (退 10) 或超温 (退 11) 才该中止
  farm task new|msg|note|result|check|handoff|ls …   任务目录协议 (_tmp/<任务>/)
  farm hook                         Claude Code PreToolUse 钩子: 拦绕过垫片直接调真 adb/hdc 的命令

设计说明在 hgamey/_tmp/harness/DESIGN.md。这里只放代码。
"""
from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import shlex
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent  # phonefarm 根
SHIM_DIR = HERE / "bin"

EXIT_DENY = 3  # 守卫拒绝
EXIT_OFFLINE = 10
EXIT_HOT = 11
HOT_C_DEFAULT = 70.0  # 用户定的: 只有离线或 >70°C 才中止


def now_iso() -> str:
    return _dt.datetime.now().astimezone().isoformat(timespec="seconds")


def farm_home() -> Path:
    return Path(os.environ.get("FARM_HOME", "/private/tmp/claude-501/farm"))


def safe_name(serial: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]", "_", serial)


def write_new(path: Path, text: str) -> None:
    """只新建: 已有同名文件先改名留底 (<名>.prev-<时间>), 绝不覆盖。"""
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        stamp = _dt.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
        path.rename(path.with_name(f"{path.name}.prev-{stamp}"))
    path.write_text(text)


def append(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as f:
        f.write(text)


# ---------------------------------------------------------------- 租约 (每台设备一把 devlock)

def lease_dir(serial: str) -> Path:
    return farm_home() / "lease" / safe_name(serial)


def devlock_bin() -> str:
    return os.environ.get("DEVLOCK_BIN") or str(REPO / "tools" / "devlock")


def lease_env(serial: str) -> dict:
    """持租约的 runner 跑 phonefarm / game_opt_loop 时也要带上这份环境:
    它们内部的 devlock 调用落到同一台设备的锁目录上, 认出是自己这一侧。"""
    return {"DEVLOCK_DIR": str(lease_dir(serial))}


def lease_cmd(op: str, serial: str, labels: list[str]) -> int:
    env = dict(os.environ, **lease_env(serial))
    return subprocess.call(["zsh", devlock_bin(), op, *labels], env=env)


def lease_holder(serial: str) -> str | None:
    """当前持有人 label; 心跳超过 STALE 视为没人持有 (与 devlock 的接管规则一致)。"""
    owner = lease_dir(serial) / "devlock.d" / "owner"
    try:
        text = owner.read_text().strip()
        age = _dt.datetime.now().timestamp() - owner.stat().st_mtime
    except OSError:
        return None
    if age > float(os.environ.get("DEVLOCK_STALE", "600")):
        return None
    return text.split(" ", 1)[0] or None


# ---------------------------------------------------------------- 真设备执行器

def real_tool(tool: str) -> str:
    """真 adb/hdc 的路径: FARM_REAL_ADB / FARM_REAL_HDC > PATH 上第一个不是垫片的。"""
    env = os.environ.get(f"FARM_REAL_{tool.upper()}")
    if env:
        return env
    for d in os.environ.get("PATH", "").split(os.pathsep):
        if not d or Path(d).resolve() == SHIM_DIR.resolve():
            continue
        p = Path(d) / tool
        if p.is_file() and os.access(p, os.X_OK):
            return str(p)
    raise SystemExit(f"farm: 找不到真 {tool} (设 FARM_REAL_{tool.upper()})")


def tool_of(serial: str) -> tuple[str, str]:
    """serial 写成 hdc:<key> 表示鸿蒙设备, 否则 adb。"""
    return ("hdc", serial[4:]) if serial.startswith("hdc:") else ("adb", serial)


def dev_shell(serial: str, cmd: str, timeout: int = 60) -> tuple[int, str]:
    tool, key = tool_of(serial)
    flag = "-t" if tool == "hdc" else "-s"
    try:
        p = subprocess.run([real_tool(tool), flag, key, "shell", cmd], capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return 124, ""
    return p.returncode, (p.stdout or "") + (p.stderr or "")


# ---------------------------------------------------------------- 键: 怎么读、怎么写回

KINDS = ("setting", "prop", "param", "sysfs", "devcfg", "ls")


def _su(cmd: str, root: bool) -> str:
    return f"su -c {shlex.quote(cmd)}" if root else cmd


def key_get_cmd(key: str, root: bool = False) -> str:
    kind, _, rest = key.partition(":")
    if kind == "setting":
        ns, _, k = rest.partition(":")
        return f"settings get {ns} {k}"
    if kind == "prop":
        return f"getprop {rest}"
    if kind == "param":
        return f"param get {rest}"
    if kind == "sysfs":
        return _su(f"cat {rest}", root)
    if kind == "devcfg":
        ns, _, k = rest.partition(":")
        return f"device_config get {ns} {k}"
    if kind == "ls":
        return _su(f"ls -1a {rest}", root)
    raise ValueError(f"不认识的键: {key}")


def key_put_cmd(key: str, value: str, root: bool = False) -> str | None:
    """写回原值的命令; None = 不能自动写回 (目录清单只能人工处理, 不删东西)。"""
    kind, _, rest = key.partition(":")
    q = shlex.quote(value)
    if kind == "setting":
        ns, _, k = rest.partition(":")
        return f"settings put {ns} {k} {q}"
    if kind == "prop":
        return f"setprop {rest} {q}"
    if kind == "param":
        return f"param set {rest} {q}"
    if kind == "sysfs":
        return _su(f"echo {q} > {rest}", root)
    if kind == "devcfg":
        ns, _, k = rest.partition(":")
        return f"device_config put {ns} {k} {q}"
    return None


def normalize(key: str, rc: int, out: str) -> str | None:
    """读数归一: None = 不存在或读不到。"""
    kind = key.partition(":")[0]
    out = out.replace("\r", "")
    if rc != 0:
        return None
    if kind == "ls":
        names = sorted(n for n in out.split("\n") if n.strip() and n.strip() not in (".", ".."))
        return "\n".join(names)
    v = out.strip()
    if kind in ("setting", "devcfg") and v == "null":
        return None
    if kind == "param" and ("fail" in v.lower() or "error" in v.lower()):
        return None
    return v


def read_key(serial: str, key: str, root: bool) -> str | None:
    rc, out = dev_shell(serial, key_get_cmd(key, root))
    return normalize(key, rc, out)


# ---------------------------------------------------------------- 守卫: 规则

NO_DEVICE_ADB = {"devices", "version", "--version", "help", "start-server", "connect", "disconnect", "get-serialno", "get-state", "wait-for-device"}
NO_DEVICE_HDC = {"list", "version", "-v", "help", "-h", "start", "tconn", "checkserver"}
DENY_ADB_SUB = {"uninstall": "卸载", "kill-server": "会断掉所有人的设备连接", "remount": "改系统分区",
                "sideload": "刷机", "disable-verity": "改系统分区", "reboot-bootloader": "进刷机模式"}
DENY_HDC_SUB = {"uninstall": "卸载", "kill": "会断掉所有人的设备连接"}

DENY_SHELL = [
    (r"(^|[;&|`(]\s*|\s)(rm|rmdir|unlink|truncate|shred)\s", "删除/截断文件"),
    (r"\bpm\s+(clear|uninstall|reset-permissions|remove-user)\b", "清数据/卸载/删用户"),
    (r"\bcmd\s+package\s+(clear|uninstall)\b", "清数据/卸载"),
    (r"\bsettings\s+(delete|reset)\b", "删除/重置设置"),
    (r"\bdevice_config\s+(delete|reset)\b", "删除/重置设置"),
    (r"\bbm\s+(uninstall|clean)\b", "鸿蒙卸载/清数据"),
    (r"\bparam\s+(restore|erase)\b", "重置系统参数"),
    (r"\b(wipe|mkfs\S*|fastboot)\b", "擦除/刷机"),
    (r"\breboot\s+(recovery|bootloader|edl)\b", "进刷机模式"),
    (r"\b(log-?out|sign-?out)\b", "登出账号"),
    (r"\bam\s+\S+.*\b(LOGOUT|SIGN_OUT|REMOVE_ACCOUNT)\b", "登出账号"),
]
# 单个 > 会截断目标; 只允许写这几处 (sysfs/procfs 是写设置, /dev/null, 我们自己的临时目录)
REDIRECT_OK = ("/sys/", "/proc/", "/dev/null", "/data/local/tmp/")

# 改设置的命令 → 键 (守卫据此先快照再放行)
MUTATIONS = [
    (r"\bsettings\s+put\s+(system|global|secure)\s+(\S+)", lambda m: f"setting:{m[1]}:{m[2]}"),
    (r"\b(?:setprop|resetprop)\s+(?:-\w+\s+)*([\w.\-]+)", lambda m: f"prop:{m[1]}"),
    (r"\bparam\s+set\s+(\S+)", lambda m: f"param:{m[1]}"),
    (r"\bdevice_config\s+put\s+(\S+)\s+(\S+)", lambda m: f"devcfg:{m[1]}:{m[2]}"),
    (r"(?<![0-9&>])>>?\s*['\"]?((?:/sys|/proc/sys)/[^\s'\";&|]+)", lambda m: f"sysfs:{m[1]}"),
]
# 也会改现场、但没有通用读写对的命令: 放行, 记成「未覆盖的改动」, job end 时一律算没恢复干净
RAW_MUTATIONS = [r"\bcmd\s+game\s+(set|mode|reset)\b", r"\bwm\s+(size|density)\s+\S", r"\bsvc\s+\w+\s+\w+",
                 r"\bcmd\s+(display|thermalservice|power)\s+\S+", r"\bpower\s+-\w+"]


def unwrap_su(cmd: str) -> str:
    m = re.match(r"^\s*su\s+(?:0\s+|root\s+)?-c\s+(.*)$", cmd, re.S)
    if not m:
        return cmd
    try:
        return " ".join(shlex.split(m[1]))
    except ValueError:
        return m[1]


def shell_violations(cmd: str) -> list[str]:
    c = unwrap_su(cmd)
    bad = [why for pat, why in DENY_SHELL if re.search(pat, c, re.I)]
    for m in re.finditer(r"(?<![0-9&>])>(?!>|&)\s*['\"]?([^\s'\";&|)]+)", c):
        tgt = m[1]
        if not tgt.startswith(REDIRECT_OK):
            bad.append(f"> 会覆盖 {tgt} (用 >> 追加, 或写到 /data/local/tmp/ 下的新文件)")
    return bad


def shell_mutations(cmd: str) -> tuple[list[str], list[str]]:
    c = unwrap_su(cmd)
    keys = []
    for pat, mk in MUTATIONS:
        for m in re.finditer(pat, c):
            k = mk(m)
            if k not in keys:
                keys.append(k)
    raw = [m[0] for pat in RAW_MUTATIONS for m in re.finditer(pat, c)]
    return keys, raw


def parse_argv(tool: str, argv: list[str]) -> tuple[str | None, str | None, list[str]]:
    """→ (serial 或 None, 子命令, 子命令之后的参数)。"""
    serial, i = None, 0
    if tool == "adb":
        serial = os.environ.get("ANDROID_SERIAL")
        while i < len(argv) and argv[i].startswith("-") and argv[i] not in NO_DEVICE_ADB:
            if argv[i] in ("-s", "-t", "-H", "-P", "-L") and i + 1 < len(argv):
                if argv[i] == "-s":
                    serial = argv[i + 1]
                i += 2
            else:
                i += 1
    else:
        while i < len(argv) and argv[i].startswith("-") and argv[i] not in NO_DEVICE_HDC:
            if argv[i] in ("-t", "-s", "-p") and i + 1 < len(argv):
                if argv[i] == "-t":
                    serial = "hdc:" + argv[i + 1]
                i += 2
            else:
                i += 1
    sub = argv[i] if i < len(argv) else None
    return serial, sub, argv[i + 1:]


def job_path(serial: str) -> Path:
    return farm_home() / "job" / f"{safe_name(serial)}.json"


def load_job(serial: str) -> dict | None:
    try:
        return json.loads(job_path(serial).read_text())
    except (OSError, ValueError):
        return None


def save_job(serial: str, job: dict) -> None:
    p = job_path(serial)
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps(job, ensure_ascii=False, indent=1))
    tmp.replace(p)  # 自己的状态文件, 原子替换


def guard_decide(tool: str, argv: list[str]) -> tuple[bool, str, str | None, str | None]:
    """→ (放行?, 理由, serial, shell 命令或 None)。纯规则, 不碰设备。"""
    serial, sub, rest = parse_argv(tool, argv)
    no_dev = NO_DEVICE_ADB if tool == "adb" else NO_DEVICE_HDC
    if sub is None or sub in no_dev:
        return True, "不碰具体设备", serial, None
    deny_sub = DENY_ADB_SUB if tool == "adb" else DENY_HDC_SUB
    if sub in deny_sub:
        return False, f"{tool} {sub}: {deny_sub[sub]}", serial, None
    if serial is None:
        return False, f"没指定设备 ({'-s <serial> 或 ANDROID_SERIAL' if tool == 'adb' else '-t <key>'}); 多机农场上不许让工具自己挑", None, None
    role = os.environ.get("FARM_ROLE", "")
    label = os.environ.get("FARM_AGENT", "")
    if role != "runner":
        return False, f"FARM_ROLE={role or '<空>'}: 只有设备 runner 能碰设备 (研究/代码/评审 agent 一律不碰)", serial, None
    holder = lease_holder(serial)
    if not label or holder != label:
        return False, f"{serial} 的租约在 {holder or '<没人>'} 手里, 你是 {label or '<没设 FARM_AGENT>'}", serial, None
    shell = None
    if sub in ("shell", "exec-out"):
        if not rest:
            return False, "交互 shell 检查不了, 请把命令写在参数里", serial, None
        shell = " ".join(rest)
        bad = shell_violations(shell)
        if bad:
            return False, "; ".join(bad), serial, shell
    if tool == "adb" and sub == "reboot" and rest and rest[0] in ("recovery", "bootloader", "edl", "sideload"):
        return False, "进刷机模式", serial, None
    return True, "ok", serial, shell


def guard(tool: str, argv: list[str]) -> int:
    ok, why, serial, shell = guard_decide(tool, argv)
    log = farm_home() / "guard.log"
    if not ok:
        append(log, f"{now_iso()} DENY {os.environ.get('FARM_AGENT', '-')} {tool} {shlex.join(argv)} :: {why}\n")
        print(f"farm guard: 拒绝 —— {why}", file=sys.stderr)
        return EXIT_DENY
    if shell is not None and serial is not None:
        keys, raw = shell_mutations(shell)
        if keys or raw:
            job = load_job(serial)
            if job is None:
                print(f"farm guard: 拒绝 —— 这条命令会改设置 ({', '.join(keys + raw)}), 先 `farm job begin {serial}` 快照", file=sys.stderr)
                append(log, f"{now_iso()} DENY-NOJOB {tool} {shlex.join(argv)}\n")
                return EXIT_DENY
            root = bool(job.get("root"))
            for k in keys:  # 第一次改到的键: 先读原值记进快照, 再放行
                if k not in job["before"]:
                    job["before"][k] = read_key(serial, k, root)
                    job.setdefault("lazy", []).append(k)
            for r in raw:
                if r not in job.setdefault("raw", []):
                    job["raw"].append(r)
            job.setdefault("journal", []).append({"t": now_iso(), "cmd": shell, "keys": keys, "raw": raw})
            save_job(serial, job)
    append(log, f"{now_iso()} ok {os.environ.get('FARM_AGENT', '-')} {tool} {shlex.join(argv)}\n")
    os.execv(real_tool(tool), [tool, *argv])
    return 0  # 不会到这


# ---------------------------------------------------------------- 快照 / 恢复校验

def profile_keys(serial: str, extra: list[str]) -> tuple[list[str], list[str], bool]:
    """→ (要快照的键, 允许自己漂移的键, 是否 root)。profiles.json: common.<adb|hdc> + devices.<serial>。"""
    prof = json.loads((HERE / "profiles.json").read_text())
    tool, _ = tool_of(serial)
    keys = list(prof.get("common", {}).get(tool, []))
    dev = prof.get("devices", {}).get(serial, {})
    keys += dev.get("keys", [])
    keys += extra
    seen, out = set(), []
    for k in keys:
        if k.partition(":")[0] not in KINDS:
            raise SystemExit(f"不认识的键: {k}")
        if k not in seen:
            seen.add(k)
            out.append(k)
    return out, dev.get("volatile", []), bool(dev.get("root", False))


def job_begin(serial: str, label: str, task: str | None, extra: list[str]) -> int:
    if lease_holder(serial) != label:
        print(f"farm job begin: {serial} 的租约不在 {label} 手里 (先 farm lease acquire)", file=sys.stderr)
        return EXIT_DENY
    old = load_job(serial)
    if old is not None:
        print(f"farm job begin: {serial} 上还有没收尾的作业 ({old.get('label')}, {old.get('begun')}); "
              f"先 `farm job end {serial} --label {old.get('label')}` 校验恢复", file=sys.stderr)
        return EXIT_DENY
    keys, volatile, root = profile_keys(serial, extra)
    before = {k: read_key(serial, k, root) for k in keys}
    job = {"serial": serial, "label": label, "task": task, "begun": now_iso(), "root": root,
           "volatile": volatile, "before": before, "lazy": [], "raw": [], "journal": []}
    save_job(serial, job)
    if task:
        write_new(Path(task) / f"snapshot-before-{safe_name(serial)}.json", json.dumps(job, ensure_ascii=False, indent=1))
    print(f"快照 {len(keys)} 个键 → {job_path(serial)}")
    return 0


def job_end(serial: str, label: str, task: str | None, restore: bool = True) -> int:
    job = load_job(serial)
    if job is None:
        print(f"farm job end: {serial} 没有进行中的作业", file=sys.stderr)
        return 1
    if job["label"] != label:
        print(f"farm job end: 作业属于 {job['label']}, 不是 {label}", file=sys.stderr)
        return EXIT_DENY
    root = bool(job.get("root"))
    restored, failed, unresolved = [], [], []
    if restore:
        for k, orig in job["before"].items():
            if k in job.get("volatile", []):
                continue
            cur = read_key(serial, k, root)
            if cur == orig:
                continue
            put = key_put_cmd(k, orig, root) if orig is not None else None
            if put is None:
                why = "原来不存在, 写回需要删除键, 按规矩不删" if orig is None else "目录清单不能自动恢复 (不删文件; 外来文件请改名停放)"
                unresolved.append({"key": k, "before": orig, "now": cur, "why": why})
                continue
            rc, out = dev_shell(serial, put)
            (restored if rc == 0 else failed).append({"key": k, "from": cur, "to": orig, "rc": rc, "out": out.strip()[:200]})
    diffs, drift = [], []
    for k, orig in job["before"].items():
        cur = read_key(serial, k, root)
        if cur != orig:
            (drift if k in job.get("volatile", []) else diffs).append({"key": k, "before": orig, "now": cur})
    for r in job.get("raw", []):
        unresolved.append({"key": f"raw:{r}", "why": "这类改动没有通用读写对, 守卫记下了但无法校验, 需要 runner 在报告里说明怎么还原的"})
    unresolved_keys = {u["key"] for u in unresolved}
    ok = not diffs and not failed and not unresolved
    res = {"serial": serial, "label": label, "task": job.get("task") or task, "begun": job["begun"], "ended": now_iso(),
           "keys": len(job["before"]), "lazy_keys": job.get("lazy", []), "journal": job.get("journal", []),
           "restored": restored, "restore_failed": failed,
           "still_different": [d for d in diffs if d["key"] not in unresolved_keys], "unresolved": unresolved,
           "volatile_drift": drift, "ok": ok}
    out_dir = Path(task or job.get("task") or (farm_home() / "restore"))
    write_new(out_dir / f"restore-{safe_name(serial)}.json", json.dumps(res, ensure_ascii=False, indent=1))
    # 作业文件改名归档 (不删), 设备回到「没有进行中作业」
    arch = farm_home() / "job" / "done" / f"{safe_name(serial)}-{_dt.datetime.now().strftime('%Y%m%d-%H%M%S-%f')}.json"
    arch.parent.mkdir(parents=True, exist_ok=True)
    job_path(serial).rename(arch)
    print(json.dumps({"ok": ok, "restored": len(restored), "still_different": len(res["still_different"]),
                      "unresolved": len(unresolved), "restore_failed": len(failed)}, ensure_ascii=False))
    return 0 if ok else 1


# ---------------------------------------------------------------- 健康: 只有离线 / 超温才中止

def health(serial: str, hot_c: float) -> int:
    tool, key = tool_of(serial)
    if tool == "adb":
        p = subprocess.run([real_tool("adb"), "-s", key, "get-state"], capture_output=True, text=True)
        online = p.returncode == 0 and p.stdout.strip() == "device"
    else:
        p = subprocess.run([real_tool("hdc"), "list", "targets"], capture_output=True, text=True)
        online = key in p.stdout.split()
    temp = None
    if online:
        cmd = "dumpsys battery" if tool == "adb" else "hidumper -s BatteryService -a -i"
        rc, out = dev_shell(serial, cmd)
        m = re.search(r"temperature\s*[:=]\s*(-?\d+)", out, re.I)
        if m:
            temp = int(m[1]) / 10.0  # 两边都是 0.1°C 单位 (鸿蒙这一侧未在真机核实)
    print(json.dumps({"serial": serial, "online": online, "battery_c": temp, "hot_c": hot_c}, ensure_ascii=False))
    if not online:
        return EXIT_OFFLINE
    if temp is not None and temp > hot_c:
        return EXIT_HOT
    return 0


# ---------------------------------------------------------------- 任务文件协议

ROLES = ("runner", "research", "impl", "review")
STATUSES = ("done", "partial", "failed", "aborted")


def task_new(d: Path, tid: str, role: str, device: str | None, repo: str | None, branch: str | None) -> int:
    if role not in ROLES:
        raise SystemExit(f"role 只能是 {ROLES}")
    if role == "runner" and not device:
        raise SystemExit("runner 任务必须带 --device")
    if role != "runner" and device:
        raise SystemExit("只有 runner 任务能带设备")
    d.mkdir(parents=True, exist_ok=True)
    task = {"id": tid, "role": role, "device": device, "repo": repo, "branch": branch, "created": now_iso()}
    write_new(d / "TASK.json", json.dumps(task, ensure_ascii=False, indent=1))
    for name in ("BRIEF.md", "MESSAGES.md", "PROGRESS.md"):
        if not (d / name).exists():
            (d / name).write_text("")
    print(d)
    return 0


def task_append(d: Path, name: str, text: str) -> int:
    append(d / name, f"--- {now_iso()}\n{text.rstrip()}\n")
    return 0


def task_result(d: Path, status: str, summary: str, commits: list[str], tests: list[str], open_items: list[str]) -> int:
    if status not in STATUSES:
        raise SystemExit(f"status 只能是 {STATUSES}")
    cs = []
    for c in commits:
        parts = c.split(":")
        if len(parts) != 3:
            raise SystemExit(f"--commit 写成 <仓库路径>:<分支>:<提交号>: {c}")
        cs.append({"repo": parts[0], "branch": parts[1], "sha": parts[2]})
    ts = []
    for t in tests:
        m = re.match(r"^(.*)\|(\d+)\|(\d+)$", t)
        if not m:
            raise SystemExit(f"--tests 写成 <命令>|<通过数>|<失败数>: {t}")
        ts.append({"cmd": m[1], "passed": int(m[2]), "failed": int(m[3])})
    res = {"status": status, "summary": summary, "commits": cs, "tests": ts, "open": open_items, "at": now_iso()}
    write_new(d / "RESULT.json", json.dumps(res, ensure_ascii=False, indent=1))
    return 0


def task_problems(d: Path) -> list[str]:
    """主控收到完成通知后跑一次: 有问题就打回, 不靠读 agent 的自述。"""
    probs = []
    try:
        task = json.loads((d / "TASK.json").read_text())
    except (OSError, ValueError) as e:
        return [f"TASK.json 读不了: {e}"]
    rp = d / "REPORT.md"
    if not rp.exists() or not rp.read_text().strip():
        probs.append("REPORT.md 缺失或为空")
    try:
        res = json.loads((d / "RESULT.json").read_text())
    except (OSError, ValueError) as e:
        return probs + [f"RESULT.json 读不了: {e}"]
    if res.get("status") not in STATUSES:
        probs.append(f"RESULT.status 非法: {res.get('status')}")
    for t in res.get("tests", []):
        if t.get("failed", 0) > 0 and res.get("status") == "done":
            probs.append(f"测试有失败却标 done: {t.get('cmd')}")
    for c in res.get("commits", []):
        r = subprocess.run(["git", "-C", c["repo"], "cat-file", "-e", f"{c['sha']}^{{commit}}"], capture_output=True)
        if r.returncode != 0:
            probs.append(f"提交不存在: {c['repo']} {c['sha']}")
            continue
        r = subprocess.run(["git", "-C", c["repo"], "merge-base", "--is-ancestor", c["sha"], c["branch"]], capture_output=True)
        if r.returncode != 0:
            probs.append(f"提交 {c['sha']} 不在分支 {c['branch']} 上")
        r = subprocess.run(["git", "-C", c["repo"], "branch", "-r", "--contains", c["sha"]], capture_output=True, text=True)
        if r.stdout.strip() and task.get("role") != "runner":
            pass  # 已在远端只说明它是旧提交; push 与否由 git 钩子/权限管, 这里不判
    if task.get("role") == "runner":
        dev = task.get("device") or ""
        rf = d / f"restore-{safe_name(dev)}.json"
        if not rf.exists():
            probs.append(f"runner 任务缺 {rf.name} (收尾没跑 farm job end)")
        else:
            rr = json.loads(rf.read_text())
            if not rr.get("ok"):
                n = len(rr.get("still_different", [])) + len(rr.get("unresolved", [])) + len(rr.get("restore_failed", []))
                probs.append(f"设备现场没恢复干净: {n} 项, 见 {rf.name}")
        if (farm_home() / "job" / f"{safe_name(dev)}.json").exists():
            probs.append(f"{dev} 上还挂着没收尾的作业")
    return probs


def task_check(d: Path) -> int:
    probs = task_problems(d)
    print(json.dumps({"task": str(d), "ok": not probs, "problems": probs}, ensure_ascii=False, indent=1))
    return 0 if not probs else 1


def task_handoff(d: Path, tail: int) -> int:
    """重启一个 agent 时喂给它的上下文: 原始 brief + 主控的全部消息 + 最近几条进度。"""
    rd = lambda n: (d / n).read_text() if (d / n).exists() else ""
    notes = [b for b in rd("PROGRESS.md").split("--- ") if b.strip()]
    out = ["# ORIGINAL BRIEF\n", rd("BRIEF.md").strip(), "\n\n# TASK\n", rd("TASK.json").strip(),
           "\n\n# ALL MESSAGES FROM COORDINATOR (chronological)\n", rd("MESSAGES.md").strip(),
           f"\n\n# LAST {tail} PROGRESS NOTES\n", "".join("--- " + b for b in notes[-tail:]).strip()]
    task = json.loads(rd("TASK.json") or "{}")
    if task.get("role") == "runner":
        job = load_job(task.get("device") or "")
        out.append("\n\n# DEVICE STATE\n" + (f"作业进行中 (开始于 {job['begun']}, 已改动键 {job.get('lazy', [])})。先核对现场再继续。"
                                             if job else "没有进行中的作业。"))
    print("".join(out))
    return 0


def task_ls(root: Path) -> int:
    rows = []
    for t in sorted(root.glob("*/TASK.json")) + sorted(root.glob("*/*/TASK.json")):
        d = t.parent
        try:
            task = json.loads(t.read_text())
            res = json.loads((d / "RESULT.json").read_text()) if (d / "RESULT.json").exists() else {}
        except ValueError:
            continue
        rows.append(f"{task.get('id', '?'):24} {task.get('role', '?'):8} {task.get('device') or '-':20} {res.get('status', 'running'):8} {d}")
    print("\n".join(rows))
    return 0


# ---------------------------------------------------------------- Claude Code 钩子

REAL_TOOL_RE = re.compile(r"(/opt/homebrew/bin/|platform-tools/|ohos-sdk/bin/|/usr/local/bin/)(adb|hdc)\b")


def hook(stdin_text: str) -> int:
    """PreToolUse(Bash): 命令里直接写真 adb/hdc 路径 = 绕过垫片, 拒绝。其余交给垫片判。"""
    try:
        ev = json.loads(stdin_text)
    except ValueError:
        return 0
    if ev.get("tool_name") != "Bash":
        return 0
    cmd = (ev.get("tool_input") or {}).get("command", "")
    if REAL_TOOL_RE.search(cmd) or re.search(r"FARM_REAL_(ADB|HDC)=", cmd):
        print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny",
                                                 "permissionDecisionReason": "直接调真 adb/hdc 绕过了 farm 守卫; 用 PATH 上的 adb/hdc (tools/farm/bin)"}},
                         ensure_ascii=False))
    return 0


# ---------------------------------------------------------------- CLI

def main(argv: list[str]) -> int:
    if len(argv) >= 1 and argv[0] == "guard":
        if len(argv) < 2 or argv[1] not in ("adb", "hdc"):
            raise SystemExit("farm guard adb|hdc <argv…>")
        return guard(argv[1], argv[2:])
    ap = argparse.ArgumentParser(prog="farm")
    sp = ap.add_subparsers(dest="cmd", required=True)
    p = sp.add_parser("lease")
    p.add_argument("op", choices=["acquire", "renew", "release", "status", "queue"])
    p.add_argument("serial")
    p.add_argument("labels", nargs="*")
    p = sp.add_parser("job")
    p.add_argument("op", choices=["begin", "end", "show"])
    p.add_argument("serial")
    p.add_argument("--label", default=os.environ.get("FARM_AGENT", ""))
    p.add_argument("--task")
    p.add_argument("--keys", nargs="*", default=[])
    p.add_argument("--no-restore", action="store_true")
    p = sp.add_parser("health")
    p.add_argument("serial")
    p.add_argument("--hot-c", type=float, default=HOT_C_DEFAULT)
    p = sp.add_parser("task")
    tp = p.add_subparsers(dest="op", required=True)
    q = tp.add_parser("new"); q.add_argument("dir"); q.add_argument("--id", required=True); q.add_argument("--role", required=True)
    q.add_argument("--device"); q.add_argument("--repo"); q.add_argument("--branch")
    for n in ("msg", "note"):
        q = tp.add_parser(n); q.add_argument("dir"); q.add_argument("text", nargs="?")
    q = tp.add_parser("result"); q.add_argument("dir"); q.add_argument("--status", required=True); q.add_argument("--summary", default="")
    q.add_argument("--commit", action="append", default=[]); q.add_argument("--tests", action="append", default=[]); q.add_argument("--open", action="append", default=[])
    q = tp.add_parser("check"); q.add_argument("dir")
    q = tp.add_parser("handoff"); q.add_argument("dir"); q.add_argument("--tail", type=int, default=5)
    q = tp.add_parser("ls"); q.add_argument("root")
    sp.add_parser("hook")
    a = ap.parse_args(argv)
    if a.cmd == "lease":
        if a.op in ("acquire", "renew", "release") and len(a.labels) != 1:
            raise SystemExit(f"farm lease {a.op} <serial> <label>")
        return lease_cmd(a.op, a.serial, a.labels)
    if a.cmd == "job":
        if a.op == "show":
            print(json.dumps(load_job(a.serial), ensure_ascii=False, indent=1))
            return 0
        if not a.label:
            raise SystemExit("--label 或 FARM_AGENT 必须给")
        if a.op == "begin":
            return job_begin(a.serial, a.label, a.task, a.keys)
        return job_end(a.serial, a.label, a.task, restore=not a.no_restore)
    if a.cmd == "health":
        return health(a.serial, a.hot_c)
    if a.cmd == "task":
        d = Path(a.dir) if a.op != "ls" else Path(a.root)
        if a.op == "new":
            return task_new(d, a.id, a.role, a.device, a.repo, a.branch)
        if a.op in ("msg", "note"):
            text = a.text if a.text is not None else sys.stdin.read()
            return task_append(d, "MESSAGES.md" if a.op == "msg" else "PROGRESS.md", text)
        if a.op == "result":
            return task_result(d, a.status, a.summary, a.commit, a.tests, a.open)
        if a.op == "check":
            return task_check(d)
        if a.op == "handoff":
            return task_handoff(d, a.tail)
        return task_ls(d)
    if a.cmd == "hook":
        return hook(sys.stdin.read())
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
