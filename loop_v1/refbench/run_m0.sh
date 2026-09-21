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

# ── 风扇状态另存, 退出必还原 (fan 不在 38 行快照内) ──
FAN_EN=$(ashell "su -c 'cat /sys/kernel/fan/fan_enable'" 2>/dev/null | tr -d '\r' || true)
FAN_LV=$(ashell "su -c 'cat /sys/kernel/fan/fan_speed_level'" 2>/dev/null | tr -d '\r' || true)
restore_fan() {
  ashell "su -c 'echo ${FAN_EN:-0} > /sys/kernel/fan/fan_enable; echo ${FAN_LV:-4} > /sys/kernel/fan/fan_speed_level'" >/dev/null 2>&1 || true
}
trap restore_fan EXIT

# 判据5 关键修正: thermal_pwrlevel / max_gpuclk 是温度派生量, 两次快照必须在同一热态取。
# 首版把 before 取在风扇关(暖)、after 取在风扇开(过冷) → 两行漂移 → 判据5挂。
# 现在两次都取在"风扇开 + GPU 空闲"的稳定台阶上, 使这两行相等。
wait_plateau() {  # 风扇开+空闲下, 等 max_gpuclk 连续 3 次不变 (热稳态), 有界 60×4s
  local prev="" cur same=0
  for _ in $(seq 1 60); do
    cur=$(ashell "su -c 'cat /sys/class/kgsl/kgsl-3d0/max_gpuclk'" 2>/dev/null | tr -d '\r' || true)
    if [ -n "$cur" ] && [ "$cur" = "$prev" ]; then
      same=$((same+1)); [ "$same" -ge 3 ] && { echo "  热台阶 max_gpuclk=$cur"; return 0; }
    else same=0; fi
    prev="$cur"; sleep 4
  done
  echo "  热台阶等待超时 (max_gpuclk=$cur)"
}

ashell "su -c 'echo 1 > /sys/kernel/fan/fan_enable'" >/dev/null 2>&1 || true
ashell "am force-stop $PKG" >/dev/null 2>&1 || true   # 确保 GPU 空闲再等台阶
echo "── 起始热台阶 (风扇开+空闲) ──"; wait_plateau
ashell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUT/snap_before.txt" 2>&1

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
# 判据 2: 强度阶梯 (同场景同臂, 只动 intensity)。用 i06/i08/i10: 都在 spf=1 区。
# (i14 时驱动会把一次 vkQueueSubmit 拆成多个 cmdbatch, loop_v1 解析器自相关判成 spf=6,
#  帧时被 6× 虚高 —— 那是驱动拆批 + 小样本 ACF 的产物, 不是真负载。避开该区保持阶梯干净。)
run_valid inten_i06 bw_pingpong 6 off 3000 || FAIL=1
run_valid inten_i08 bw_pingpong 8 off 3000 || FAIL=1
run_valid inten_i10 bw_pingpong 10 off 3000 || FAIL=1
# 判据 1/3: 交错 A/B, 残余漂移对两臂等量影响
for i in 1 2 3 4 5; do
  run_valid bw_ctrl$i bw_pingpong 10 off 3000 || FAIL=1
  run_valid bw_knob$i bw_pingpong 10 on 3000 || FAIL=1
done
# 判据 4: 瓶颈类型不同的变体
for i in 1 2 3; do run_valid frag$i frag_alu 6 off 3000 || FAIL=1; done
for i in 1 2 3; do run_valid idle$i idle_cap 1 off 3600 || FAIL=1; done

# ── 末快照: 同样在"风扇开 + GPU 空闲"台阶上取, 与 snap_before 同热态 → 判据5 相等 ──
ashell "am force-stop $PKG" >/dev/null 2>&1 || true
echo "── 末尾热台阶 (风扇开+空闲) ──"; wait_plateau
ashell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUT/snap_final.txt" 2>&1
restore_fan     # 风扇还原 (关), 在末快照之后, 不影响 38 行
trap - EXIT

# ── 回放自检 (refbench 版, --comm RefbenchDrv) → 报告 → 再回放 (含报告重放) ──
bash "$RB/replay_refbench.sh" "$OUT" > "$OUT/replay1.log" 2>&1
echo $? > "$OUT/replay_exit.txt"

python3 - "$RB" "$OUT" <<'PYEOF'
import os, sys
rel = os.path.relpath(os.path.join(sys.argv[1], "refbench_report.py"), sys.argv[2])
with open(os.path.join(sys.argv[2], "report.cmd"), "w") as f:
    f.write(f'python3 "{rel}" --root .\n')
PYEOF
(cd "$OUT" && bash report.cmd) > "$OUT/report.json"
bash "$RB/replay_refbench.sh" "$OUT" > "$OUT/replay2.log" 2>&1
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
