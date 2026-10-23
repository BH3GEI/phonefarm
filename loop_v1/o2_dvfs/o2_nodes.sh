#!/system/bin/sh
# o2_nodes.sh — O2 锁帧降频实验的频率节点「记原值 / 核对 / 还原」(设备端, su 下跑)。
#
#   o2_nodes.sh save    <文件>   记下当前值。文件已存在就拒绝 (不覆盖任何已有记录)
#   o2_nodes.sh check   <文件>   当前值与记录逐行比对, 列出不同的键; 全同输出 O2_NODES_SAME
#   o2_nodes.sh restore <文件>   只把「本实验会写的键」写回记录值, 再核对一遍
#                                成功 O2_RESTORE_OK, 有键没回来 O2_RESTORE_FAIL <键...>
#   o2_nodes.sh release <文件>   撤掉约束: governor 写回记录值, scaling_max/min 写成硬件上下限
#                                (cpuinfo_max/min_freq), GPU 档位放开到 0..最低档。成功 O2_RELEASED
#   o2_nodes.sh show             只打印当前值
#
# 还原用 release, 不用 restore。sysfs 的这几个节点是厂商性能服务和我们共用的 (谁最后写谁生效),
# 厂商只在状态切换时重写。2026-10-22 红魔实测: restore 把发热限频时记下的 883/768 MHz 写回去后,
# 厂商不再改, CPU 上限在游戏里一直卡在这两个值上; 改成写硬件上下限、再重开游戏, 厂商重新下发了
# 游戏配置 (中核 1.23→3.63 GHz, 超大核 1.38→1.50 GHz 动态)。
#
# 会写的键: cpu.policyN.{scaling_governor,scaling_max_freq,scaling_min_freq}、
#           gpu.{max_pwrlevel,min_pwrlevel}。其余 (thermal_pwrlevel、max_gpuclk、总线、
#           刷新率、bind mount) 只记录、只核对, 从不写。
# 还原要在游戏停掉后做: 游戏在前台时厂商性能服务会再改这些节点。
# O2_SYSROOT=<目录> 时所有路径加这个前缀 (离线自测用假 sysfs)。
set -u

R="${O2_SYSROOT:-}"
CPU="$R/sys/devices/system/cpu/cpufreq"
KGSL="$R/sys/class/kgsl/kgsl-3d0"
BUS="$R/sys/devices/system/cpu/bus_dcvs"

rd() { cat "$1" 2>/dev/null | tr '\n' ' ' | sed 's/ *$//'; }

dump() {
  echo "# o2_nodes v1"
  for p in "$CPU"/policy*; do
    [ -d "$p" ] || continue
    n=$(basename "$p")
    echo "cpu.$n.scaling_governor=$(rd "$p/scaling_governor")"
    echo "cpu.$n.scaling_max_freq=$(rd "$p/scaling_max_freq")"
    echo "cpu.$n.scaling_min_freq=$(rd "$p/scaling_min_freq")"
    echo "ro.cpu.$n.cpuinfo_max_freq=$(rd "$p/cpuinfo_max_freq")"
  done
  echo "gpu.max_pwrlevel=$(rd "$KGSL/max_pwrlevel")"
  echo "gpu.min_pwrlevel=$(rd "$KGSL/min_pwrlevel")"
  echo "ro.gpu.thermal_pwrlevel=$(rd "$KGSL/thermal_pwrlevel")"
  echo "ro.gpu.max_gpuclk=$(rd "$KGSL/max_gpuclk")"
  echo "ro.gpu.default_pwrlevel=$(rd "$KGSL/default_pwrlevel")"
  for b in DDR LLCC; do
    echo "ro.bus.$b.boost_freq=$(rd "$BUS/$b/boost_freq")"
  done
  if [ -z "$R" ]; then
    echo "ro.setting.refresh_rate_mode=$(settings get system refresh_rate_mode 2>/dev/null)"
    # 本实验的 bind mount 全挂在 /data/local/tmp/o2 下的文件上; 记下来, 结束时必须一条不剩
    echo "ro.mounts.o2=$(grep -c '/data/local/tmp/o2' /proc/1/mountinfo 2>/dev/null)"
  fi
}

val_of() { grep "^$1=" "$2" 2>/dev/null | head -1 | cut -d= -f2-; }

# 写一个节点 (回读核对在最后统一做)
put() {
  echo "$2" > "$1" 2>/dev/null
}

restore_policy() {  # restore_policy <policy 目录> <记录文件>
  p="$1"; f="$2"; n=$(basename "$p")
  gov=$(val_of "cpu.$n.scaling_governor" "$f")
  mx=$(val_of "cpu.$n.scaling_max_freq" "$f")
  mn=$(val_of "cpu.$n.scaling_min_freq" "$f")
  # governor 先写: 切 governor 会重设 min/max (knob_sysparam.sh 的实测教训)
  if [ -n "$gov" ] && [ "$(rd "$p/scaling_governor")" != "$gov" ]; then put "$p/scaling_governor" "$gov"; fi
  # 次序保证任何时刻 min <= max: 目标 max 比当前 min 低就先写 min
  cur_mn=$(rd "$p/scaling_min_freq")
  if [ -n "$mx" ] && [ -n "$cur_mn" ] && [ "$mx" -lt "$cur_mn" ] 2>/dev/null; then
    [ -n "$mn" ] && put "$p/scaling_min_freq" "$mn"
    put "$p/scaling_max_freq" "$mx"
  else
    [ -n "$mx" ] && put "$p/scaling_max_freq" "$mx"
    [ -n "$mn" ] && put "$p/scaling_min_freq" "$mn"
  fi
}

restore_gpu() {  # kgsl: 恒有 max_pwrlevel <= min_pwrlevel (数字越大越慢)
  f="$1"
  mx=$(val_of gpu.max_pwrlevel "$f"); mn=$(val_of gpu.min_pwrlevel "$f")
  cur_mn=$(rd "$KGSL/min_pwrlevel")
  if [ -n "$mx" ] && [ -n "$cur_mn" ] && [ "$mx" -gt "$cur_mn" ] 2>/dev/null; then
    [ -n "$mn" ] && put "$KGSL/min_pwrlevel" "$mn"
    put "$KGSL/max_pwrlevel" "$mx"
  else
    [ -n "$mx" ] && put "$KGSL/max_pwrlevel" "$mx"
    [ -n "$mn" ] && put "$KGSL/min_pwrlevel" "$mn"
  fi
}

release_all() {  # release_all <记录文件>
  f="$1"
  for p in "$CPU"/policy*; do
    [ -d "$p" ] || continue
    n=$(basename "$p")
    gov=$(val_of "cpu.$n.scaling_governor" "$f")
    if [ -n "$gov" ] && [ "$(rd "$p/scaling_governor")" != "$gov" ]; then put "$p/scaling_governor" "$gov"; fi
    put "$p/scaling_max_freq" "$(rd "$p/cpuinfo_max_freq")"
    put "$p/scaling_min_freq" "$(rd "$p/cpuinfo_min_freq")"
  done
  nl=$(rd "$KGSL/num_pwrlevels")
  if [ -n "$nl" ]; then
    put "$KGSL/min_pwrlevel" "$((nl - 1))"
    put "$KGSL/max_pwrlevel" 0
  fi
  sleep 1
  bad=""
  for p in "$CPU"/policy*; do
    [ -d "$p" ] || continue
    [ "$(rd "$p/scaling_max_freq")" = "$(rd "$p/cpuinfo_max_freq")" ] || bad="$bad $(basename "$p").max=$(rd "$p/scaling_max_freq")"
    [ "$(rd "$p/scaling_min_freq")" = "$(rd "$p/cpuinfo_min_freq")" ] || bad="$bad $(basename "$p").min=$(rd "$p/scaling_min_freq")"
  done
  if [ -z "$bad" ]; then echo O2_RELEASED; else echo "O2_RELEASE_PARTIAL$bad (厂商服务可能已在改写, 记录在案)"; fi
}

diff_keys() {  # diff_keys <记录文件> [只看可写键=1]
  now=$(dump)
  grep -v '^#' "$1" | while IFS= read -r line; do
    k=${line%%=*}
    case "$k" in ro.*) [ "${2:-0}" = 1 ] && continue ;; esac
    want=${line#*=}
    got=$(echo "$now" | grep "^$k=" | head -1 | cut -d= -f2-)
    [ "$got" = "$want" ] || echo "$k want=$want got=$got"
  done
}

case "${1:-show}" in
  show) dump ;;
  release)
    f="${2:?缺文件名}"
    [ -f "$f" ] || { echo "O2_RELEASE_FAIL 记录文件不在: $f"; exit 2; }
    release_all "$f"
    ;;
  save)
    f="${2:?缺文件名}"
    if [ -e "$f" ]; then echo "O2_REFUSE $f 已存在, 不覆盖"; exit 2; fi
    dump > "$f" && echo "O2_SAVED $f"
    ;;
  check)
    f="${2:?缺文件名}"
    d=$(diff_keys "$f")
    if [ -z "$d" ]; then echo O2_NODES_SAME; else echo "$d"; echo O2_NODES_DIFF; fi
    ;;
  restore)
    f="${2:?缺文件名}"
    [ -f "$f" ] || { echo "O2_RESTORE_FAIL 记录文件不在: $f"; exit 2; }
    for p in "$CPU"/policy*; do [ -d "$p" ] && restore_policy "$p" "$f"; done
    restore_gpu "$f"
    sleep 1
    d=$(diff_keys "$f" 1)
    if [ -z "$d" ]; then
      echo O2_RESTORE_OK
    else
      echo "$d"
      echo "O2_RESTORE_FAIL $(echo "$d" | cut -d' ' -f1 | tr '\n' ' ')"
      exit 1
    fi
    ;;
  *) echo "用法: $0 save|check|restore <文件> | show"; exit 2 ;;
esac
