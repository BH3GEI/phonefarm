#!/usr/bin/env python3
"""pack_repro.py <会话目录> <候选ID> <输出目录> — 把一次 phonefarm eval 打成可交给游戏厂商的复现包。

会话目录是 o2_sweep.py (或任何同布局的调用方) 的 --out: requests/<cid>.json、reports/<cid>.json、
evid/<cid>/。输出目录必须不存在 (只新建, 不覆盖)。包里:

  request.json      原请求, serial 抹掉 (复现时由 repro.sh 的参数给)
  report.json       原回包 (判定、指标、资格)
  qualification.json / qualification_policy.json   冻结的资格规则与当次核验结果
  whitelist.json    当次在游戏运行时探测到的可写节点与取值
  summary.txt       一页结论: 改了什么、画面、帧时、功耗、p 值、资格逐项
  repro.sh          一条命令复现: sh repro.sh <serial>, 在设备锁里重跑同一请求, 跑完打印与原结论的对照
  MANIFEST.sha256   以上文件的摘要

原始逐轮证据 (trace / env / 截图) 体积大, 不进包; summary.txt 记着它在本机的位置。
"""
import hashlib
import json
import shutil
import sys
from pathlib import Path

REPRO_SH = r'''#!/bin/sh
# 一条命令复现: sh repro.sh <serial> [phonefarm 二进制]
# 前提: 手机已 root、游戏已装好并能进大世界; 主机上有 phonefarm 仓库 (PF_REPO_ROOT 指过去)。
set -eu
SERIAL="${1:?用法: sh repro.sh <serial> [phonefarm 二进制]}"
PF="${2:-${PF_BIN:-phonefarm}}"
HERE=$(cd "$(dirname "$0")" && pwd)
OUT="$HERE/rerun_$(date +%Y%m%d-%H%M%S)"
mkdir "$OUT"
python3 - "$HERE/request.json" "$SERIAL" "$OUT/request.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1])); r["serial"] = sys.argv[2]
r.get("protocol", {}).pop("order_seed", None)   # 新的随机轮序, 不复用原来的
json.dump(r, open(sys.argv[3], "w"), ensure_ascii=False, indent=1)
PY
STAGE=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["protocol"].get("stage","full"))' "$OUT/request.json")
EXTRA=""; [ "$STAGE" = full ] && EXTRA="--no-curtail"
LOCK="${PF_REPO_ROOT:-.}/tools/devlock"
if [ -x "$LOCK" ] && [ -z "${DEVLOCK_LABEL:-}" ]; then
  "$LOCK" run repro -- "$PF" eval --request "$OUT/request.json" --serial "$SERIAL" --out "$OUT/evid" $EXTRA > "$OUT/report.json"
else
  "$PF" eval --request "$OUT/request.json" --serial "$SERIAL" --out "$OUT/evid" $EXTRA > "$OUT/report.json"
fi
python3 "$HERE/compare.py" "$HERE/report.json" "$OUT/report.json"
'''

COMPARE_PY = r'''import json, sys
def view(p):
    r = json.load(open(p)); v = r.get("verdict") or {}; m = r.get("metrics") or {}
    return {"status": r.get("status"), "pareto": v.get("is_pareto_improvement"), "p": v.get("p_value"),
            "power_w": m.get("power_watt"), "fps_mean": m.get("fps_mean"), "p95_ms": m.get("fps_p95_ms"),
            "eligible": (r.get("qualification") or {}).get("eligible"), "reason": v.get("reason")}
a, b = view(sys.argv[1]), view(sys.argv[2])
w = max(len(k) for k in a)
print(f"{'':{w}}  {'原结论':>14}  {'复现':>14}")
for k in a:
    print(f"{k:{w}}  {str(a[k])[:14]:>14}  {str(b[k])[:14]:>14}")
same = a["status"] == b["status"] and a["pareto"] == b["pareto"]
print("复现一致" if same else "复现与原结论不一致, 见上表与 rerun_*/evid")
sys.exit(0 if same else 3)
'''


def summary(cid, req, rep, q, evid):
    v = rep.get("verdict") or {}
    m = rep.get("metrics") or {}
    lines = [f"候选: {cid}", f"改动: {json.dumps((req.get('payload') or {}).get('params'), ensure_ascii=False)}",
             f"游戏: {req.get('package')}  负载: {(req.get('workload') or {}).get('script')}",
             f"协议: {json.dumps({k: (req.get('protocol') or {}).get(k) for k in ('stage', 'rounds', 'p_threshold', 'stat', 'baseline_runs', 'baseline_warmup')}, ensure_ascii=False)}",
             "", f"状态: {rep.get('status')}  帕累托改进: {v.get('is_pareto_improvement')}  p={v.get('p_value')}",
             f"理由: {v.get('reason')}", "", "指标 (候选相对对照):"]
    for row in ((v.get("scorecard") or {}).get("rows") or []):
        lines.append(f"  {row.get('dimension')}: {row.get('status')} value={row.get('value')}{row.get('unit') or ''} "
                     f"ci95={row.get('ci95')}  {row.get('reason') or ''}")
    lines.append(f"  原始: {json.dumps(m, ensure_ascii=False)}")
    lines += ["", f"资格: eligible={q.get('eligible')} status={q.get('status')}"]
    for c in q.get("checks") or []:
        lines.append(f"  [{c.get('state')}] {c.get('id')}  {c.get('reason') if c.get('state') != 'pass' else ''}")
    lines += ["", f"本机原始证据: {evid}"]
    return "\n".join(lines) + "\n"


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    sess, cid, dest = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
    if dest.exists():
        sys.exit(f"{dest} 已存在, 不覆盖")
    req = json.loads((sess / "requests" / f"{cid}.json").read_text())
    rep = json.loads((sess / "reports" / f"{cid}.json").read_text())
    evid = sess / "evid" / cid
    q = rep.get("qualification") or {}
    dest.mkdir(parents=True)
    req.pop("serial", None)
    (dest / "request.json").write_text(json.dumps(req, ensure_ascii=False, indent=1))
    shutil.copyfile(sess / "reports" / f"{cid}.json", dest / "report.json")
    for f in ("qualification.json", "qualification_policy.json"):
        if (evid / f).is_file():
            shutil.copyfile(evid / f, dest / f)
    wl = (req.get("payload") or {}).get("whitelist")
    if wl is not None:
        (dest / "whitelist.json").write_text(json.dumps(wl, ensure_ascii=False, indent=1))
    (dest / "summary.txt").write_text(summary(cid, req, rep, q, evid.resolve()))
    (dest / "repro.sh").write_text(REPRO_SH)
    (dest / "repro.sh").chmod(0o755)
    (dest / "compare.py").write_text(COMPARE_PY)
    files = sorted(p for p in dest.iterdir() if p.is_file())
    (dest / "MANIFEST.sha256").write_text("".join(
        f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in files))
    print(dest)


if __name__ == "__main__":
    main()
