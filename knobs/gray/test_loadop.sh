#!/bin/bash
# test_loadop.sh — 灰档 LoadOp 改写的三臂对照 (判据 5 的证据采集)
#
# refbench 是白档靶子, 自带 knob.loadop, 所以这条改写有**已知答案**可以对:
#
#   A  无层            + knob.loadop=off  → refbench 用 loadOp=LOAD 的 render pass (基线)
#   B  无层            + knob.loadop=on   → refbench 自己换成 DONT_CARE  (白档答案)
#   C  挂层+改写       + knob.loadop=off  → 层把 LOAD 改写成 DONT_CARE   (灰档, 要验的)
#   D  挂层+改写       + knob.loadop=on   → 已经没有 LOAD 可改, effective 应为空 (自洽检查)
#
# 本脚本**不做任何测量与判定** —— 那是 loop_v1 的事 (见 DESIGN.md 边界)。
# 它只采三样东西: 层自报改了什么、refbench 自报它请求的是什么、进程有没有干净跑完。
#
# C 成立的判据:
#   1. 层自报 effective 非空, field=loadOp, from=LOAD, to=DONT_CARE
#   2. refbench 仍然 frames_submitted=<请求帧数> 且 clean_exit=true (改写没把应用弄坏)
#   3. refbench 自报的 passes[].load_op 仍是 LOAD —— 它并不知道自己被改了。
#      "应用以为是 LOAD, 驱动实际收到 DONT_CARE" 就是灰档改写生效的定义。
#   4. D 臂 effective 为空 + unavailable_reason 非 null (层只在真有 LOAD 时才报改写)
set -uo pipefail

SERIAL="${REFBENCH_SERIAL:-91253241019A}"
PKG=io.github.hgamey.refbench
FRAMES="${FRAMES:-1800}"
INTENSITY="${INTENSITY:-8}"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUTDIR="${1:-$HERE/../evidence/05_loadop}"
mkdir -p "$OUTDIR"
ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }
FILES=/storage/emulated/0/Android/data/$PKG/files

arm() { # $1=臂名  $2=层(none|rewrite)  $3=knob.loadop(off|on)
  local name="$1" layer="$2" knob="$3"
  echo "=== 臂 $name: 层=$layer  knob.loadop=$knob ==="
  bash "$HERE/enable_layer.sh" off >/dev/null 2>&1
  ashell "am force-stop $PKG"
  ashell "su -c 'rm -f $FILES/refbench_out.json $FILES/knobs_layer_out.json'"

  if [ "$layer" = "rewrite" ]; then
    # 只挂 .so + 开属性, 不让 enable_layer.sh 自己启动 (我们要带 intent 参数启动)
    bash "$HERE/enable_layer.sh" loadop "$PKG" --keep-prop >/dev/null 2>&1
    ashell "am force-stop $PKG"
  fi

  ashell "am start -n $PKG/android.app.NativeActivity \
          --es scene bw_pingpong --es run_id $name --ei frames $FRAMES \
          --ei intensity $INTENSITY --es knob.loadop $knob" >/dev/null 2>&1

  # 全局属性一旦置上, 期间启动的、lib 目录里没有本 .so 的 Vulkan 应用会起不来
  # (实测 cn.nubia.gameassist 会崩溃重启)。所以**一检测到层已加载就立刻清掉**,
  # 把暴露窗口压到几秒, 而不是等这一臂整个跑完。层已在 refbench 进程里, 清掉不影响它。
  local i p
  if [ "$layer" = "rewrite" ]; then
    for i in $(seq 1 30); do
      [ -n "$(ashell "su -c 'cat $FILES/knobs_layer_out.json 2>/dev/null'" | tr -d '\r')" ] && break
      sleep 1
    done
    ashell "su -c 'setprop debug.vulkan.layers \"\"'"
    echo "  (全局属性已清, 暴露窗口约 ${i} 秒)"
  fi
  for i in $(seq 1 60); do
    p=$(ashell "pidof $PKG" | tr -d '\r')
    [ -z "$p" ] && [ "$i" -gt 2 ] && break
    sleep 2
  done

  ashell "su -c 'cat $FILES/refbench_out.json 2>/dev/null'" > "$OUTDIR/$name.refbench.json"
  ashell "su -c 'cat $FILES/knobs_layer_out.json 2>/dev/null'" > "$OUTDIR/$name.layer.json"
  echo "  refbench: $(grep -oE '\"frames_submitted\": *[0-9]+|\"clean_exit\": *[a-z]+' "$OUTDIR/$name.refbench.json" | tr '\n' ' ')"
  echo "  refbench 自报 load_op: $(grep -oE '\"load_op\": *\"[A-Z_]+\"' "$OUTDIR/$name.refbench.json" | sort -u | tr '\n' ' ')"
  echo "  层自报: $(cat "$OUTDIR/$name.layer.json" 2>/dev/null | head -c 400)"
  echo
}

arm A_noloayer_off none    off
arm B_nolayer_on   none    on
arm C_rewrite_off  rewrite off
arm D_rewrite_on   rewrite on

bash "$HERE/enable_layer.sh" off >/dev/null 2>&1
echo "已还原。证据: $OUTDIR"
