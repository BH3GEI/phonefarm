#!/bin/bash
# replay_refbench.sh <runs根目录> — 判据6 (refbench 版): 离线回放, 无设备, 字节一致。
#
# 与 loop_v1/tools/replay_test.sh 同构, 唯一区别: 重解 trace 时传 --comm RefbenchDrv。
# 通用版硬编码默认 comm (UnityGfxDeviceW), 对 refbench 的 trace 解析出 n_submits=0,
# 与原 summary 对不上 —— 那是 comm 没串下来, 不是解析不确定 (解析是纯函数, 同 comm 必一致)。
set -uo pipefail

ROOT="${1:?用法: replay_refbench.sh <runs根目录>}"
TOOLS="$(cd "$(dirname "$0")/../tools" && pwd)"
. "$TOOLS/pf_bin.sh"
COMM="${REFBENCH_COMM:-RefbenchDrv}"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
fail=0; n=0

echo "── refbench 离线回放自检 (不连设备, --comm $COMM) ──"

for d in "$ROOT"/*/; do
  [ -f "$d/trace.txt" ] && [ -f "$d/summary.json" ] || continue
  n=$((n+1)); label=$(basename "$d")
  "$PF" parse-trace "$d/trace.txt" --comm "$COMM" > "$TMP/$label.json" 2>/dev/null
  if cmp -s "$TMP/$label.json" "$d/summary.json"; then echo "  ✓ $label summary 字节一致"
  else echo "  ✗ $label summary 不一致"; fail=1; fi
done

for d in "$ROOT"/*/; do
  [ -f "$d/attribution.json" ] || continue
  label=$(basename "$d")
  "$PF" attribute "$d/summary.json" > "$TMP/$label.attr.json" 2>/dev/null
  cmp -s "$TMP/$label.attr.json" "$d/attribution.json" && echo "  ✓ $label attribution 字节一致" \
    || { echo "  ✗ $label attribution 不一致"; fail=1; }
done

if [ -f "$ROOT/report.json" ] && [ -f "$ROOT/report.cmd" ]; then
  ( cd "$ROOT" && bash report.cmd ) > "$TMP/report.json" 2>/dev/null
  cmp -s "$TMP/report.json" "$ROOT/report.json" && echo "  ✓ report.json 字节一致" \
    || { echo "  ✗ report.json 不一致"; diff "$ROOT/report.json" "$TMP/report.json" | head -20 | sed 's/^/      /'; fail=1; }
fi

echo "──"
[ "$fail" = 0 ] && echo "✅ 回放自检通过: $n 轮 trace 全部可离线重算且字节一致" || echo "❌ 回放自检失败"
exit "$fail"
