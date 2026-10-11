#!/system/bin/sh
# Owned remote worker. All paths/values originate in frozen receiver plans.
set -eu
D=${1:?owned directory}; TOKEN=${2:?token}; BOOT=${3:?boot}; MODE=${4:?mode}; RUN=${5:-none}
case "$D" in /data/local/tmp/phonefarm-confirmation-*) ;; *) exit 70 ;; esac
case "$TOKEN:$RUN" in *[!a-zA-Z0-9_:-]*) exit 70 ;; esac
[ "$(cat "$D/owner")" = "$TOKEN $BOOT" ] || exit 71
[ "$(cat /proc/sys/kernel/random/boot_id)" = "$BOOT" ] || exit 72
[ ! -e "$D/session.closed" ] || exit 86
R="$D/$RUN"
intent() {
  [ ! -e "$D/session.closed" ] || exit 86
  [ "$(cat "$D/owner")" = "$TOKEN $BOOT" ] || exit 71
  [ "$(cat /proc/sys/kernel/random/boot_id)" = "$BOOT" ] || exit 72
  printf '%s\t%s\t%s\t%s\n' "$TOKEN" "$1" "$2" "${3:-}" >> "$D/journal.tsv"
  sync
}
read_value() {
  if [ "$1" = setting ]; then settings get system "${2#system:}"; else cat "$2"; fi
}
write_value() {
  kind=$1; path=$2; value=$3
  case "$path" in *thermal*|*cooling*|*trip_point*|*fan*|*bcl*|*throttl*) exit 73 ;; esac
  case "$kind:$path" in
    sysfs:/sys/devices/system/cpu/cpufreq/policy*/scaling_governor|sysfs:/sys/devices/system/cpu/cpufreq/policy*/scaling_min_freq|sysfs:/sys/devices/system/cpu/cpufreq/policy*/scaling_max_freq|sysfs:/sys/class/kgsl/kgsl-3d0/min_pwrlevel|sysfs:/sys/class/kgsl/kgsl-3d0/max_pwrlevel|sysfs:/sys/class/kgsl/kgsl-3d0/devfreq/governor|sysfs:/sys/class/kgsl/kgsl-3d0/devfreq/min_freq|sysfs:/sys/class/kgsl/kgsl-3d0/devfreq/max_freq|sysfs:/sys/devices/system/cpu/bus_dcvs/DDR/boost_freq|sysfs:/sys/devices/system/cpu/bus_dcvs/LLCC/boost_freq) ;;
    setting:system:peak_refresh_rate|setting:system:min_refresh_rate|setting:system:refresh_rate_mode) ;;
    *) exit 73 ;;
  esac
  intent write "$path" "$value" || return 1
  if [ "$kind" = setting ]; then
    if [ "$value" = null ]; then settings delete system "${path#system:}" >/dev/null; else settings put system "${path#system:}" "$value"; fi
  else printf '%s\n' "$value" > "$path"; fi
}
# Re-read current bounds after governor changes. Legal endpoint order depends on direction.
target_value() {
  wanted=''
  if [ "$PLAN" != - ]; then wanted=$(awk -F '\t' -v p="$2" '$3==p {print $4}' "$PLAN"); fi
  if [ -n "$wanted" ]; then printf '%s' "$wanted"; else awk -F '\t' -v p="$2" '$2==p {print $3}' "$D/restore.tsv"; fi
}
bounds_group() {
  low=$1; high=$2
  grep -F "$(printf '\t')$low$(printf '\t')" "$D/restore.tsv" >/dev/null || return 0
  lo=$(target_value sysfs "$low"); hi=$(target_value sysfs "$high")
  curlo=$(read_value sysfs "$low"); curhi=$(read_value sysfs "$high")
  case "$lo:$hi:$curlo:$curhi" in *[!0-9:]*) return 1 ;; esac
  [ -n "$lo" ] && [ -n "$hi" ] && [ -n "$curlo" ] && [ -n "$curhi" ] || return 1
  [ "$lo" -le "$hi" ] && [ "$curlo" -le "$curhi" ] || return 1
  if [ "$hi" -lt "$curlo" ]; then
    write_value sysfs "$low" "$lo" || return 1
    write_value sysfs "$high" "$hi" || return 1
  else
    write_value sysfs "$high" "$hi" || return 1
    write_value sysfs "$low" "$lo" || return 1
  fi
  [ "$(read_value sysfs "$low")" = "$lo" ] && [ "$(read_value sysfs "$high")" = "$hi" ]
}
refresh_group() {
  grep -F "$(printf '\t')system:peak_refresh_rate$(printf '\t')" "$D/restore.tsv" >/dev/null || return 0
  lo=$(target_value setting system:min_refresh_rate); hi=$(target_value setting system:peak_refresh_rate)
  current=$(read_value setting system:min_refresh_rate)
  if [ "$lo" = null ] || [ "$hi" = null ]; then
    write_value setting system:min_refresh_rate "$lo" || return 1
    write_value setting system:peak_refresh_rate "$hi" || return 1
  else
    [ "$current" != null ] || current=0
    awk -v a="$lo" -v b="$hi" 'BEGIN {exit !(a<=b)}' || return 1
    if awk -v a="$hi" -v b="$current" 'BEGIN {exit !(a<b)}'; then
      write_value setting system:min_refresh_rate "$lo" || return 1
      write_value setting system:peak_refresh_rate "$hi" || return 1
    else
      write_value setting system:peak_refresh_rate "$hi" || return 1
      write_value setting system:min_refresh_rate "$lo" || return 1
    fi
  fi
}
write_graph() {
  PLAN=$1
  while IFS="$(printf '\t')" read -r kind path old; do
    case "$path" in */scaling_governor|*/devfreq/governor) write_value "$kind" "$path" "$(target_value "$kind" "$path")" || return 1 ;; esac
  done < "$D/restore.tsv"
  for base in /sys/devices/system/cpu/cpufreq/policy*; do
    [ -d "$base" ] || continue
    bounds_group "$base/scaling_min_freq" "$base/scaling_max_freq" || return 1
  done
  bounds_group /sys/class/kgsl/kgsl-3d0/devfreq/min_freq /sys/class/kgsl/kgsl-3d0/devfreq/max_freq || return 1
  # KGSL powerlevel numbers run backwards: max is the numeric lower endpoint.
  bounds_group /sys/class/kgsl/kgsl-3d0/max_pwrlevel /sys/class/kgsl/kgsl-3d0/min_pwrlevel || return 1
  refresh_group || return 1
  while IFS="$(printf '\t')" read -r kind path old; do
    [ -n "$kind" ] || continue
    case "$path" in system:peak_refresh_rate|system:min_refresh_rate|*/scaling_governor|*/scaling_min_freq|*/scaling_max_freq|*/devfreq/governor|*/devfreq/min_freq|*/devfreq/max_freq|*/min_pwrlevel|*/max_pwrlevel) continue ;; esac
    write_value "$kind" "$path" "$(target_value "$kind" "$path")" || return 1
  done < "$D/restore.tsv"
  while IFS="$(printf '\t')" read -r kind path old; do
    [ -n "$kind" ] || continue
    [ "$(read_value "$kind" "$path")" = "$(target_value "$kind" "$path")" ] || return 1
  done < "$D/restore.tsv"
}
restore_controls() { write_graph -; }
trace_write() { intent trace_write "$1" "$2" || return 1; printf '%s\n' "$2" > "/sys/kernel/tracing/$1"; }
restore_trace() {
  trace_write tracing_on 0 || return 1
  trace_write current_tracer "$(cat "$D/ftrace-current_tracer")" || return 1
  trace_write buffer_size_kb "$(cat "$D/ftrace-buffer_size_kb")" || return 1
  trace_write set_event '' || return 1
  while IFS= read -r event; do [ -z "$event" ] || { intent trace_event set_event "$event" || return 1; printf '%s\n' "$event" >> /sys/kernel/tracing/set_event || return 1; }; done < "$D/ftrace-set_event"
  trace_write tracing_on "$(cat "$D/ftrace-tracing_on")" || return 1
}
snapshot() {
  name=$1
  if sh "$D/snapshot.sh" > "$R/$name.txt" 2> "$R/$name.stderr"; then echo 0 > "$R/$name.exit"; else code=$?; echo "$code" > "$R/$name.exit"; return "$code"; fi
}
start_ticks() { sed 's/^.*) //' "/proc/$1/stat" 2>/dev/null | awk '{print $20}'; }
group_id() { sed 's/^.*) //' "/proc/$1/stat" 2>/dev/null | awk '{print $3}'; }
start_child() {
  tag=$1; script=$2; shift 2
  intent launch_owned_child "$script" "$tag" || return 1
  printf '%s\t%s\n' "$tag" "$script" > "$R/launch-$tag.pending" || return 1
  sync || return 1
  setsid sh "$D/bootstrap.sh" "$D" "$TOKEN" "$BOOT" "$R" "$tag" "$script" "$@" > "$R/$tag.log" 2> "$R/$tag.stderr" &
  pid=$!; ticks=''; n=0
  while [ "$n" -lt 20 ]; do
    ticks=$(start_ticks "$pid")
    [ -n "$ticks" ] && [ "$(group_id "$pid")" = "$pid" ] && break
    n=$((n+1)); sleep .05
  done
  [ -n "$ticks" ] && [ "$(group_id "$pid")" = "$pid" ] || return 1
  printf '%s\t%s\t%s\t%s\n' "$tag" "$pid" "$ticks" "$script" >> "$R/children.tsv"
  sync || return 1
  intent permit_owned_child "$script" "$tag" || return 1
  [ ! -e "$R/permit-$tag" ] && [ ! -e "$R/permit-$tag.tmp" ] || return 1
  printf '%s\n' "$TOKEN $BOOT" > "$R/permit-$tag.tmp" || return 1
  sync || return 1
  mv "$R/permit-$tag.tmp" "$R/permit-$tag" || return 1
  sync || return 1
  intent clear_pending "$script" "$tag" || return 1
  rm "$R/launch-$tag.pending" || return 1
  sync || return 1
  CHILD_PID=$pid
}
pending_known() {
  for pending in "$R"/launch-*.pending; do
    [ -f "$pending" ] || continue
    [ -s "$pending" ] || return 1
    [ -f "$R/children.tsv" ] || return 1
    while IFS="$(printf '\t')" read -r tag script; do
      [ "$(awk -F '\t' -v t="$tag" -v s="$script" '$1==t && $4==s {n++} END {print n+0}' "$R/children.tsv")" = 1 ] || return 1
    done < "$pending"
  done
}
group_absent_remote() {
  expected=$1
  for proc in /proc/[0-9]*; do
    [ -d "$proc" ] || continue
    pg=$(group_id "${proc##*/}")
    if [ -z "$pg" ]; then [ ! -d "$proc" ] || return 1; continue; fi
    [ "$pg" != "$expected" ] || return 1
  done
  if error=$(/system/bin/kill -0 -- "-$expected" 2>&1); then return 1; fi
  case "$error" in *"No such process"*) ;; *) return 1 ;; esac
}
quiescent() {
  pending_known || return 1
  [ -f "$R/children.tsv" ] || return 0
  while IFS="$(printf '\t')" read -r tag pid ticks script; do
    if [ -d "/proc/$pid" ]; then return 1; fi
    # A dead leader with a surviving group cannot be safely attributed from PID alone.
    group_absent_remote "$pid" || return 1
  done < "$R/children.tsv"
}
stop_owned() {
  pending_known || return 1
  [ -f "$R/children.tsv" ] || return 0
  while IFS="$(printf '\t')" read -r tag pid ticks script; do
    if [ -d "/proc/$pid" ]; then
      [ "$(start_ticks "$pid")" = "$ticks" ] && [ "$(group_id "$pid")" = "$pid" ] || return 1
      cmd=$(tr '\000' ' ' < "/proc/$pid/cmdline")
      case "$cmd" in *"$script"*) ;; *) return 1 ;; esac
      intent stop_owned "$pid" "$ticks" || return 1
      kill -TERM -- "-$pid" || return 1
      wait "$pid" 2>/dev/null || true
    fi
  done < "$R/children.tsv"
  n=0; while [ "$n" -lt 50 ]; do quiescent && return 0; n=$((n+1)); sleep .1; done
  return 1
}
apply_plan() {
  restore_controls || return 1
  write_graph "$1"
}
capture() {
  dur=$1
  trace_write tracing_on 0 || return 1
  trace_write current_tracer nop || return 1
  trace_write buffer_size_kb 65536 || return 1
  trace_write trace '' || return 1
  for event in adreno_cmdbatch_submitted adreno_cmdbatch_retired kgsl_buslevel kgsl_pwrlevel kgsl_gpu_frequency kgsl_thermal_constraint kgsl_clock_throttling kgsl_bcl_clock_throttling; do
    if [ -f "/sys/kernel/tracing/events/kgsl/$event/enable" ]; then trace_write "events/kgsl/$event/enable" 1 || return 1; fi
  done
  [ -f /sys/kernel/tracing/events/kgsl/adreno_cmdbatch_submitted/enable ] || return 1
  [ -f /sys/kernel/tracing/events/kgsl/adreno_cmdbatch_retired/enable ] || return 1
  trace_write tracing_on 1 || return 1
  a=$(cut -d' ' -f1 /proc/uptime) || return 1
  sleep "$dur" || return 1
  trace_write tracing_on 0 || return 1
  b=$(cut -d' ' -f1 /proc/uptime) || return 1
  { printf '#loop_v1_meta capture_start_uptime=%s capture_end_uptime=%s duration_req=%s\n' "$a" "$b" "$dur"; cat /sys/kernel/tracing/trace; } > "$R/trace.txt"
}
cleanup_failure() {
  trap - EXIT INT TERM
  # The absence of an attributable child/group is required before any restore write.
  if stop_owned; then restore_trace && restore_controls || true; fi
}
acquire_master() {
  [ ! -e "$D/session.closed" ] || return 1
  L="$D/worker.lock"
  if [ -d "$L" ]; then
    [ "$MODE" = reconcile ] || return 1
    [ -f "$L/owner" ] || return 1
    IFS=' ' read -r oldtoken oldboot oldpid oldticks oldrun < "$L/owner" || return 1
    [ "$oldtoken" = "$TOKEN" ] && [ "$oldboot" = "$BOOT" ] || return 1
    case "$oldpid:$oldticks:$oldrun" in *[!a-zA-Z0-9_:-]*) return 1 ;; esac
    case "$oldpid:$oldticks" in *[!0-9:]*) return 1 ;; esac
    [ "$oldpid" -gt 0 ] && [ "$oldticks" -gt 0 ] || return 1
    # A live/reused PID or surviving group is unknown; no recovery writes may race it.
    [ ! -d "/proc/$oldpid" ] && group_absent_remote "$oldpid" || return 1
    intent reconcile_dead_master "$oldpid" "$oldticks" || return 1
    rm "$L/owner" && rmdir "$L" && sync || return 1
  fi
  intent acquire_master "$L" "$$" || return 1
  mkdir "$L" || return 1
  [ ! -e "$D/session.closed" ] || return 1
  ticks=$(start_ticks "$$"); group=$(group_id "$$")
  [ -n "$ticks" ] && [ "$group" = "$$" ] || return 1
  MASTER="$TOKEN $BOOT $$ $ticks $RUN"
  printf '%s\n' "$MASTER" > "$L/owner" || return 1
  sync || return 1
}
release_master() {
  [ "$(cat "$D/worker.lock/owner")" = "$MASTER" ] || return 1
  if [ -e "$D/session.closed" ]; then
    # Closure forbids all normal intents. Only this matching terminal reconcile may unlock.
    [ "$MODE" = reconcile ] && [ "$(cat "$D/session.closed/owner")" = "$TOKEN $BOOT $RUN" ] || return 1
    [ "$(cat "$D/owner")" = "$TOKEN $BOOT" ] && [ "$(cat /proc/sys/kernel/random/boot_id)" = "$BOOT" ] || return 1
    printf '%s\tterminal_release_master\t%s\t%s\n' "$TOKEN" "$D/worker.lock" "$$" >> "$D/journal.tsv"
    sync || return 1
  else
    intent release_master "$D/worker.lock" "$$" || return 1
  fi
  rm "$D/worker.lock/owner" && rmdir "$D/worker.lock" && sync
}
close_session() {
  [ "$(cat "$D/worker.lock/owner")" = "$MASTER" ] || return 1
  intent revoke_session "$D/session.closed" "$RUN" || return 1
  # Existence itself is a permanent fail-closed tombstone, even if the receipt write crashes.
  mkdir "$D/session.closed" || return 1
  sync || return 1
  printf '%s\n' "$TOKEN $BOOT $RUN" > "$D/session.closed/owner" || return 1
  sync || return 1
  printf 'SESSION_REVOKED %s %s %s\n' "$TOKEN" "$BOOT" "$RUN" > "$R/session_closed.txt" || return 1
  sync || return 1
}
# The host invokes this worker through setsid, so master and its synchronous children own one group.
acquire_master || exit 85
case "$MODE" in
  probe)
    intent mkdir "$R" ''; mkdir "$R"
    trap 'cleanup_failure' EXIT INT TERM
    snapshot snapshot_before
    apply_plan "$D/$RUN.plan.tsv"
    sleep 1
    while IFS="$(printf '\t')" read -r key kind path value; do
      [ "$(read_value "$kind" "$path")" = "$value" ] || exit 74
    done < "$D/$RUN.plan.tsv"
    sleep 10
    while IFS="$(printf '\t')" read -r key kind path value; do
      [ "$(read_value "$kind" "$path")" = "$value" ] || exit 75
      printf '%s.writable=yes\n%s.effect=live\n' "$key" "$key" >> "$R/probe_receipt.txt"
      if [ "$path" = system:refresh_rate_mode ]; then
        fps=$(dumpsys display | grep -m1 'mActiveSfDisplayMode=' | grep -oE 'peakRefreshRate=[0-9]+' | head -1 | cut -d= -f2 || true)
        printf 'setting.system.refresh_rate_mode.mode%s_fps=%s (readback=%s)\n' "$value" "$fps" "$(read_value "$kind" "$path")" >> "$R/probe_receipt.txt"
      fi
    done < "$D/$RUN.plan.tsv"
    restore_controls; restore_trace; snapshot snapshot_restored
    while IFS="$(printf '\t')" read -r key kind path value; do printf '%s.effect_restored=yes\n' "$key" >> "$R/probe_receipt.txt"; done < "$D/$RUN.plan.tsv"
    trap - EXIT INT TERM
    release_master
    echo PROBE_COMPLETE
    ;;
  sample)
    dur=${6:?capture seconds}; lead=${7:?lead}; envdur=${8:?ENV seconds}; role=${9:?role}
    intent mkdir "$R" ''; mkdir "$R"
    trap 'cleanup_failure' EXIT INT TERM
    id > "$R/root_id.txt"; cat /proc/sys/kernel/random/boot_id > "$R/boot_id.txt"
    snapshot snapshot_before
    [ "$role" = baseline ] || apply_plan "$D/candidate.plan.tsv"
    snapshot snapshot_at_run
    start_child workload "$D/workload.sh"; workload_pid=$CHILD_PID
    start_child sampler "$D/sample_env.sh" "$envdur" 1; sampler_pid=$CHILD_PID
    sleep "$lead"
    if capture "$dur" > "$R/capture.log" 2> "$R/capture.stderr"; then echo 0 > "$R/capture_exit.txt"; else code=$?; echo "$code" > "$R/capture_exit.txt"; exit "$code"; fi
    if wait "$workload_pid"; then echo 0 > "$R/workload_exit.txt"; else code=$?; echo "$code" > "$R/workload_exit.txt"; exit "$code"; fi
    if wait "$sampler_pid"; then echo 0 > "$R/sampler_exit.txt"; else code=$?; echo "$code" > "$R/sampler_exit.txt"; exit "$code"; fi
    quiescent || exit 76
    echo OWNED_GROUPS_ABSENT > "$R/quiescence.txt"
    restore_trace; snapshot snapshot_after_run
    restore_controls; snapshot snapshot_restored
    echo 0 > "$R/restore_exit.txt"
    trap - EXIT INT TERM
    release_master
    echo SAMPLE_COMPLETE
    ;;
  reconcile)
    # Every old run owns a separate process ledger. Unknown identities are never killed.
    for prior in "$D"/*; do
      [ -d "$prior" ] || continue
      [ "$prior" != "$D/worker.lock" ] || continue
      R=$prior; stop_owned || exit 77
    done
    restore_trace; restore_controls
    R="$D/$RUN"; intent mkdir "$R" ''; mkdir "$R"; snapshot snapshot_restored
    echo 0 > "$R/restore_exit.txt"
    echo OWNED_GROUPS_ABSENT > "$R/quiescence.txt"
    close_session
    release_master
    echo RECONCILE_COMPLETE
    ;;
  *) exit 70 ;;
esac
