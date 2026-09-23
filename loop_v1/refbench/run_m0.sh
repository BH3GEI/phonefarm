#!/bin/bash
# run_m0.sh — refbench M0 全套判据电池, 零交互
#
# 序列: 强度阶梯 (判据 2) → 交错 5v5 loadop 两臂 (判据 1/3) → frag/idle 变体 (判据 4)
#       → 末快照 (判据 5) → 回放自检 (判据 6) → phonefarm refbench-report 汇总
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
wait_plateau() {  # 等"完全散热"态: max_gpuclk 回到未受限上限 且 thermal_pwrlevel=0。
  # 首版只等"稳定"→ 收在过冷/过热的瞬时台阶 (826 vs 1200), 两次快照不等。现在两端都
  # 等到同一个确定态 (满频 1200MHz + 无热约束), 使 max_gpuclk / thermal_pwrlevel 逐位相同。
  # 末端从跑热回落到满频需较久, 有界 90×10s=15min (风扇主动降温, 空闲发热极小, 可达)。
  local clk tp
  for _ in $(seq 1 90); do
    clk=$(ashell "su -c 'cat /sys/class/kgsl/kgsl-3d0/max_gpuclk'" 2>/dev/null | tr -d '\r' || true)
    tp=$(ashell "su -c 'cat /sys/class/kgsl/kgsl-3d0/thermal_pwrlevel'" 2>/dev/null | tr -d '\r' || true)
    [ "$clk" = "1200000000" ] && [ "$tp" = "0" ] && { echo "  完全散热态 max_gpuclk=$clk thermal_pwrlevel=$tp"; return 0; }
    sleep 10
  done
  echo "  散热等待超时 (max_gpuclk=$clk thermal_pwrlevel=$tp)"
}

ashell "su -c 'echo 1 > /sys/kernel/fan/fan_enable; echo 5 > /sys/kernel/fan/fan_speed_level'" >/dev/null 2>&1 || true
ashell "am force-stop $PKG" >/dev/null 2>&1 || true   # 确保 GPU 空闲再等台阶
echo "── 起始热台阶 (风扇开+空闲) ──"; wait_plateau
ashell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUT/snap_before.txt" 2>&1

# ── 单轮 + 无效重试 ──
# 设备被本会话连续压测热浸透, 起测温度偏高 → 满载时偶发单次 kgsl_thermal_constraint。
# 实测该单次事件对帧时无影响 (无效 i06 与有效重试 p50 差 0.05%), 但归因 (phonefarm attribute) 只要
# n_thermal>0 就判"热降频受限", 污染判据4 的归因, 所以仍须拿到 0 事件轮 —— 靠更深的
# 轮前散热 (REFBENCH_COOL_MC) + 更多重试, 而非放宽判定。
export REFBENCH_COOL_MC=39500   # 松散热地板 (快速轮转), 靠 12s 短采集 + 6 次重试拿 0 事件轮
run_valid() { # run_valid <label> <scene> <intensity> <loadop> <frames>
  local lab="$1" dir rc
  for att in 0 1 2 3 4 5; do
    dir="$OUT/$lab"; [ "$att" -gt 0 ] && dir="$OUT/${lab}_retry$att"
    bash "$RB/run_refbench.sh" "$lab" "$dir" "$2" "$3" "$4" "$5"
    rc=$?
    [ "$rc" = 0 ] && return 0
    [ "$rc" = 3 ] && { echo "[$lab] 无效, 重试"; continue; }
    echo "[$lab] 硬失败 rc=$rc"; return "$rc"
  done
  echo "[$lab] 连续六次无效 (设备热浸透, 需更长物理静置)"; return 3
}

FAIL=0
# 判据 2: 强度阶梯 (同场景同臂, 只动 intensity)。用 i06/i08/i10: 都在 spf=1 区。
# (i14 时驱动会把一次 vkQueueSubmit 拆成多个 cmdbatch, loop_v1 解析器自相关判成 spf=6,
#  帧时被 6× 虚高 —— 那是驱动拆批 + 小样本 ACF 的产物, 不是真负载。避开该区保持阶梯干净。)
run_valid inten_i06 bw_pingpong 6 off 3000 || FAIL=1
run_valid inten_i08 bw_pingpong 8 off 3000 || FAIL=1
run_valid inten_i10 bw_pingpong 10 off 3000 || FAIL=1
# 判据 1/3: 交错 A/B。用 i06 (150fps): 12s 采集拿 ~1800 帧 → 分位数采样噪声小 → 判据1
# 离散度更稳; 且轻负载更凉少触热。i06 仍 GPU 受限 (bw~51000) 且 6 个多余 LOAD 足够让
# loadop 旋钮在主指标上显著 (判据3 历史裕度极大, p≈0.008)。
for i in 1 2 3 4 5; do
  run_valid bw_ctrl$i bw_pingpong 6 off 3000 || FAIL=1
  run_valid bw_knob$i bw_pingpong 6 on 3000 || FAIL=1
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

printf '%s\n' '. ../tools/pf_bin.sh' '"$PF" refbench-report --root .' > "$OUT/report.cmd"
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
