#!/usr/bin/env python3
"""llm.py — 让大模型从白名单里挑下一组系统参数。

与 game_opt_loop 的 `src/engine/generator.rs` 同一套约束, 只是换成 Python:

1. **密钥只读不带走**。从 `secrets.env` 读, 只用于 Authorization 头, 不进日志、
   不进 prompt、不进任何归档文件。
2. **失败必须可降级**。LLM 会超时、限流、空回包, 任何失败都只让这一代作废,
   由本地变异器补齐 —— 闭环不能因为某个 API 抽风就停摆。
3. **模型只出参数, 不出结论**。好不好由真机 + 置换检验裁决。模型看得到历史每组
   参数的实测数字与 p 值, 但没有任何改写判定的途径。

只用标准库 (urllib), 不引第三方依赖。
"""
from __future__ import annotations
import json
import os
import random
import re
import urllib.error
import urllib.request

# 与 generator.rs 的 PROVIDERS 同源, 按优先级排列
PROVIDERS = [
    {"name": "glm-4.7",
     "url": "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
     "model": "glm-4.7", "key_env": "GLM_KEY", "timeout_s": 180, "thinking_disable": True},
    {"name": "kimi-k2.7-code",
     "url": "https://api.kimi.com/coding/v1/chat/completions",
     "model": "kimi-k2.7-code", "key_env": "KIMI_KEY", "timeout_s": 180, "thinking_disable": True},
    {"name": "siliconflow-qwen3-coder",
     "url": "https://api.siliconflow.cn/v1/chat/completions",
     "model": "Qwen/Qwen3-Coder-30B-A3B-Instruct",
     "key_env": "SILICONFLOW_KEY", "timeout_s": 180, "thinking_disable": False},
    {"name": "openrouter",
     "url": "https://openrouter.ai/api/v1/chat/completions",
     "model": "qwen/qwen3-coder", "key_env": "OPENROUTER_KEY",
     "timeout_s": 180, "thinking_disable": False},
]


def parse_secrets(text: str) -> dict[str, str]:
    """只认 `KEY=VALUE` 与 `export KEY=VALUE`, 引号可选。

    刻意**不**做 shell 展开 —— 这是一份配置不是脚本, `$(...)`、反引号、管道
    一律当普通字符, 不给命令注入留口子 (与 generator.rs::parse_secrets 同规则)。
    """
    out: dict[str, str] = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):].strip()
        if "=" not in line:
            continue
        k, v = line.split("=", 1)
        v = v.strip()
        if len(v) >= 2 and v[0] == v[-1] and v[0] in "\"'":
            v = v[1:-1]
        out[k.strip()] = v
    return out


def load_keys(paths: list[str]) -> dict[str, str]:
    for p in paths:
        if p and os.path.exists(p):
            with open(p) as f:
                return parse_secrets(f.read())
    return {}


SYSTEM_PROMPT = (
    "你是移动端系统性能调优专家。给你一台已 root 的安卓手机的可写系统参数白名单, "
    "以及历史上每组参数在真机游戏负载下的实测结果。你的任务是挑下一批候选参数组。\n"
    "硬规矩:\n"
    "1. 只能用白名单里的参数名, 值只能从给出的取值表里选, 一个字都不能改。\n"
    "2. 绝不提出任何与温控保护相关的改动 (关热保护、抬温控阈值), 提了会被直接拒绝。\n"
    "3. 每组 1-4 个参数。参数少一点容易归因, 别一次全改。\n"
    "4. 各组之间要有明显差异, 不要提交几乎一样的组。\n"
    "5. 只输出 JSON, 不要解释、不要 markdown 代码块以外的任何文字。\n"
    "输出格式 (顶层是数组):\n"
    '[{"why":"一句话说明这组想验证什么","params":{"参数名":"值"}}, ...]'
)


def build_prompt(whitelist_desc: str, history: list[dict], n: int,
                 goal_note: str = "") -> str:
    lines = [
        "# 设备与负载",
        "红魔 NX809J (骁龙 canoe / Adreno 840v2, Android 16, 已 root)。",
        "负载: 原神大世界定点匀速转视角 36 秒, 游戏被厂商限帧器钉在 30fps。",
        "",
        "# 判定口径 (已冻结, 你改不了)",
        "看三样: frame_p95 (帧时 95 分位, 越小越好)、fps_mean (越大越好)、",
        "power_w_mean (整机功耗, 越小越好)。每组参数跑 A/B 交替多轮, 精确置换检验。",
        "任一指标显著改善且无任何指标显著变差 = 保留, 否则淘汰。",
        "**目标不是单纯省电** —— 把帧时压下去同样算赢。",
        "",
        "# 可改参数白名单 (只能用这些)",
        whitelist_desc,
        "",
    ]
    if goal_note:
        lines += [goal_note, ""]
    if history:
        lines.append("# 已经试过的参数组与真机实测结果")
        for h in history:
            lines.append(f"- 参数 {json.dumps(h.get('params', {}), ensure_ascii=False)}")
            lines.append(f"  判定 {h.get('verdict')} — {h.get('reason', '')}")
            for m, d in (h.get("per_metric") or {}).items():
                if isinstance(d, dict) and d.get("diff_pct") is not None:
                    lines.append(f"  {m}: {d.get('diff_pct')}% (p={d.get('p')}) {d.get('status')}")
        lines.append("")
        lines.append("别再提交与上面雷同的组。从被淘汰的组里学到的方向也请说明在 why 里。")
    else:
        lines.append("# 历史")
        lines.append("这是第一代, 还没有任何实测结果。")
        lines.append("已知线索: 之前手工验证过把 DDR 与 LLCC 的 boost_freq 钉到硬件上限, "
                     "frame_p95 改善 1.61% (p=0.0079)。说明这台机器上访存下限确实吃紧。")
    lines.append("")
    lines.append(f"# 现在给我 {n} 组候选, 只输出 JSON 数组。")
    return "\n".join(lines)


def parse_candidates(text: str, n: int) -> list[dict]:
    """从模型回包里抠出候选数组。纯函数, 容忍 markdown 围栏与前后废话。"""
    if not text:
        return []
    s = text.strip()
    fence = re.search(r"```(?:json)?\s*(.+?)```", s, re.S)
    if fence:
        s = fence.group(1).strip()
    start = s.find("[")
    end = s.rfind("]")
    if start < 0 or end <= start:
        return []
    try:
        arr = json.loads(s[start:end + 1])
    except json.JSONDecodeError:
        return []
    if not isinstance(arr, list):
        return []
    out = []
    for item in arr:
        if not isinstance(item, dict):
            continue
        params = item.get("params")
        if not isinstance(params, dict) or not params:
            continue
        out.append({"why": str(item.get("why", ""))[:300],
                    "params": {str(k): str(v) for k, v in params.items()}})
        if len(out) >= n:
            break
    return out


def chat(prompt: str, keys: dict[str, str], log=print) -> tuple[str, str] | None:
    """按优先级依次试 provider。返回 (provider 名, 回包文本), 全挂返回 None。"""
    for prov in PROVIDERS:
        key = keys.get(prov["key_env"]) or os.environ.get(prov["key_env"])
        if not key:
            continue
        body = {
            "model": prov["model"],
            "messages": [{"role": "system", "content": SYSTEM_PROMPT},
                         {"role": "user", "content": prompt}],
            "temperature": 0.8,
            "max_tokens": 2000,
        }
        if prov["thinking_disable"]:
            body["thinking"] = {"type": "disabled"}
        req = urllib.request.Request(
            prov["url"],
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json",
                     "Authorization": f"Bearer {key}"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=prov["timeout_s"]) as resp:
                raw = json.loads(resp.read().decode())
            content = raw["choices"][0]["message"]["content"]
            if content and content.strip():
                return prov["name"], content
            log(f"[llm] {prov['name']} 空回包, 换下一个")
        except (urllib.error.URLError, KeyError, IndexError, json.JSONDecodeError,
                TimeoutError, OSError) as e:
            # 只记异常类型与首行, 绝不打印请求头 (含密钥)
            log(f"[llm] {prov['name']} 失败: {type(e).__name__}: {str(e)[:120]}")
    return None


def _is_ordinal(pid: str, spec: dict) -> bool:
    """这个参数的取值大小有没有「更高 = 更激进」的物理含义?

    频率 (Hz/kHz) 和 pwrlevel 档位有: 数字大小直接对应快慢。
    厂商枚举没有 —— `refresh_rate_mode` 的 1 是 60Hz 而 0 是 120Hz auto,
    按数值往上推会把「降刷新率」当成「更激进」。governor 是字符串, 更没有。
    分不清就当没有: 等概率换一个别的值, 比装作知道方向要诚实。
    """
    if spec.get("kind") != "sysfs":
        return False
    return ("freq" in pid) or ("pwrlevel" in pid)


def local_mutate(wl: dict, history: list[dict], n: int, seed: int) -> list[dict]:
    """本地降级变异器 —— LLM 全挂时闭环照常往下跑。

    策略: 随机挑 1-2 个白名单参数, 往「更激进」的方向推 (频率类取更高档,
    pwrlevel 取更快档), 避开历史上已经试过的组, 并且**必须过白名单校验**
    —— 跨项约束 (min<=max 之类) 由 validate_candidate 统一把关, 这里不重复实现。
    固定 seed 下产出确定, 所以降级路径本身也是可复现的。
    """
    from whitelist import validate_candidate

    rng = random.Random(seed)
    tried = {json.dumps(h.get("params", {}), sort_keys=True) for h in history}
    pids = sorted(wl)
    out: list[dict] = []
    for _ in range(n * 60):
        if len(out) >= n:
            break
        pick = rng.sample(pids, min(len(pids), rng.choice([1, 2])))
        params = {}
        for pid in pick:
            vals = wl[pid]["values"]
            cur = wl[pid]["current"]
            numeric = bool(vals) and all(v.lstrip("-").isdigit() for v in vals)
            if _is_ordinal(pid, wl[pid]) and numeric and cur.lstrip("-").isdigit():
                # pwrlevel 语义反过来: 0 是最快档, 所以「更激进」= 往小走
                if "pwrlevel" in pid:
                    cand = [v for v in vals if int(v) < int(cur)]
                else:
                    cand = [v for v in vals if int(v) > int(cur)]
                # 没有更激进的取值就跳过这一项, 不往回退 —— 往回退等于在试一个
                # 与「探索更高性能」意图相反的方向, 那不是变异, 是噪声
            else:
                # 取值是厂商枚举 (如 refresh_rate_mode 的 0/1/2/4) 或 governor 名字,
                # 数值大小没有「更激进」的含义 —— 1 是 60Hz 而 0 是 120Hz auto。
                # 这种只能等概率换一个别的值, 不许假装知道方向。
                cand = [v for v in vals if v != cur]
            if cand:
                params[pid] = rng.choice(cand)
        if not params:
            continue
        key = json.dumps(params, sort_keys=True)
        if key in tried:
            continue
        if not validate_candidate(params, wl)[0]:
            continue
        tried.add(key)
        out.append({"why": "本地变异器降级产出 (LLM 不可用)", "params": params})
    return out
