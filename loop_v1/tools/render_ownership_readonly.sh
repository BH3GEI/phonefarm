#!/system/bin/sh
# Registered bounded readonly producer: package/task epochs, formats, clock and loss.
set -eu
PKG=${1:?package}; COMM=${2:?render comm}
case "$PKG:$COMM" in *[!a-zA-Z0-9_.:]*) exit 81 ;; esac
bounded() { size=$(wc -c < "$1"); [ "$size" -gt 0 ] && [ "$size" -le 65536 ]; }
block() { bounded "$2"; printf 'BEGIN\t%s\n' "$1"; cat "$2"; printf '\nEND\t%s\n' "$1"; }
value() { printf 'BEGIN\t%s\n%s\nEND\t%s\n' "$1" "$2" "$1"; }
printf '#phonefarm_render_witness v2\n'
value package "$PKG"
block boot /proc/sys/kernel/random/boot_id
block uptime /proc/uptime
block kernel /proc/version
PIDS=$(pidof "$PKG"); set -- $PIDS; [ "$#" -eq 1 ]; PID=$1
case "$PID" in ''|*[!0-9]*) exit 81 ;; esac
value pid "$PID"
value package_uid "$(cmd package list packages -U "$PKG")"
VERSION=$(dumpsys package "$PKG" | sed -n '/^[[:space:]]*versionCode=/p; /^[[:space:]]*versionName=/p')
[ "${#VERSION}" -le 4096 ]; value package_version "$VERSION"
for item in status stat; do block "process_$item" "/proc/$PID/$item"; done
bounded "/proc/$PID/cmdline"; value process_cmdline "$(tr '\000' '\n' < "/proc/$PID/cmdline")"
count=0
for task in /proc/"$PID"/task/[0-9]*; do
  [ -d "$task" ]; count=$((count+1)); [ "$count" -le 256 ]; TID=${task##*/}
  for item in status stat comm; do block "thread_$item/$TID" "$task/$item"; done
done
[ "$count" -gt 0 ]; value task_count "$count"
count=0
for dev in /sys/class/kgsl/kgsl-*; do
  [ -d "$dev" ]; count=$((count+1)); [ "$count" -le 1 ]; value device "${dev##*/}"
done
[ "$count" -eq 1 ]
for event in adreno_cmdbatch_queued adreno_cmdbatch_submitted kgsl_context_create kgsl_context_detach kgsl_context_destroy; do
  block "format/$event" "/sys/kernel/tracing/events/kgsl/$event/format"
  block "enabled/$event" "/sys/kernel/tracing/events/kgsl/$event/enable"
  block "filter/$event" "/sys/kernel/tracing/events/kgsl/$event/filter"
  block "trigger/$event" "/sys/kernel/tracing/events/kgsl/$event/trigger"
done
for event in sched_process_fork sched_process_exit; do
  block "format/$event" "/sys/kernel/tracing/events/sched/$event/format"
  block "enabled/$event" "/sys/kernel/tracing/events/sched/$event/enable"
  block "filter/$event" "/sys/kernel/tracing/events/sched/$event/filter"
  block "trigger/$event" "/sys/kernel/tracing/events/sched/$event/trigger"
done
block trace_clock /sys/kernel/tracing/trace_clock
[ "$(wc -c < /sys/kernel/tracing/set_event_pid)" -le 65536 ]
value event_pid "EMPTY$(cat /sys/kernel/tracing/set_event_pid)"
[ "$(wc -c < /sys/kernel/tracing/set_event_notrace_pid)" -le 65536 ]
value event_notrace_pid "EMPTY$(cat /sys/kernel/tracing/set_event_notrace_pid)"
block tracing_cpumask /sys/kernel/tracing/tracing_cpumask
count=0
for cpu in /sys/kernel/tracing/per_cpu/cpu[0-9]*; do
  [ -d "$cpu" ]; count=$((count+1)); [ "$count" -le 64 ]; block "stats/${cpu##*/}" "$cpu/stats"
done
[ "$count" -gt 0 ]
block boot_end /proc/sys/kernel/random/boot_id
block uptime_end /proc/uptime
printf '#end\n'
