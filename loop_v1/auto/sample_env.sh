#!/bin/sh
# sample_env.sh <时长s> <间隔s> — 设备端执行。整机功耗与结温的等间隔采样。
#
# 与 ftrace 采集并行跑: ftrace 给帧时序, 这里给「这一轮到底费了多少电、烫到多少度」。
# 项目目标不是单纯省电, 但功耗与温度是判定「更好」的两个必要项, 所以每轮都要量。
#
# 只读 /sys/class/power_supply 与 /sys/class/thermal, 不碰任何充电控制、
# 不改任何热策略。输出每行一条样本, 主机端 env_stats() 纯函数解析。
#
#   ENV <uptime> <batt_uv> <batt_ua> <batt_uw|NA> <usb_uv> <usb_ua> <热区> <毫摄氏度>
set -u

DUR="${1:-30}"
IVL="${2:-2}"
B=/sys/class/power_supply/battery
U=/sys/class/power_supply/usb

echo "#sample_env v1 dur=$DUR interval=$IVL"
echo "#battery_status=$(cat $B/status 2>/dev/null)"

n=0
while [ "$n" -lt "$DUR" ]; do
  up=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)
  bv=$(cat $B/voltage_now 2>/dev/null || echo 0)
  bi=$(cat $B/current_now 2>/dev/null || echo 0)
  bp=$(cat $B/power_now 2>/dev/null || echo NA)
  uv=$(cat $U/voltage_now 2>/dev/null || echo 0)
  ui=$(cat $U/current_now 2>/dev/null || echo 0)
  # 只取 SoC 结温 (cpu-* / cpullc* / gpuss*), 与 phonefarm hwcond.rs 的 soc_max_c 同口径
  hot_t=0; hot_z=none
  for z in /sys/class/thermal/thermal_zone*; do
    ty=$(cat "$z/type" 2>/dev/null)
    case "$ty" in
      cpu-*|cpullc*|gpuss*) ;;
      *) continue ;;
    esac
    t=$(cat "$z/temp" 2>/dev/null)
    case "$t" in ''|*[!0-9-]*) continue ;; esac
    [ "$t" -gt 0 ] 2>/dev/null || continue
    [ "$t" -lt 100000 ] 2>/dev/null || continue
    if [ "$t" -gt "$hot_t" ] 2>/dev/null; then hot_t=$t; hot_z=$ty; fi
  done
  printf 'ENV %s %s %s %s %s %s %s %s\n' "$up" "$bv" "$bi" "$bp" "$uv" "$ui" "$hot_z" "$hot_t"
  n=$((n + IVL))
  sleep "$IVL"
done
echo "#sample_env done"
