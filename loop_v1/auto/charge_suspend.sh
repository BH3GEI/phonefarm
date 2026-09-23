#!/bin/sh
# charge_suspend.sh {suspend|restore|status} — 测功耗期间停充, 让整机真由电池供电。
#
# 为什么非停充不可 (实测):
#   插着 USB 时 USB 输入功率里约 46% 是在给电池充电, 而且充电电流随电量**单调衰减**
#   —— 那个衰减会被当成"功耗随时间下降"混进 A/B 比较。电池轨这边同样量不到整机开销,
#   读到的只是充电电流与系统耗电相抵之后的余量。
#
# 节点与判据全部照搬 phonefarm src/hwcond.rs (PR #27 真机验过的那一版), 不另造一套:
#   · 候选节点表与探测顺序一致 —— 本机红魔 NX809J 管用的是 /sys/class/qcom-battery/
#     charging_enabled, 它不在 power_supply class 下, 只看 power_supply 会整个错过
#   · 等 12 秒让电量计跟上 —— 第一版只等 2.5 秒, 把管用的节点误判成"写进去无效"
#   · 电量低于 30% 不停充: 停充期间整机纯靠电池, 电量本来就低时再满载抽几分钟,
#     轻则测到一半关机, 重则过放
#   · on_battery 要两个判据同时成立 —— status 不是 Charging **且** current_now < 0。
#     两个都会单独骗人: 停充后有的内核仍写 "Not charging" 而不是 "Discharging";
#     current_now 在充放平衡的瞬间会过零
#
# 回滚纪律同 knob_sysparam.sh: 先把「改了哪个节点、原值是什么」写进 STATE 再动设备,
# 所以进程中途被杀也能由下一次 restore 回滚。
set -u

STATE=/data/local/tmp/loop_v1_charge.state
B=/sys/class/power_supply/battery
MIN_CAPACITY=30
SETTLE_TRIES=12      # 12 x 1s = 12s, 与 hwcond.rs 的 SUSPEND_SETTLE_MS 一致
                     # (不用 sleep 0.5: Android 的 sh 不保证支持小数秒)

# "<路径> <how>" —— how: one=写1 / zero=写0 / cap=把上限压到当前电量以下
CANDIDATES="/sys/class/power_supply/battery/input_suspend one
/sys/class/power_supply/usb/input_suspend one
/sys/class/qcom-battery/charging_enabled zero
/sys/class/qcom-battery/battery_charging_enabled zero
/sys/class/power_supply/battery/charging_enabled zero
/sys/class/power_supply/battery/battery_charging_enabled zero
/sys/class/power_supply/battery/charge_control_end_threshold cap
/sys/class/power_supply/usb/input_current_limit zero"

bstat() { cat "$B/status" 2>/dev/null; }
bcur()  { cat "$B/current_now" 2>/dev/null; }
bcap()  { cat "$B/capacity" 2>/dev/null; }

on_battery() {
  st=$(bstat); cur=$(bcur)
  case "$st" in Charging|charging|CHARGING) return 1 ;; esac
  case "$cur" in ''|*[!0-9-]*) return 1 ;; esac
  [ "$cur" -lt 0 ] 2>/dev/null
}

report_state() {
  printf 'CHARGE_STATE status=%s current_now=%s capacity=%s on_battery=%s\n' \
    "$(bstat)" "$(bcur)" "$(bcap)" "$(on_battery && echo yes || echo no)"
}

case "${1:-status}" in
  suspend)
    if [ -f "$STATE" ]; then
      echo "CHARGE_ALREADY_SUSPENDED (state=$STATE)"
      report_state
      exit 0
    fi
    cap=$(bcap)
    case "$cap" in
      ''|*[!0-9]*) echo "CHARGE_SKIP: 读不到电池电量, 不敢停充"; report_state; exit 1 ;;
    esac
    if [ "$cap" -lt "$MIN_CAPACITY" ] 2>/dev/null; then
      echo "CHARGE_SKIP: 电量 $cap% 低于停充下限 $MIN_CAPACITY%, 不停充"
      report_state
      exit 1
    fi
    if on_battery; then
      echo "CHARGE_ALREADY_ON_BATTERY (本来就在放电态, 不需要停充)"
      report_state
      exit 0
    fi

    # 用 here-doc 而不是管道喂这个循环: 管道会把 while 放进子 shell, 里面的
    # exit 0 只退子 shell, 成功路径就得靠外面再判一次 —— 少一层拐弯少一个坑。
    while IFS=' ' read -r path how; do
      [ -n "${path:-}" ] || continue
      saved=$(cat "$path" 2>/dev/null)
      [ -n "$saved" ] || continue          # 这台机器没有这个节点
      case "$how" in
        one)  want=1 ;;
        zero) want=0 ;;
        cap)
          # 压到电量以下但不低于 1: 压到电量**以上**等于没压 —— 本机出厂就是
          # end_threshold=80 而电量 91% 照充不误, 正是这个道理
          want=$((cap - 5)); [ "$want" -lt 1 ] && want=1; [ "$want" -gt 99 ] && want=99 ;;
        *) continue ;;
      esac

      # 先落状态文件再动设备
      printf '%s\n%s\n' "$path" "$saved" > "$STATE"
      echo "$want" > "$path" 2>/dev/null

      i=0
      ok=0
      while [ "$i" -lt "$SETTLE_TRIES" ]; do
        sleep 1
        if on_battery; then ok=1; break; fi
        i=$((i + 1))
      done
      if [ "$ok" = "1" ]; then
        echo "CHARGE_SUSPENDED $path: $saved -> $want"
        report_state
        exit 0
      fi
      # 没生效: 就地还原, 试下一个
      echo "$saved" > "$path" 2>/dev/null
      rm -f "$STATE"
      echo "CHARGE_TRY_FAILED $path (写 $want 无效)"
    done <<CANDS
$CANDIDATES
CANDS

    echo "CHARGE_FAIL: 所有候选节点都没能把设备转成放电态"
    report_state
    exit 1
    ;;

  restore)
    if [ ! -f "$STATE" ]; then
      echo "CHARGE_NOT_SUSPENDED (无 $STATE, 无需还原)"
      report_state
      exit 0
    fi
    path=$(sed -n 1p "$STATE")
    saved=$(sed -n 2p "$STATE")
    if [ -z "$path" ] || [ -z "$saved" ]; then
      echo "CHARGE_RESTORE_FAIL: 状态文件不完整 ($STATE)"
      exit 1
    fi
    echo "$saved" > "$path" 2>/dev/null
    back=$(cat "$path" 2>/dev/null)
    if [ "$back" = "$saved" ]; then
      rm -f "$STATE"
      echo "CHARGE_RESTORED $path: -> $back"
      report_state
    else
      echo "CHARGE_RESTORE_FAIL $path: 想写 $saved, 回读 $back"
      report_state
      exit 1
    fi
    ;;

  status)
    if [ -f "$STATE" ]; then
      echo "state: 已停充 ($(sed -n 1p "$STATE") 原值 $(sed -n 2p "$STATE"))"
    else
      echo "state: 未停充"
    fi
    report_state
    ;;

  *)
    echo "用法: charge_suspend.sh {suspend|restore|status}" >&2
    exit 2
    ;;
esac
