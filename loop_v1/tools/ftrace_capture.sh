#!/bin/sh
# ftrace_capture.sh <duration_s> <out_path> — 设备端执行, 需 root
#
# 采一段 ftrace 文本, 同时拿到:
#   帧节奏  : adreno_cmdbatch_submitted/retired (游戏渲染线程的 GPU 提交与退役)
#   GPU 归因: retired 事件里的 start/retire 时间戳 = 该次提交的真实 GPU 执行时长
#   带宽    : kgsl_buslevel 的 avg_bw (直接观测, 不是推断)
#   频率/热 : kgsl_pwrlevel / kgsl_gpu_frequency / kgsl_thermal_constraint / kgsl_clock_throttling
#
# 纪律: 进入前把 ftrace 的四项可变状态存档, 无论成功失败都在退出前逐项还原
#       (tracing_on / current_tracer / buffer_size_kb / set_event)。
#       只 enable 本脚本需要的事件, 退出时只 disable 自己开的那些, 原本开着的原样保留。
set -u

DUR="${1:-10}"
OUT="${2:-/data/local/tmp/loop_v1_trace.txt}"
T=/sys/kernel/tracing

# 本脚本需要的事件 (kgsl 子系统)
EVENTS="adreno_cmdbatch_submitted adreno_cmdbatch_retired kgsl_buslevel kgsl_pwrlevel kgsl_gpu_frequency kgsl_thermal_constraint kgsl_clock_throttling kgsl_bcl_clock_throttling"

# ── 存档 ──
SAVE_ON=$(cat "$T/tracing_on" 2>/dev/null)
SAVE_TRACER=$(cat "$T/current_tracer" 2>/dev/null)
SAVE_BUF=$(cat "$T/buffer_size_kb" 2>/dev/null)
SAVE_EVENTS=$(cat "$T/set_event" 2>/dev/null)

# 记录哪些事件是"我开的"(原本没开的才算), 退出时只关这些
MINE=""
for e in $EVENTS; do
  if ! echo "$SAVE_EVENTS" | grep -qx "kgsl:$e"; then
    MINE="$MINE $e"
  fi
done

restore() {
  echo 0 > "$T/tracing_on" 2>/dev/null
  for e in $MINE; do
    echo 0 > "$T/events/kgsl/$e/enable" 2>/dev/null
  done
  [ -n "$SAVE_TRACER" ] && echo "$SAVE_TRACER" > "$T/current_tracer" 2>/dev/null
  [ -n "$SAVE_BUF" ]    && echo "$SAVE_BUF"    > "$T/buffer_size_kb" 2>/dev/null
  echo > "$T/trace" 2>/dev/null
  [ -n "$SAVE_ON" ]     && echo "$SAVE_ON"     > "$T/tracing_on" 2>/dev/null
}
trap 'restore' EXIT INT TERM

# ── 布置 ──
echo 0 > "$T/tracing_on"
echo nop > "$T/current_tracer"
echo 65536 > "$T/buffer_size_kb"   # 每 CPU 64MB, 足够 60s @ 60fps
echo > "$T/trace"

for e in $EVENTS; do
  echo 1 > "$T/events/kgsl/$e/enable" 2>/dev/null || echo "WARN: 无法启用 kgsl:$e" >&2
done

# ── 采集 ──
echo 1 > "$T/tracing_on"
MARK_START=$(cat /proc/uptime | cut -d' ' -f1)
sleep "$DUR"
echo 0 > "$T/tracing_on"
MARK_END=$(cat /proc/uptime | cut -d' ' -f1)

# ── 落盘 (带元信息头, 供离线回放自证) ──
{
  echo "#loop_v1_meta capture_start_uptime=$MARK_START capture_end_uptime=$MARK_END duration_req=$DUR"
  echo "#loop_v1_meta gpu_model=$(cat /sys/class/kgsl/kgsl-3d0/gpu_model 2>/dev/null)"
  echo "#loop_v1_meta ddr_cur_khz=$(cat /sys/devices/system/cpu/bus_dcvs/DDR/cur_freq 2>/dev/null) ddr_max_khz=$(cat /sys/devices/system/cpu/bus_dcvs/DDR/hw_max_freq 2>/dev/null)"
  echo "#loop_v1_meta ddr_boost_khz=$(cat /sys/devices/system/cpu/bus_dcvs/DDR/boost_freq 2>/dev/null) llcc_boost_khz=$(cat /sys/devices/system/cpu/bus_dcvs/LLCC/boost_freq 2>/dev/null)"
  echo "#loop_v1_meta gpu_busy=$(cat /sys/class/kgsl/kgsl-3d0/gpubusy 2>/dev/null) gpu_clk=$(cat /sys/class/kgsl/kgsl-3d0/gpuclk 2>/dev/null)"
  echo "#loop_v1_meta thermal_pwrlevel=$(cat /sys/class/kgsl/kgsl-3d0/thermal_pwrlevel 2>/dev/null) min_pwrlevel=$(cat /sys/class/kgsl/kgsl-3d0/min_pwrlevel 2>/dev/null)"
  cat "$T/trace"
} > "$OUT" 2>/dev/null

LINES=$(wc -l < "$OUT" 2>/dev/null)
echo "CAPTURE_OK lines=$LINES out=$OUT"
# restore 由 trap 执行
