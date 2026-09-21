#!/bin/bash
# run_m0.sh — refbench M0 全套判据电池, 零交互
#
# 序列: 强度阶梯 (判据 2) → 交错 5v5 loadop 两臂 (判据 1/3) → frag/idle 变体 (判据 4)
#       → 末快照 (判据 5) → 回放自检 (判据 6) → refbench_report.py 汇总
# 无效轮 (热事件/非干净退出) 就地重试至多 2 次, 证据全部保留。
set -uo pipefail

SERIAL="${REFBENCH_SERIAL:-91253241019A}"
PKG=io.github.hgamey.refbench
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RB="$ROOT/loop_v1/refbench"
TOOLS="$ROOT/loop_v1/tools"
OUT="${1:-$ROOT/loop_v1/runs_refbench}"

ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }
mkdir -p "$OUT"

echo "══ refbench M0 电池开始 → $OUT"

# ── 前置: 设备脚本 + 唤醒 ──
adb -s "$SERIAL" push "$TOOLS/device_snapshot.sh" "$TOOLS/ftrace_capture.sh" /data/local/tmp/ >/dev/null
ashell "chmod 755 /data/local/tmp/device_snapshot.sh /data/local/tmp/ftrace_capture.sh"
ashell "input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard" >/dev/null 2>&1 || true

# ── 会话基线快照; 风扇状态另存 (不在 38 行快照内), 退出必还原 ──
ashell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUT/snap_before.txt" 2>&1
FAN_EN=$(ashell "su -c 'cat /sys/kernel/fan/fan_enable'" 2>/dev/null | tr -d '\r' || true)
FAN_LV=$(ashell "su -c 'cat /sys/kernel/fan/fan_speed_level'" 2>/dev/null | tr -d '\r' || true)
restore_fan() {
  ashell "su -c 'echo ${FAN_EN:-0} > /sys/kernel/fan/fan_enable; echo ${FAN_LV:-4} > /sys/kernel/fan/fan_speed_level'" >/dev/null 2>&1 || true
}
trap restore_fan EXIT
ashell "su -c 'echo 1 > /sys/kernel/fan/fan_enable'" >/dev/null 2>&1 || true

# ── 单轮 + 无效重试 ──
run_valid() { # run_valid <label> <scene> <intensity> <loadop> <frames>
  local lab="$1" dir rc
  for att in 0 1 2; do
    dir="$OUT/$lab"; [ "$att" -gt 0 ] && dir="$OUT/${lab}_retry$att"
    bash "$RB/run_refbench.sh" "$lab" "$dir" "$2" "$3" "$4" "$5"
    rc=$?
    [ "$rc" = 0 ] && return 0
    [ "$rc" = 3 ] && { echo "[$lab] 无效, 重试"; continue; }
    echo "[$lab] 硬失败 rc=$rc"; return "$rc"
  done
  echo "[$lab] 连续三次无效"; return 3
}

FAIL=0
# 判据 2: 强度阶梯 (同场景同臂, 只动 intensity)
run_valid inten_i06 bw_pingpong 6 off 3000 || FAIL=1
run_valid inten_i10 bw_pingpong 10 off 3000 || FAIL=1
run_valid inten_i14 bw_pingpong 14 off 3000 || FAIL=1
# 判据 1/3: 交错 A/B, 残余漂移对两臂等量影响
for i in 1 2 3 4 5; do
  run_valid bw_ctrl$i bw_pingpong 10 off 3000 || FAIL=1
  run_valid bw_knob$i bw_pingpong 10 on 3000 || FAIL=1
done
# 判据 4: 瓶颈类型不同的变体
for i in 1 2 3; do run_valid frag$i frag_alu 6 off 3000 || FAIL=1; done
for i in 1 2 3; do run_valid idle$i idle_cap 1 off 3600 || FAIL=1; done

# ── 还原风扇, 等热态回落到基线再取末快照 (thermal_pwrlevel 是 38 行之一) ──
restore_fan
trap - EXIT
BASE_TP=$(grep '^kgsl.thermal_pwrlevel=' "$OUT/snap_before.txt" || true)
for _ in $(seq 1 360); do
  CUR=$(ashell "su -c 'cat /sys/class/kgsl/kgsl-3d0/thermal_pwrlevel'" 2>/dev/null | tr -d '\r' || true)
  [ "kgsl.thermal_pwrlevel=$CUR" = "$BASE_TP" ] && break
  sleep 2
done
ashell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUT/snap_final.txt" 2>&1

# ── 回放自检 (先跑: 逐轮 summary/attribution 字节一致) → 报告 → 再回放 (含报告重放) ──
bash "$TOOLS/replay_test.sh" "$OUT" > "$OUT/replay1.log" 2>&1
echo $? > "$OUT/replay_exit.txt"

python3 - "$RB" "$OUT" <<'PYEOF'
import os, sys
rel = os.path.relpath(os.path.join(sys.argv[1], "refbench_report.py"), sys.argv[2])
with open(os.path.join(sys.argv[2], "report.cmd"), "w") as f:
    f.write(f'python3 "{rel}" --root .\n')
PYEOF
(cd "$OUT" && bash report.cmd) > "$OUT/report.json"
bash "$TOOLS/replay_test.sh" "$OUT" > "$OUT/replay2.log" 2>&1
R2=$?
echo "$R2" > "$OUT/replay_exit2.txt"

python3 - "$OUT/report.json" <<'PYEOF'
import json, sys
d = json.load(open(sys.argv[1]))
s = d["summary"]
print("══ 六判据:", s["per_criterion"])
print("══ 全绿:", s["all_pass"])
PYEOF
[ "$R2" = 0 ] || { echo "══ 报告回放不一致 (replay2=$R2)"; FAIL=1; }
ALL=$(python3 -c "import json;print(json.load(open('$OUT/report.json'))['summary']['all_pass'])")
if [ "$FAIL" = 0 ] && [ "$ALL" = "True" ]; then echo "══ M0 通过"; exit 0; fi
echo "══ M0 未通过 (FAIL=$FAIL all_pass=$ALL)"; exit 1
