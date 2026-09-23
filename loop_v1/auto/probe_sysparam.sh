#!/bin/sh
# probe_sysparam.sh — 设备端执行, 需 root。只读探明「哪些系统参数真能改」。
#
# 白名单不是拍脑袋列出来的: 每个候选节点都要在真机上过三关才准进,
#   1) 存在且可读
#   2) 可写 (把当前值原样写回去, 不改变任何状态)
#   3) 写了真生效 —— 写一个不同的合法值, 等 1s 回读, 看内核有没有把它退回去
# 第 3 关是关键: 很多 sysfs 节点接受 write() 但随即被驱动/热策略夹回原值,
# 只看 write 的返回码会得出"能改"的假结论。
#
# 本脚本每次改动都在同一个函数里立刻还原, 退出时不留任何痕迹
# (loop_v1 判据 4 的 device_snapshot.sh 会复核)。
set -u

KGSL=/sys/class/kgsl/kgsl-3d0
# kgsl 的 devfreq 在本机不是 $KGSL/devfreq, 而是挂在 /sys/class/devfreq 下。
# 别写死一条路径 —— 换个芯片或内核版本名字就变了, 探不到就如实报 absent。
DEVFREQ=""
for d in "$KGSL/devfreq" /sys/class/devfreq/*kgsl-3d0 /sys/class/devfreq/*kgsl*; do
  if [ -d "$d" ]; then DEVFREQ="$d"; break; fi
done
BUS=/sys/devices/system/cpu/bus_dcvs
CPU=/sys/devices/system/cpu/cpufreq

echo "# probe_sysparam v1"

# ── effect_test <key> <path> <试写值> ──
# 写一个与当前不同的合法值, 等 1s 回读:
#   live     : 值稳住了 (内核接受并保持)
#   rejected : 被退回原值
#   nowrite  : write 本身失败
# 无论结果如何都把原值写回去, 并再回读确认还原成功。
# 变量名一律加 et_ 前缀: sh 的函数没有局部作用域, 用 p/key 这种短名会把调用方
# `for p in .../policy*` 的循环变量当场改掉, 之后每一项读的都是拼错的路径
# (而且失败得很安静 —— 只会看到一串 unreadable)。
effect_test() {
  et_key="$1"; et_p="$2"; et_val="$3"
  old=$(cat "$et_p" 2>/dev/null)
  if [ -z "$old" ]; then
    printf '%s.effect=unreadable\n' "$et_key"
    return
  fi
  if [ "$old" = "$et_val" ]; then
    printf '%s.effect=skipped_same_value\n' "$et_key"
    return
  fi
  if ! echo "$et_val" > "$et_p" 2>/dev/null; then
    printf '%s.effect=nowrite\n' "$et_key"
    return
  fi
  sleep 1
  back=$(cat "$et_p" 2>/dev/null)
  echo "$old" > "$et_p" 2>/dev/null
  sleep 1
  rst=$(cat "$et_p" 2>/dev/null)
  if [ "$back" = "$et_val" ]; then
    printf '%s.effect=live\n' "$et_key"
  else
    printf '%s.effect=rejected(wrote=%s readback=%s)\n' "$et_key" "$et_val" "$back"
  fi
  if [ "$rst" = "$old" ]; then
    printf '%s.effect_restored=yes\n' "$et_key"
  else
    printf '%s.effect_restored=NO(want=%s got=%s)\n' "$et_key" "$et_val" "$rst"
  fi
}

# ── writable_test: 把当前值原样写回, 不改变状态 ──
writable_test() {
  wt_key="$1"; wt_p="$2"
  cur=$(cat "$wt_p" 2>/dev/null)
  if [ -z "$cur" ]; then printf '%s.writable=unreadable\n' "$wt_key"; return; fi
  if echo "$cur" > "$wt_p" 2>/dev/null; then
    printf '%s.writable=yes\n' "$wt_key"
  else
    printf '%s.writable=no\n' "$wt_key"
  fi
}

# ════ CPU 各簇 ════
pols=""
for pol_dir in "$CPU"/policy*; do
  [ -d "$pol_dir" ] || continue
  n=$(basename "$pol_dir" | sed 's/policy//')
  pols="$pols$n,"
done
printf 'cpu.policies=%s\n' "$(echo "$pols" | sed 's/,$//')"

for pol_dir in "$CPU"/policy*; do
  [ -d "$pol_dir" ] || continue
  n=$(basename "$pol_dir" | sed 's/policy//')
  k="cpu.policy$n"
  printf '%s.cpus=%s\n'       "$k" "$(cat "$pol_dir/related_cpus" 2>/dev/null)"
  printf '%s.avail_freqs=%s\n' "$k" "$(cat "$pol_dir/scaling_available_frequencies" 2>/dev/null)"
  printf '%s.avail_governors=%s\n' "$k" "$(cat "$pol_dir/scaling_available_governors" 2>/dev/null)"
  printf '%s.cpuinfo_min=%s\n' "$k" "$(cat "$pol_dir/cpuinfo_min_freq" 2>/dev/null)"
  printf '%s.cpuinfo_max=%s\n' "$k" "$(cat "$pol_dir/cpuinfo_max_freq" 2>/dev/null)"
  printf '%s.scaling_min_freq.cur=%s\n' "$k" "$(cat "$pol_dir/scaling_min_freq" 2>/dev/null)"
  printf '%s.scaling_max_freq.cur=%s\n' "$k" "$(cat "$pol_dir/scaling_max_freq" 2>/dev/null)"
  printf '%s.scaling_governor.cur=%s\n' "$k" "$(cat "$pol_dir/scaling_governor" 2>/dev/null)"
  printf '%s.scaling_cur_freq=%s\n' "$k" "$(cat "$pol_dir/scaling_cur_freq" 2>/dev/null)"
  writable_test "$k.scaling_min_freq" "$pol_dir/scaling_min_freq"
  writable_test "$k.scaling_max_freq" "$pol_dir/scaling_max_freq"
  writable_test "$k.scaling_governor" "$pol_dir/scaling_governor"
  # 生效测试的试写值必须落在内核允许的区间里。本机 policy0 的 scaling_max_freq
  # 只有 1785600 (上限 3628800 的一半, 厂商自己压的), 拿「第二高的可用频点」去试
  # min, 内核会直接夹回 max —— 那测出来的是 min>max 被夹, 不是「这个节点写不动」。
  #   min 试: <= 当前 max 的最高档, 且 != 当前 min
  #   max 试: >= 当前 min 的最低档, 且 != 当前 max
  avail=$(cat "$pol_dir/scaling_available_frequencies" 2>/dev/null | tr ' ' '\n' | grep -E '^[0-9]+$' | sort -n)
  cmin=$(cat "$pol_dir/scaling_min_freq" 2>/dev/null)
  cmax=$(cat "$pol_dir/scaling_max_freq" 2>/dev/null)
  f2=$(echo "$avail" | awk -v hi="$cmax" -v cur="$cmin" '$1<=hi && $1!=cur' | tail -1)
  [ -n "$f2" ] && effect_test "$k.scaling_min_freq" "$pol_dir/scaling_min_freq" "$f2"
  g2=$(echo "$avail" | awk -v lo="$cmin" -v cur="$cmax" '$1>=lo && $1!=cur' | head -1)
  [ -n "$g2" ] && effect_test "$k.scaling_max_freq" "$pol_dir/scaling_max_freq" "$g2"
  # governor 试一个与当前不同的可用值
  curgov=$(cat "$pol_dir/scaling_governor" 2>/dev/null)
  othergov=$(cat "$pol_dir/scaling_available_governors" 2>/dev/null | tr ' ' '\n' | grep -v '^$' | grep -vx "$curgov" | head -1)
  [ -n "$othergov" ] && effect_test "$k.scaling_governor" "$pol_dir/scaling_governor" "$othergov"
done

# ════ GPU (kgsl) ════
printf 'gpu.devfreq_path=%s\n' "$DEVFREQ"
printf 'gpu.available_frequencies=%s\n' "$(cat "$KGSL/gpu_available_frequencies" 2>/dev/null)"
printf 'gpu.model=%s\n'        "$(cat "$KGSL/gpu_model" 2>/dev/null)"
printf 'gpu.num_pwrlevels=%s\n' "$(cat "$KGSL/num_pwrlevels" 2>/dev/null)"
printf 'gpu.min_pwrlevel.cur=%s\n' "$(cat "$KGSL/min_pwrlevel" 2>/dev/null)"
printf 'gpu.max_pwrlevel.cur=%s\n' "$(cat "$KGSL/max_pwrlevel" 2>/dev/null)"
printf 'gpu.thermal_pwrlevel=%s\n' "$(cat "$KGSL/thermal_pwrlevel" 2>/dev/null)"
printf 'gpu.devfreq.avail_freqs=%s\n' "$(cat "$DEVFREQ/available_frequencies" 2>/dev/null)"
printf 'gpu.devfreq.avail_governors=%s\n' "$(cat "$DEVFREQ/available_governors" 2>/dev/null)"
printf 'gpu.devfreq.min_freq.cur=%s\n' "$(cat "$DEVFREQ/min_freq" 2>/dev/null)"
printf 'gpu.devfreq.max_freq.cur=%s\n' "$(cat "$DEVFREQ/max_freq" 2>/dev/null)"
printf 'gpu.devfreq.governor.cur=%s\n' "$(cat "$DEVFREQ/governor" 2>/dev/null)"
writable_test gpu.min_pwrlevel      "$KGSL/min_pwrlevel"
writable_test gpu.max_pwrlevel      "$KGSL/max_pwrlevel"
writable_test gpu.devfreq.min_freq  "$DEVFREQ/min_freq"
writable_test gpu.devfreq.max_freq  "$DEVFREQ/max_freq"
writable_test gpu.devfreq.governor  "$DEVFREQ/governor"
# min_pwrlevel 语义: 0 = 最快档, 数字越大越慢。往 0 方向走 = 抬高频率下限。
mpl=$(cat "$KGSL/min_pwrlevel" 2>/dev/null)
if [ -n "$mpl" ] && [ "$mpl" -gt 0 ] 2>/dev/null; then
  effect_test gpu.min_pwrlevel "$KGSL/min_pwrlevel" "$((mpl - 1))"
fi
# max_pwrlevel 语义同上: 恒有 max_pwrlevel <= min_pwrlevel, 所以只能往 min 的方向试
xpl=$(cat "$KGSL/max_pwrlevel" 2>/dev/null)
if [ -n "$xpl" ] && [ -n "$mpl" ] && [ "$xpl" -lt "$mpl" ] 2>/dev/null; then
  effect_test gpu.max_pwrlevel "$KGSL/max_pwrlevel" "$((xpl + 1))"
fi
gmin=$(cat "$DEVFREQ/min_freq" 2>/dev/null)
g2=$(cat "$DEVFREQ/available_frequencies" 2>/dev/null | tr ' ' '\n' | grep -E '^[0-9]+$' | sort -n | tail -2 | head -1)
if [ -n "$g2" ] && [ "$g2" != "$gmin" ]; then
  effect_test gpu.devfreq.min_freq "$DEVFREQ/min_freq" "$g2"
fi
gmax=$(cat "$DEVFREQ/max_freq" 2>/dev/null)
g3=$(cat "$DEVFREQ/available_frequencies" 2>/dev/null | tr ' ' '\n' | grep -E '^[0-9]+$' | sort -n | head -2 | tail -1)
if [ -n "$g3" ] && [ "$g3" != "$gmax" ] && [ "$g3" != "$gmin" ]; then
  effect_test gpu.devfreq.max_freq "$DEVFREQ/max_freq" "$g3"
fi
curggov=$(cat "$DEVFREQ/governor" 2>/dev/null)
oggov=$(cat "$DEVFREQ/available_governors" 2>/dev/null | tr ' ' '\n' | grep -v '^$' | grep -vx "$curggov" | head -1)
[ -n "$oggov" ] && effect_test gpu.devfreq.governor "$DEVFREQ/governor" "$oggov"

# ════ 总线 (DDR / LLCC) ════
for n in DDR LLCC; do
  [ -d "$BUS/$n" ] || { printf 'bus.%s=absent\n' "$n"; continue; }
  printf 'bus.%s.boost_freq.cur=%s\n' "$n" "$(cat "$BUS/$n/boost_freq" 2>/dev/null)"
  printf 'bus.%s.hw_min_freq=%s\n'    "$n" "$(cat "$BUS/$n/hw_min_freq" 2>/dev/null)"
  printf 'bus.%s.hw_max_freq=%s\n'    "$n" "$(cat "$BUS/$n/hw_max_freq" 2>/dev/null)"
  printf 'bus.%s.cur_freq=%s\n'       "$n" "$(cat "$BUS/$n/cur_freq" 2>/dev/null)"
  printf 'bus.%s.avail_freqs=%s\n'    "$n" "$(cat "$BUS/$n/available_frequencies" 2>/dev/null)"
  writable_test "bus.$n.boost_freq" "$BUS/$n/boost_freq"
  b=$(cat "$BUS/$n/boost_freq" 2>/dev/null)
  hm=$(cat "$BUS/$n/hw_max_freq" 2>/dev/null)
  if [ -n "$hm" ] && [ "$b" != "$hm" ]; then
    effect_test "bus.$n.boost_freq" "$BUS/$n/boost_freq" "$hm"
  fi
done

# ════ 刷新率 ════
# AOSP 的 peak_refresh_rate / min_refresh_rate 在本机是 null —— 红魔用自己的
# refresh_rate_mode (取值表在 system:all_refresh_rate 里, 形如 auto,60,90,120,144)。
# 厂商键的取值语义没有文档, 所以不能凭 all_refresh_rate 的下标猜: 逐个写进去,
# 看 SurfaceFlinger 的活动模式 fps 到底有没有跟着变, 变了才算数, 完了写回原值。
printf 'setting.system.peak_refresh_rate.cur=%s\n' "$(settings get system peak_refresh_rate 2>/dev/null)"
printf 'setting.system.min_refresh_rate.cur=%s\n'  "$(settings get system min_refresh_rate 2>/dev/null)"
printf 'setting.system.all_refresh_rate=%s\n' "$(settings get system all_refresh_rate 2>/dev/null)"
printf 'setting.system.refresh_rate_mode.cur=%s\n' "$(settings get system refresh_rate_mode 2>/dev/null)"
printf 'display.modes=%s\n' "$(dumpsys display 2>/dev/null | grep -oE 'fps=[0-9]+' | sort -u | tr '\n' ',' | sed 's/,$//')"

active_fps() {
  dumpsys display 2>/dev/null | grep -m1 'mActiveSfDisplayMode=' \
    | grep -oE 'peakRefreshRate=[0-9]+' | head -1 | cut -d= -f2
}
rrm_old=$(settings get system refresh_rate_mode 2>/dev/null)
rrm_vals=$(settings get system all_refresh_rate 2>/dev/null | tr ',' '\n' | grep -c .)
printf 'setting.system.refresh_rate_mode.base_fps=%s\n' "$(active_fps)"
if [ -n "$rrm_old" ] && [ "$rrm_old" != "null" ] && [ "$rrm_vals" -gt 1 ] 2>/dev/null; then
  i=0
  while [ "$i" -lt "$rrm_vals" ]; do
    if [ "$i" != "$rrm_old" ]; then
      settings put system refresh_rate_mode "$i" 2>/dev/null
      sleep 2
      printf 'setting.system.refresh_rate_mode.mode%s_fps=%s (readback=%s)\n' \
        "$i" "$(active_fps)" "$(settings get system refresh_rate_mode 2>/dev/null)"
    fi
    i=$((i + 1))
  done
  settings put system refresh_rate_mode "$rrm_old" 2>/dev/null
  sleep 2
  printf 'setting.system.refresh_rate_mode.restored=%s fps=%s\n' \
    "$(settings get system refresh_rate_mode 2>/dev/null)" "$(active_fps)"
fi

# ════ 限帧 / 游戏空间相关控制点 (只读扫描, 不写) ════
for key in game_driver_all_apps ANGLE_gl_driver_all_angle; do
  printf 'framecap.global.%s=%s\n' "$key" "$(settings get global "$key" 2>/dev/null)"
done
printf 'framecap.gamemanager=%s\n' "$(dumpsys game 2>/dev/null | head -20 | tr '\n' ';' | sed 's/;$//')"
printf 'framecap.nubia_props=%s\n' "$(getprop 2>/dev/null | grep -iE 'game|fps|frame' | head -20 | tr '\n' ';' | sed 's/;$//')"

# ════ 功耗轨 ════
for rail in battery usb; do
  d=/sys/class/power_supply/$rail
  printf 'power.%s.voltage_now=%s\n' "$rail" "$(cat "$d/voltage_now" 2>/dev/null)"
  printf 'power.%s.current_now=%s\n' "$rail" "$(cat "$d/current_now" 2>/dev/null)"
  printf 'power.%s.power_now=%s\n'   "$rail" "$(cat "$d/power_now" 2>/dev/null)"
  printf 'power.%s.status=%s\n'      "$rail" "$(cat "$d/status" 2>/dev/null)"
done

# ════ 主动散热风扇 (只读记录, 永不进白名单) ════
# 红魔内置风扇。它自己也耗电, 会进整机功耗读数, 所以必须把当轮的风扇状态记进证据:
# 同一组对照的两臂要是同一风扇状态, 风扇开启前后的数据不能混在一起比。
# 风扇转速本身是个系统参数, 但**不进自动调参白名单** —— DENY_KEYWORDS 里的 "fan"
# 在主机端拦一道, knob_sysparam.sh 的 denied() 在设备端再拦一道。
for f in /sys/kernel/fan/* /sys/class/hwmon/hwmon*/fan1_input /sys/class/hwmon/hwmon*/pwm1; do
  [ -f "$f" ] || continue
  printf 'fan.%s=%s\n' "$(echo "$f" | tr '/' '_')" "$(cat "$f" 2>/dev/null)"
done
printf 'fan.props=%s\n' "$(getprop 2>/dev/null | grep -i fan | head -10 | tr '\n' ';' | sed 's/;$//')"

# ════ 热区 ════
for z in /sys/class/thermal/thermal_zone*; do
  [ -d "$z" ] || continue
  printf 'thermal.%s=%s\n' "$(cat "$z/type" 2>/dev/null)" "$(cat "$z/temp" 2>/dev/null)"
done

echo "# end"
