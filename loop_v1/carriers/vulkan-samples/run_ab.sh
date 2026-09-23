#!/bin/bash
# run_ab.sh <sample> <config_A> <config_B> [rounds] [outdir] [frames] [capdur]
#
# 对同一个 Vulkan-Samples 样例的两档开关跑交错 NvN, 交给 ../tools/analyze.py 出
# 效应量 / 精确置换检验 p 值 / 置换反演 95% CI。
#
# 交错 (A,B,A,B,...) 而不是先跑完 A 再跑 B: 温度、内存压力、后台调度都会沿时间漂移,
# 分块跑会把漂移整块算进臂间差异。交错让漂移对两臂同等作用。
# 无效轮 (热事件 / 非干净退出 / 提交线程不唯一) 就地重试至多 2 次, 证据全部保留。
#
# 退出码: 0=跑完并出报告  2=硬失败
set -uo pipefail

SAMPLE="${1:?用法: run_ab.sh <sample> <config_A> <config_B> [rounds] [outdir] [frames] [capdur]}"
CFG_A="${2:?缺 A 臂 config 索引}"
CFG_B="${3:?缺 B 臂 config 索引}"
ROUNDS="${4:-5}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"          # loop_v1/
OUT="${5:-$ROOT/runs_vks/${SAMPLE}_c${CFG_A}_vs_c${CFG_B}}"
FRAMES="${6:-3000}"
CAPDUR="${7:-12}"

SERIAL="${VKS_SERIAL:-91253241019A}"
TOOLS="$ROOT/tools"
ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }

mkdir -p "$OUT"
echo "══ vulkan-samples A/B: $SAMPLE  config $CFG_A vs $CFG_B  ${ROUNDS}v${ROUNDS} → $OUT"

# ── 前置: 设备脚本 + 唤醒 ──
adb -s "$SERIAL" push "$TOOLS/device_snapshot.sh" "$TOOLS/ftrace_capture.sh" /data/local/tmp/ >/dev/null
ashell "chmod 755 /data/local/tmp/device_snapshot.sh /data/local/tmp/ftrace_capture.sh"
ashell "input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard" >/dev/null 2>&1 || true

# ── 风扇状态另存, 退出必还原 (fan 不在 38 行快照内, 与 refbench/run_m0.sh 同) ──
FAN_EN=$(ashell "su -c 'cat /sys/kernel/fan/fan_enable'" 2>/dev/null | tr -d '\r' || true)
FAN_LV=$(ashell "su -c 'cat /sys/kernel/fan/fan_speed_level'" 2>/dev/null | tr -d '\r' || true)
restore_fan() {
  ashell "su -c 'echo ${FAN_EN:-0} > /sys/kernel/fan/fan_enable; echo ${FAN_LV:-4} > /sys/kernel/fan/fan_speed_level'" >/dev/null 2>&1 || true
}
trap restore_fan EXIT
ashell "su -c 'echo 1 > /sys/kernel/fan/fan_enable; echo 5 > /sys/kernel/fan/fan_speed_level'" >/dev/null 2>&1 || true

one() {   # one <label> <config>
  local label="$1" cfg="$2" try
  for try in 1 2 3; do
    rm -rf "$OUT/$label"
    if bash "$HERE/run_vks.sh" "$label" "$OUT/$label" "$SAMPLE" "$cfg" "$FRAMES" "$CAPDUR"; then
      return 0
    fi
    local rc=$?
    [ $rc -eq 2 ] && { echo "  [$label] 硬失败"; return 2; }
    mv "$OUT/$label" "$OUT/${label}_invalid$try" 2>/dev/null || true
    echo "  [$label] 无效轮, 重试 $try/2"
  done
  echo "  [$label] 三次都无效, 放弃该轮"
  return 3
}

for i in $(seq 1 "$ROUNDS"); do
  one "a$i" "$CFG_A" || [ $? -eq 3 ] || exit 2
  one "b$i" "$CFG_B" || [ $? -eq 3 ] || exit 2
done

echo "══ 汇总"
python3 "$TOOLS/analyze.py" "$OUT/a*/summary.json" "$OUT/b*/summary.json" > "$OUT/analyze.json"
python3 "$HERE/vks_report.py" --root "$OUT" --sample "$SAMPLE" \
        --config-a "$CFG_A" --config-b "$CFG_B" | tee "$OUT/report.txt"
