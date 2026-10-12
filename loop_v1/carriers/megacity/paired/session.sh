#!/bin/bash
# session.sh <outdir> <arm>...  — Megacity 成对采集: 同一台手机先后跑「低清+放大」和「原生高清」
#
# 在手机节点上、phonefarm 根目录下、持设备锁时跑:
#   pf-node -o <本地目录> devlock run pair -- bash loop_v1/carriers/megacity/paired/session.sh @out/s1 \
#       audit:0.5 cap:0.5 cap:1.0 perf:0.5 perf:1.0
#
# 臂 (按给的顺序跑):
#   audit:<resscale>  frame-audit 拆一帧 (只读层), 看有没有「低分辨率渲染 → 放大到送显」那一步
#   cap:<resscale>    抓帧遍: 挂层 framecap —— 按帧序号抓无损帧 (每 60 帧一张 + 每 600 帧前后各 2 帧)、
#                     每帧左上角盖帧序号条码, 同时 adb screenrecord 录屏 (只给视频用)。
#                     这一遍有抓帧卡顿和录屏开销, **不进性能统计**。
#   perf:<resscale>   性能遍: 不挂层、不录屏, 只读: SurfaceFlinger 每帧上屏时间 (帧率) + 电池电流电压 (功耗)
#   cap:<resscale>:<后缀> 同一档再跑一遍 (例 cap:1.0:b, 量两遍之间的「本底差」)
#   upop:<resscale>   同 cap, 但层同时开 upop (用我们的放大算子替换游戏自带的放大); 前提是 audit 显示适用
#
# 环境: PAIR_SO (层 .so, 缺省 knobs/gray/build/out/libVkLayer_refknobs.so)、PAIR_FRAMES (缺省 3600)、
#       PAIR_BUDGET_S (整段上限秒数, 缺省 2400 = 40 分钟; 超了不再开新臂)
#
# 收尾 (trap, 被杀也走): 停录屏、杀应用、清 framecap 属性、摘层、debug.vulkan.layers 置空、
# 充电节点若为 0 写回 1、媒体音量若不为 0 置 0。
set -uo pipefail

# 可选前缀参数 (pf-node 不传环境变量, 所以也能用参数给): --so=<层.so> --tools=<本目录> --frames=N --budget=秒
while [ $# -gt 0 ]; do
  case "$1" in
    --so=*) PAIR_SO="${1#--so=}"; shift ;;
    --tools=*) PAIR_TOOLS="${1#--tools=}"; shift ;;
    --frames=*) PAIR_FRAMES="${1#--frames=}"; shift ;;
    --budget=*) PAIR_BUDGET_S="${1#--budget=}"; shift ;;
    *) break ;;
  esac
done
OUT="${1:?用法: session.sh [--so=..] [--tools=..] <outdir> <arm>...}"; shift
mkdir -p "$OUT"
SERIAL="${PAIR_SERIAL:-91253241019A}"
# 节点服务由 launchd 拉起, PATH 里未必有 homebrew
ADB="$(command -v adb 2>/dev/null || echo /opt/homebrew/bin/adb)"
export PATH="$PATH:/opt/homebrew/bin:/usr/bin"
PKG=com.unity.megacity.metro
ACT=com.unity.megacity.MegacityMetro
FRAMES="${PAIR_FRAMES:-3600}"
SO="${PAIR_SO:-knobs/gray/build/out/libVkLayer_refknobs.so}"
BUDGET="${PAIR_BUDGET_S:-2400}"
APPF=/storage/emulated/0/Android/data/$PKG/files
ROOT="$PWD"
PF="$ROOT/phonefarm"
HERE="${PAIR_TOOLS:-$(cd "$(dirname "$0")" && pwd)}"   # framecap_tools.py 所在目录
T0=$(date +%s)
LOG="$OUT/session.log"

say() { echo "[$(date '+%H:%M:%S')] $*" | tee -a "$LOG"; }
a() { "$ADB" -s "$SERIAL" shell "$@" </dev/null; }
asu() { "$ADB" -s "$SERIAL" shell "su -c '$*'" </dev/null; }

FC_PROPS="debug.knobs.framecap debug.knobs.framecap_stamp debug.knobs.framecap_every debug.knobs.framecap_burst debug.knobs.framecap_marker debug.knobs.framecap_max debug.knobs.upop debug.knobs.passdump"
SRPID=""

volume_zero() {
  local v
  v=$(a "cmd media_session volume --stream 3 --get" 2>/dev/null | tr -d '\r' | grep -o 'volume is [0-9]*' | grep -o '[0-9]*$')
  if [ "${v:-x}" != "0" ]; then
    a "cmd audio set-volume 3 0" >/dev/null 2>&1 || a "media volume --stream 3 --set 0" >/dev/null 2>&1
    v=$(a "cmd media_session volume --stream 3 --get" 2>/dev/null | tr -d '\r' | grep -o 'volume is [0-9]*' | grep -o '[0-9]*$')
  fi
  echo "${v:-unknown}"
}

restore() {
  say "收尾还原"
  [ -n "$SRPID" ] && { a "pkill -INT screenrecord" >/dev/null 2>&1; wait "$SRPID" 2>/dev/null; }
  a "am force-stop $PKG" >/dev/null 2>&1
  for p in $FC_PROPS; do asu "setprop $p 0" >/dev/null 2>&1; done
  asu "setprop debug.knobs.framecap_marker \"\"" >/dev/null 2>&1
  "$PF" gray-layer off >>"$LOG" 2>&1
  asu 'setprop debug.vulkan.layers ""' >/dev/null 2>&1
  local ce
  ce=$(asu "cat /sys/class/qcom-battery/charging_enabled" 2>/dev/null | tr -d '\r')
  [ "$ce" = "0" ] && asu "echo 1 > /sys/class/qcom-battery/charging_enabled"
  local vol layers
  vol=$(volume_zero)
  layers=$(a "getprop debug.vulkan.layers" | tr -d '\r')
  ce=$(asu "cat /sys/class/qcom-battery/charging_enabled" 2>/dev/null | tr -d '\r')
  say "还原回读: debug.vulkan.layers=[$layers] charging_enabled=$ce 媒体音量=$vol"
  printf '{"debug_vulkan_layers":"%s","charging_enabled":"%s","media_volume":"%s"}\n' "$layers" "$ce" "$vol" > "$OUT/restore.json"
}
trap restore EXIT
trap 'exit 130' INT TERM

gpu_temp() { asu "cat /sys/class/kgsl/kgsl-3d0/temp" 2>/dev/null | tr -d '\r'; }

cool_down() {   # 有界: 最多 120s 等到 45°C, 等不到照跑并记下起跑温度
  local t
  for _ in $(seq 1 60); do
    t=$(gpu_temp); [ -n "$t" ] && [ "$t" -le 45000 ] 2>/dev/null && break
    sleep 2
  done
  echo "${t:-unknown}"
}

clear_app() {
  a "am force-stop $PKG" >/dev/null 2>&1
  asu "rm -rf $APPF/framecap $APPF/megacity_out.json $APPF/megacity_started $APPF/knobs_layer_out.json" >/dev/null 2>&1
  a "pm grant $PKG android.permission.RECORD_AUDIO" >/dev/null 2>&1 || true
}

launch() {   # launch <run_id> <resscale>
  a "am start -n $PKG/$ACT --es scene city_flythrough --es run_id $1 --es frames $FRAMES --es route route_a --es warmup 600 --es settle 7200 --es resscale $2 --es vsync off" >/dev/null 2>&1
}

wait_file() {   # wait_file <设备路径> <上限秒>; 应用中途退出就失败
  local n=$(( $2 * 5 ))
  for _ in $(seq 1 $n); do
    [ -n "$(asu "cat $1 2>/dev/null | head -c 64")" ] && return 0
    [ -z "$(a "pidof $PKG" | tr -d '\r')" ] && return 1
    sleep 0.2
  done
  return 1
}

wait_exit() {   # wait_exit <上限秒>
  for _ in $(seq 1 "$1"); do
    [ -z "$(a "pidof $PKG" | tr -d '\r')" ] && return 0
    sleep 1
  done
  return 1
}

pull_common() {   # pull_common <dir>
  asu "cat $APPF/megacity_out.json" > "$1/megacity_out.json" 2>/dev/null
  asu "cat $APPF/knobs_layer_out.json" > "$1/knobs_layer_out.json" 2>/dev/null
}

arm_cap() {   # arm_cap <resscale> <upop 0|1> [后缀]
  local R=$1 UP=$2 lab d t
  lab="cap_r${R}"; [ "$UP" = 1 ] && lab="upop_r${R}"
  [ -n "${3:-}" ] && lab="${lab}_$3"
  d="$OUT/$lab"; mkdir -p "$d"
  say "== $lab: 抓帧遍 resscale=$R upop=$UP"
  clear_app
  t=$(cool_down); say "起跑 gpu_temp=${t}mC"
  asu "setprop debug.knobs.framecap 1; setprop debug.knobs.framecap_stamp 1; setprop debug.knobs.framecap_every 60; setprop debug.knobs.framecap_burst 600:2; setprop debug.knobs.framecap_marker megacity_started; setprop debug.knobs.framecap_max $FRAMES; setprop debug.knobs.passdump $UP; setprop debug.knobs.upop $UP"
  # target 档会把 loadop/passdump/copyprobe/upop/frameaudit/storeop 全写成 0, 再在挂上之后补回 upop
  if ! "$PF" gray-layer target "$PKG" --so "$SO" --keep-prop >"$d/mount.log" 2>&1; then
    say "挂层失败, 见 $d/mount.log"; return 1
  fi
  asu "setprop debug.knobs.passdump $UP; setprop debug.knobs.upop $UP"
  launch "$lab" "$R"
  if ! wait_file "$APPF/knobs_layer_out.json" 60; then say "层没加载 (60s 内无 marker)"; asu 'setprop debug.vulkan.layers ""'; return 1; fi
  asu 'setprop debug.vulkan.layers ""'   # 层已在进程里, 立刻收窄全局属性的误伤窗口
  if ! wait_file "$APPF/megacity_started" 300; then say "应用没进被测窗口"; return 1; fi
  a "rm -f /sdcard/pair_$lab.mp4" >/dev/null 2>&1
  "$ADB" -s "$SERIAL" shell screenrecord --bit-rate 8000000 --time-limit 180 "/sdcard/pair_$lab.mp4" </dev/null >"$d/screenrecord.log" 2>&1 &
  SRPID=$!
  local ts0; ts0=$(date +%s)
  say "进被测窗口, 录屏开始"
  wait_exit 400 || say "应用 400s 内没自退"
  say "应用退出, 被测窗口约 $(( $(date +%s) - ts0 ))s"
  a "pkill -INT screenrecord" >/dev/null 2>&1; wait "$SRPID" 2>/dev/null; SRPID=""
  sleep 1
  pull_common "$d"
  "$ADB" -s "$SERIAL" pull "$APPF/framecap" "$d/" >"$d/pull.log" 2>&1 || asu "ls -la $APPF/framecap" >> "$d/pull.log"
  "$ADB" -s "$SERIAL" pull "/sdcard/pair_$lab.mp4" "$d/screen.mp4" >>"$d/pull.log" 2>&1
  a "rm -f /sdcard/pair_$lab.mp4" >/dev/null 2>&1
  asu "rm -rf $APPF/framecap" >/dev/null 2>&1
  for p in $FC_PROPS; do asu "setprop $p 0" >/dev/null 2>&1; done
  "$PF" gray-layer off >>"$LOG" 2>&1
  # raw→png 不在持锁期间做 (纯 CPU, 白占手机): 会话结束后跑 framecap_tools.py convert
  say "$lab 完成: $(ls "$d/framecap" 2>/dev/null | grep -c raw) 张 raw, 录屏 $(du -h "$d/screen.mp4" 2>/dev/null | cut -f1)"
}

arm_perf() {   # 只读: 不挂层、不录屏
  local R=$1 lab="perf_r$1" d t layer
  d="$OUT/$lab"; mkdir -p "$d"
  say "== $lab: 性能遍 (只读) resscale=$R"
  clear_app
  t=$(cool_down); say "起跑 gpu_temp=${t}mC"
  launch "$lab" "$R"
  if ! wait_file "$APPF/megacity_started" 300; then say "应用没进被测窗口"; return 1; fi
  layer=$(a "dumpsys SurfaceFlinger --list" | tr -d '\r' | grep -m1 "^SurfaceView\[$PKG" )
  say "SF 图层: ${layer:-找不到}"
  echo "$layer" > "$d/sf_layer.txt"
  local i=0
  : > "$d/samples.txt"
  while [ -n "$(a "pidof $PKG" | tr -d '\r')" ] && [ $i -lt 400 ]; do
    {
      echo "---T $(date +%s.%N 2>/dev/null || date +%s)"
      [ -n "$layer" ] && a "dumpsys SurfaceFlinger --latency '$layer'" | tr -d '\r'
      echo "---BAT"
      asu "cat /sys/class/power_supply/battery/status /sys/class/power_supply/battery/current_now /sys/class/power_supply/battery/voltage_now /sys/class/kgsl/kgsl-3d0/temp" | tr -d '\r'
    } >> "$d/samples.txt"
    i=$((i + 1))
    sleep 1
  done
  pull_common "$d"
  python3 "$HERE/framecap_tools.py" perf "$d" >>"$LOG" 2>&1 || say "性能汇总失败"
  say "$lab 完成: $(cat "$d/perf.json" 2>/dev/null | head -c 300)"
}

arm_audit() {
  local R=$1 d="$OUT/audit_r$1"
  mkdir -p "$d"
  say "== audit_r$R: frame-audit (city_static, resscale=$R)"
  "$PF" frame-audit capture --out "$d" --so "$SO" --pkg "$PKG" --captures 2 --gap 3 --settle 45 \
    --extras "--es scene city_static --es frames 100000 --es resscale $R --es warmup 300" >>"$LOG" 2>&1 || say "frame-audit 抓帧失败"
  a "am force-stop $PKG" >/dev/null 2>&1
  local js; js=$(ls "$d"/fa_*.json 2>/dev/null | head -1)
  [ -n "$js" ] && "$PF" frame-audit analyze "$d"/fa_*.json --md "$d/analysis.md" --json "$d/analysis.json" >>"$LOG" 2>&1
  say "audit_r$R 完成"
}

say "会话开始 out=$OUT arms=$* so=$SO frames=$FRAMES 音量=$(volume_zero)"
asu "cat /sys/class/power_supply/battery/status /sys/class/power_supply/battery/capacity /sys/class/qcom-battery/charging_enabled" > "$OUT/battery_at_start.txt" 2>&1
for arm in "$@"; do
  if [ $(( $(date +%s) - T0 )) -gt "$BUDGET" ]; then say "超出预算 ${BUDGET}s, 不再开新臂 ($arm 起都跳过)"; break; fi
  kind=${arm%%:*}; R=${arm#*:}; SUF=""
  case "$R" in *:*) SUF=${R#*:}; R=${R%%:*} ;; esac
  case "$kind" in
    cap)   arm_cap "$R" 0 "$SUF" ;;
    upop)  arm_cap "$R" 1 "$SUF" ;;
    perf)  arm_perf "$R" ;;
    audit) arm_audit "$R" ;;
    *) say "不认识的臂 $arm" ;;
  esac
done
say "会话结束, 用时 $(( $(date +%s) - T0 ))s"
