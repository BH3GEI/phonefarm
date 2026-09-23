#!/usr/bin/env python3
"""whitelist.py — 「哪些系统参数准改、准改成什么」的唯一裁决处 (纯函数)。

白名单不是写死的常量表, 而是 `probe_sysparam.sh` 真机探测结果的函数:
每个候选参数要同时满足「节点存在 + 可写 + 写了真生效」才进白名单, 三关缺一不可。
探不到的、写了被内核退回的, 一律不进 —— 宁可少改, 不可假改。

另有一条不看探测结果的硬规矩: **温控保护相关的任何节点永远不进白名单**。
关热保护、抬温控阈值能立刻换来漂亮数字, 但那是拿硬件安全换指标, 不是优化。
DENY_KEYWORDS 在这里拦一道, `knob_sysparam.sh` 在设备端再拦一道。

全部是纯函数: 输入 probe 文本, 输出白名单与校验结论, 不读设备、不读时钟。
"""
from __future__ import annotations

# 温控保护关键词。命中即拒, 不看探测结果, 不看大模型怎么说。
DENY_KEYWORDS = ("thermal", "trip_point", "cooling", "fan", "tsens", "bcl", "throttl")

# 生效判定: 只有这两种结论算「写了真生效」
EFFECT_OK = ("live",)
# 部分节点当前值已经是探测值, 无法做差异写测试 —— 可写 + 有合法取值表即可进,
# 每轮还有 snap_at_run.txt 复核实际生效, 所以不会放过假生效。
EFFECT_SOFT = ("skipped_same_value",)


def parse_probe(text: str) -> dict:
    """probe_sysparam.sh 的 key=value 文本 → dict。`#` 开头是注释。"""
    out: dict[str, str] = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            continue
        k, v = line.split("=", 1)
        out[k.strip()] = v.strip()
    return out


def _ints(s: str) -> list[int]:
    vals = []
    for tok in s.replace(",", " ").split():
        try:
            vals.append(int(tok))
        except ValueError:
            pass
    return sorted(set(vals))


def _denied(path: str) -> bool:
    low = path.lower()
    return any(k in low for k in DENY_KEYWORDS)


def _usable(probe: dict, key: str) -> bool:
    """可写 + (生效测试通过 或 无法做差异测试)。"""
    if probe.get(f"{key}.writable") != "yes":
        return False
    eff = probe.get(f"{key}.effect")
    if eff is None:
        return True  # 没做差异测试 (当前值即唯一可试值), 交给轮内快照复核
    return eff in EFFECT_OK or eff in EFFECT_SOFT


def build_whitelist(probe: dict) -> dict:
    """probe dict → {param_id: spec}。spec 描述这个参数准写哪些值。

    spec 字段:
      kind    : "sysfs" | "setting"
      path    : 设备端路径 (sysfs 绝对路径, 或 "<ns>:<key>")
      values  : 允许的取值列表 (字符串), 大模型只能从里面挑
      current : 探测时的原值
      group   : 用于跨项约束检查 (同 group 的 min/max 要成对合法)
    """
    wl: dict[str, dict] = {}

    def add(pid, kind, path, values, current, group=None):
        if _denied(path) or _denied(pid):
            return
        values = [str(v) for v in values if str(v) != ""]
        if len(values) < 2:
            return
        wl[pid] = {
            "kind": kind,
            "path": path,
            "values": values,
            "current": str(current),
            "group": group,
        }

    # ── CPU 各簇 ──
    for pol in (probe.get("cpu.policies") or "").split(","):
        pol = pol.strip()
        if not pol:
            continue
        k = f"cpu.policy{pol}"
        base = f"/sys/devices/system/cpu/cpufreq/policy{pol}"
        freqs = _ints(probe.get(f"{k}.avail_freqs", ""))
        if freqs:
            if _usable(probe, f"{k}.scaling_min_freq"):
                add(f"{k}.scaling_min_freq", "sysfs", f"{base}/scaling_min_freq",
                    freqs, probe.get(f"{k}.scaling_min_freq.cur", ""), group=f"{k}.freq")
            if _usable(probe, f"{k}.scaling_max_freq"):
                add(f"{k}.scaling_max_freq", "sysfs", f"{base}/scaling_max_freq",
                    freqs, probe.get(f"{k}.scaling_max_freq.cur", ""), group=f"{k}.freq")
        govs = (probe.get(f"{k}.avail_governors") or "").split()
        if govs and _usable(probe, f"{k}.scaling_governor"):
            add(f"{k}.scaling_governor", "sysfs", f"{base}/scaling_governor",
                govs, probe.get(f"{k}.scaling_governor.cur", ""))

    # ── GPU ──
    kgsl = "/sys/class/kgsl/kgsl-3d0"
    try:
        nlv = int(probe.get("gpu.num_pwrlevels", ""))
    except (TypeError, ValueError):
        nlv = 0
    if nlv > 1:
        # kgsl 语义: 0 = 最快档, 数字越大越慢。min_pwrlevel 是「最慢准到哪」,
        # max_pwrlevel 是「最快准到哪」, 因此恒有 max_pwrlevel <= min_pwrlevel。
        lv = list(range(nlv))
        if _usable(probe, "gpu.min_pwrlevel"):
            add("gpu.min_pwrlevel", "sysfs", f"{kgsl}/min_pwrlevel", lv,
                probe.get("gpu.min_pwrlevel.cur", ""), group="gpu.pwrlevel")
        if _usable(probe, "gpu.max_pwrlevel"):
            add("gpu.max_pwrlevel", "sysfs", f"{kgsl}/max_pwrlevel", lv,
                probe.get("gpu.max_pwrlevel.cur", ""), group="gpu.pwrlevel")
    gfreqs = _ints(probe.get("gpu.devfreq.avail_freqs", ""))
    if gfreqs:
        if _usable(probe, "gpu.devfreq.min_freq"):
            add("gpu.devfreq.min_freq", "sysfs", f"{kgsl}/devfreq/min_freq", gfreqs,
                probe.get("gpu.devfreq.min_freq.cur", ""), group="gpu.devfreq")
        if _usable(probe, "gpu.devfreq.max_freq"):
            add("gpu.devfreq.max_freq", "sysfs", f"{kgsl}/devfreq/max_freq", gfreqs,
                probe.get("gpu.devfreq.max_freq.cur", ""), group="gpu.devfreq")
    ggov = (probe.get("gpu.devfreq.avail_governors") or "").split()
    if ggov and _usable(probe, "gpu.devfreq.governor"):
        add("gpu.devfreq.governor", "sysfs", f"{kgsl}/devfreq/governor", ggov,
            probe.get("gpu.devfreq.governor.cur", ""))

    # ── 总线 DDR / LLCC 下限 (已验证过的那一条就在这里) ──
    for n in ("DDR", "LLCC"):
        key = f"bus.{n}.boost_freq"
        if not _usable(probe, key):
            continue
        vals = _ints(probe.get(f"bus.{n}.avail_freqs", ""))
        if not vals:
            lo = probe.get(f"bus.{n}.hw_min_freq", "")
            hi = probe.get(f"bus.{n}.hw_max_freq", "")
            vals = _ints(f"{lo} {hi}")
        add(key, "sysfs", f"/sys/devices/system/cpu/bus_dcvs/{n}/boost_freq", vals,
            probe.get(f"bus.{n}.boost_freq.cur", ""))

    # ── 刷新率 ──
    modes = sorted({int(float(x.split()[0]))
                    for x in (probe.get("display.modes") or "").split(",") if x.strip()})
    for pid, skey in (("setting.system.peak_refresh_rate", "peak_refresh_rate"),
                      ("setting.system.min_refresh_rate", "min_refresh_rate")):
        cur = probe.get(f"setting.system.{skey}.cur", "")
        if cur in ("", "null") or not modes:
            continue
        add(pid, "setting", f"system:{skey}", modes, cur, group="refresh")

    return wl


def validate_candidate(cand: dict, wl: dict) -> tuple[bool, str]:
    """校验大模型挑的一组参数。返回 (是否合法, 原因)。

    三层检查, 任何一层不过整组作废 —— 不做「剔掉违规项后凑合跑」,
    因为那样跑出来的数据对应的不是模型提出的那组参数, 证据会对不上账。
    """
    if not isinstance(cand, dict) or not cand:
        return False, "空参数组"
    for pid, val in cand.items():
        if pid not in wl:
            return False, f"{pid} 不在白名单"
        if _denied(pid) or _denied(wl[pid]["path"]):
            return False, f"{pid} 命中温控保护关键词"
        if str(val) not in wl[pid]["values"]:
            return False, f"{pid}={val} 不在合法取值表 {wl[pid]['values'][:6]}..."

    # 跨项约束: CPU 频率 min <= max
    for pid in list(cand):
        if pid.endswith(".scaling_min_freq"):
            mx_id = pid.replace(".scaling_min_freq", ".scaling_max_freq")
            mn = int(cand[pid])
            mx = int(cand.get(mx_id, wl.get(mx_id, {}).get("current") or 1 << 62))
            if mn > mx:
                return False, f"{pid}={mn} 超过同簇 max={mx}"
    # GPU pwrlevel: 0 最快, 恒有 max_pwrlevel <= min_pwrlevel
    if "gpu.max_pwrlevel" in cand or "gpu.min_pwrlevel" in cand:
        mx = int(cand.get("gpu.max_pwrlevel", wl.get("gpu.max_pwrlevel", {}).get("current") or 0))
        mn = int(cand.get("gpu.min_pwrlevel", wl.get("gpu.min_pwrlevel", {}).get("current") or 0))
        if mx > mn:
            return False, f"gpu.max_pwrlevel={mx} 必须 <= gpu.min_pwrlevel={mn} (0 是最快档)"
    # GPU devfreq min <= max
    if "gpu.devfreq.min_freq" in cand or "gpu.devfreq.max_freq" in cand:
        mn = int(cand.get("gpu.devfreq.min_freq",
                          wl.get("gpu.devfreq.min_freq", {}).get("current") or 0))
        mx = int(cand.get("gpu.devfreq.max_freq",
                          wl.get("gpu.devfreq.max_freq", {}).get("current") or 1 << 62))
        if mn > mx:
            return False, f"gpu.devfreq.min_freq={mn} 超过 max={mx}"
    # 刷新率 min <= peak
    if "setting.system.min_refresh_rate" in cand or "setting.system.peak_refresh_rate" in cand:
        mn = float(cand.get("setting.system.min_refresh_rate",
                            wl.get("setting.system.min_refresh_rate", {}).get("current") or 0))
        pk = float(cand.get("setting.system.peak_refresh_rate",
                            wl.get("setting.system.peak_refresh_rate", {}).get("current") or 1e9))
        if mn > pk:
            return False, f"min_refresh_rate={mn} 超过 peak={pk}"
    return True, "ok"


def plan_text(cand: dict, wl: dict) -> str:
    """一组参数 → 设备端 plan 文件文本。

    输出按 param_id 排序, 所以同一组参数每次生成逐字节相同 (证据可比对)。
    写入顺序里把「放宽上限」排在「抬高下限」之前, 少踩一次内核的 min<=max 夹取;
    knob_sysparam.sh 另有两遍写兜底。
    """
    def rank(pid: str) -> tuple[int, str]:
        if pid.endswith("max_freq") or pid.endswith("max_pwrlevel") or "peak_refresh" in pid:
            return (0, pid)
        return (1, pid)

    lines = ["# loop_v1 sysparam plan"]
    for pid in sorted(cand, key=rank):
        spec = wl[pid]
        lines.append(f"{spec['kind']}\t{spec['path']}\t{cand[pid]}")
    return "\n".join(lines) + "\n"


def describe_whitelist(wl: dict) -> str:
    """给大模型看的白名单说明 (紧凑, 取值表超过 8 个时只给首尾与档数)。"""
    out = []
    for pid in sorted(wl):
        s = wl[pid]
        v = s["values"]
        vs = ", ".join(v) if len(v) <= 8 else f"{v[0]} .. {v[-1]} (共 {len(v)} 档: {', '.join(v[:3])}, ...)"
        out.append(f"- {pid} (当前 {s['current']}) 可选: {vs}")
    return "\n".join(out)
