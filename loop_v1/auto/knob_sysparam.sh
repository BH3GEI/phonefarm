#!/bin/sh
# knob_sysparam.sh {apply <plan>|restore|status} — 通用系统参数旋钮。
#
# 与 knob_ddr_boost.sh 同接口 (contract/knob.md 的三态), 区别是「改哪些项」不写死在
# 脚本里, 而是由主机端下发一份 plan 文件。这样大模型每轮挑的一组参数都能用同一个
# 旋钮实现落到设备上, 不必为每组参数生成一个脚本。
#
# plan 文件格式 (制表符分隔, 每行一项):
#   sysfs<TAB>/sys/...<TAB>目标值
#   setting<TAB>system:peak_refresh_rate<TAB>目标值
#
# 安全边界 (与主机端白名单重复一遍, 故意冗余 —— 设备端是最后一道):
#   · 路径必须命中 ALLOW 前缀
#   · 路径命中任何 DENY 关键词一律拒绝。温控保护 (thermal / trip_point / cooling /
#     风扇 / bcl) 永远不准动, 哪怕主机端传了下来 —— 拒绝并如实报 KNOB_REFUSE。
#
# 回滚纪律: apply 前把每项原值写进 STATE; restore 逐项写回并回读比对,
# 全部对上才删 STATE。STATE 在 = 处于施加态, 也是崩溃后的恢复依据。
set -u

STATE=/data/local/tmp/loop_v1_sysparam.state

# 允许触碰的路径前缀 (sysfs)
allowed_sysfs() {
  case "$1" in
    /sys/devices/system/cpu/cpufreq/policy*/scaling_min_freq) return 0 ;;
    /sys/devices/system/cpu/cpufreq/policy*/scaling_max_freq) return 0 ;;
    /sys/devices/system/cpu/cpufreq/policy*/scaling_governor)  return 0 ;;
    /sys/class/kgsl/kgsl-3d0/min_pwrlevel)                     return 0 ;;
    /sys/class/kgsl/kgsl-3d0/max_pwrlevel)                     return 0 ;;
    /sys/class/kgsl/kgsl-3d0/devfreq/min_freq)                 return 0 ;;
    /sys/class/kgsl/kgsl-3d0/devfreq/max_freq)                 return 0 ;;
    /sys/class/kgsl/kgsl-3d0/devfreq/governor)                 return 0 ;;
    /sys/devices/system/cpu/bus_dcvs/DDR/boost_freq)           return 0 ;;
    /sys/devices/system/cpu/bus_dcvs/LLCC/boost_freq)          return 0 ;;
    *) return 1 ;;
  esac
}

# 温控保护相关一律拒绝
denied() {
  case "$1" in
    *thermal*|*trip_point*|*cooling*|*fan*|*tsens*|*bcl*|*throttl*) return 0 ;;
    *) return 1 ;;
  esac
}

allowed_setting() {
  case "$1" in
    system:peak_refresh_rate|system:min_refresh_rate) return 0 ;;
    *) return 1 ;;
  esac
}

read_one() {  # read_one <kind> <path>
  if [ "$1" = "setting" ]; then
    ns=$(echo "$2" | cut -d: -f1); key=$(echo "$2" | cut -d: -f2)
    settings get "$ns" "$key" 2>/dev/null
  else
    cat "$2" 2>/dev/null
  fi
}

write_one() {  # write_one <kind> <path> <value>
  if [ "$1" = "setting" ]; then
    ns=$(echo "$2" | cut -d: -f1); key=$(echo "$2" | cut -d: -f2)
    # 原值是 null / 空 = 这个键本来就没设过。回滚时必须 delete 而不是 put "null",
    # 否则会留下一个字面量 "null" —— 判据 4 的快照会当场抓到这种留痕。
    if [ "$3" = "null" ] || [ -z "$3" ]; then
      settings delete "$ns" "$key" >/dev/null 2>&1
    else
      settings put "$ns" "$key" "$3" 2>/dev/null
    fi
  else
    echo "$3" > "$2" 2>/dev/null
  fi
}

case "${1:-status}" in
  apply)
    PLAN="${2:-}"
    if [ -z "$PLAN" ] || [ ! -f "$PLAN" ]; then
      echo "KNOB_FAIL plan: 缺 plan 文件 ($PLAN)" >&2
      exit 2
    fi
    if [ -f "$STATE" ]; then
      echo "KNOB_ALREADY_APPLIED (state=$STATE)"
      exit 0
    fi

    # 先全量校验, 有任何一项越界就整份拒绝, 一个字节都不写
    bad=0
    while IFS="$(printf '\t')" read -r kind path val; do
      [ -n "${kind:-}" ] || continue
      case "$kind" in \#*) continue ;; esac
      if denied "$path"; then
        echo "KNOB_REFUSE $path: 命中温控保护关键词, 本旋钮永不触碰"
        bad=1; continue
      fi
      if [ "$kind" = "setting" ]; then
        allowed_setting "$path" || { echo "KNOB_REFUSE $path: 不在 setting 白名单"; bad=1; }
      elif [ "$kind" = "sysfs" ]; then
        allowed_sysfs "$path" || { echo "KNOB_REFUSE $path: 不在 sysfs 白名单"; bad=1; }
        [ -e "$path" ] || { echo "KNOB_REFUSE $path: 节点不存在"; bad=1; }
      else
        echo "KNOB_REFUSE $path: 未知 kind=$kind"; bad=1
      fi
    done < "$PLAN"
    if [ "$bad" = "1" ]; then
      echo "KNOB_PLAN_REJECTED (未写入任何一项)"
      exit 3
    fi

    : > "$STATE"
    # 两遍写: 第一遍可能被 min>max 之类的顺序约束夹回, 第二遍补齐。
    # 原值只在第一遍记录, 所以重复写不会污染回滚依据。
    pass=1
    while [ "$pass" -le 2 ]; do
      while IFS="$(printf '\t')" read -r kind path val; do
        [ -n "${kind:-}" ] || continue
        case "$kind" in \#*) continue ;; esac
        if [ "$pass" = "1" ]; then
          cur=$(read_one "$kind" "$path")
          printf '%s\t%s\t%s\n' "$kind" "$path" "$cur" >> "$STATE"
        fi
        write_one "$kind" "$path" "$val"
      done < "$PLAN"
      pass=$((pass + 1))
    done

    # 回读比对, 逐项如实自报
    while IFS="$(printf '\t')" read -r kind path val; do
      [ -n "${kind:-}" ] || continue
      case "$kind" in \#*) continue ;; esac
      back=$(read_one "$kind" "$path")
      old=$(grep -F "$path" "$STATE" 2>/dev/null | head -1 | cut -f3)
      if [ "$back" = "$val" ]; then
        echo "KNOB_APPLIED $path: $old -> $back"
      else
        echo "KNOB_FAIL $path: 想写 $val, 回读 $back (原值 $old)"
      fi
    done < "$PLAN"
    ;;

  restore)
    if [ ! -f "$STATE" ]; then
      echo "KNOB_NOT_APPLIED (无 $STATE, 无需回滚)"
      exit 0
    fi
    ok=1
    # 同样两遍, 处理 min/max 的顺序约束
    pass=1
    while [ "$pass" -le 2 ]; do
      while IFS="$(printf '\t')" read -r kind path val; do
        [ -n "${kind:-}" ] || continue
        write_one "$kind" "$path" "$val"
      done < "$STATE"
      pass=$((pass + 1))
    done
    while IFS="$(printf '\t')" read -r kind path val; do
      [ -n "${kind:-}" ] || continue
      back=$(read_one "$kind" "$path")
      if [ "$back" = "$val" ]; then
        echo "KNOB_RESTORED $path: -> $back"
      else
        echo "KNOB_RESTORE_FAIL $path: 想写 $val, 回读 $back"
        ok=0
      fi
    done < "$STATE"
    [ "$ok" = "1" ] && rm -f "$STATE"
    [ "$ok" = "1" ] || exit 4
    ;;

  status)
    if [ -f "$STATE" ]; then
      echo "state: 已施加"
      while IFS="$(printf '\t')" read -r kind path val; do
        [ -n "${kind:-}" ] || continue
        printf '%s cur=%s orig=%s\n' "$path" "$(read_one "$kind" "$path")" "$val"
      done < "$STATE"
    else
      echo "state: 未施加"
    fi
    ;;

  *)
    echo "用法: knob_sysparam.sh {apply <plan>|restore|status}" >&2
    exit 2
    ;;
esac
