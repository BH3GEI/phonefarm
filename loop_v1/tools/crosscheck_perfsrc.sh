#!/bin/bash
# crosscheck_perfsrc.sh <标签> <输出目录> [refbench|genshin] — 同一段负载窗口内, 两条采集通路并排采一次
#
# 为什么要并排采而不是先后采: 帧率/功耗/温度全都随热状态漂移。先跑 A 再跑 B,
# 两组数字的差里混着「这两分钟机器变热了」, 分不清是通路口径差异还是设备状态差异。
# 所以两条通路**在同一个窗口里同时跑** —— 它们都是只读采集 (一个读 tracefs,
# 一个读 sysfs + 设备端 socket), 互不写对方的东西。
#
#   通路 A (我们的): raw ftrace 的 kgsl 事件 → 逐帧间隔 / GPU 执行时长
#                    + /sys/class/power_supply 电源轨 → 功耗
#                    + /sys/class/thermal → 温度
#   通路 B (HiSmartPerf): 设备端 GamePerfToolCollector + adb forward socket 实时流
#                    → 每秒一条 {fps,gpuUsage,current,voltage,soc,gpuTemp,batTemp,…}
#
# 负载二选一:
#   refbench (默认) — 渲染靶场, 场景确定、每帧一次提交, 帧率口径没有歧义。
#                     优先用它: 原神每帧两次提交, 帧率要先自检, 多一个出错的地方。
#   genshin         — 原神蒙德城定点匀速转视角 (loop_v1 的固定场景)。
set -euo pipefail

LABEL="${1:?用法: crosscheck_perfsrc.sh <标签> <输出目录> [refbench|genshin]}"
OUTDIR="${2:?缺输出目录}"
WORKLOAD="${3:-refbench}"

SERIAL=91253241019A
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LEAD=6          # 负载起跑后等几秒再开采 (让负载进入稳态)
CAPDUR=30       # 采集窗口, 两条通路共用这一段
export PATH="$PATH:/Users/mac/Library/Android/sdk/platform-tools"

case "$WORKLOAD" in
  refbench)
    PKG=io.github.hgamey.refbench
    COMM=RefbenchDrv
    ;;
  genshin)
    PKG=com.miHoYo.Yuanshen
    COMM=UnityGfxDeviceW
    ;;
  *) echo "负载只能是 refbench 或 genshin, 给的是 $WORKLOAD" >&2; exit 2 ;;
esac

mkdir -p "$OUTDIR"
echo "── [$LABEL] 负载 $WORKLOAD ($PKG) · 两通路并排采集 ${CAPDUR}s ──"

adb -s "$SERIAL" shell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUTDIR/snap_at_run.txt" 2>&1 || true

# 主动散热风扇的状态。红魔的风扇没有 sysfs 转速节点 (vendor HAL 里, /sys 下找不到 fan/rpm),
# 能读到的只有这几个 settings 键 —— 但它必须记: 风扇开不开直接决定热状态,
# 而热状态决定帧率与功耗。两条通路是并排采的, 风扇对两边的影响一致;
# 可**跨轮**比较时若一轮开一轮关, 差值里混的就全是风扇。
{
  for k in fan_state_of_manual fan_state_of_mode fan_state_of_charging \
           liquid_cooling_off_on liquid_cooling_main_switch; do
    printf '%s=%s\n' "$k" "$(adb -s "$SERIAL" shell settings get system "$k" 2>/dev/null | tr -d '\r')"
  done
  printf 'battery_status=%s\n' \
    "$(adb -s "$SERIAL" shell 'cat /sys/class/power_supply/battery/status' 2>/dev/null | tr -d '\r')"
} > "$OUTDIR/test_conditions.txt" 2>&1 || true

# 1) 负载后台起跑
if [ "$WORKLOAD" = refbench ]; then
  # 帧数要够铺满 LEAD + CAPDUR, 而且要按**最快**的帧率算, 不是按屏幕刷新率算:
  # sr_pipeline 在这台机器上实测跑到 ~470 fps (它不等 vsync), 按 120 fps 估的
  # 4320 帧只够跑 9 秒 —— 采集窗口还没过半应用就退了, HiSmartPerf 一条样本都收不到,
  # 而报出来的症状是「设备端采集器没给出任何实时样本」, 看着像通路坏了。
  FRAMES=$(( (LEAD + CAPDUR) * 1000 ))
  adb -s "$SERIAL" shell "am force-stop $PKG" >/dev/null 2>&1 || true
  ( adb -s "$SERIAL" shell "am start -W -n $PKG/android.app.NativeActivity \
      --es scene sr_pipeline --es run_id xcheck_$LABEL --es frames $FRAMES \
      --es intensity 1.0 --es knob.postfx off" ) > "$OUTDIR/workload.log" 2>&1
  WL_PID=""
else
  WL="$ROOT/loop_v1/scripts/workload_spin_v1.json"
  ( cd "$ROOT" && ./phonefarm script --task "xcheck_$LABEL" --serial "$SERIAL" \
      --app "$PKG" --no-screen "$WL" ) > "$OUTDIR/workload.log" 2>&1 &
  WL_PID=$!
fi

sleep "$LEAD"

# 2) 同一窗口里并排起三路采集
#    A1: ftrace (逐帧口径的事实来源)
adb -s "$SERIAL" shell "su -c 'sh /data/local/tmp/ftrace_capture.sh $CAPDUR /data/local/tmp/xcheck_$LABEL.txt'" \
    > "$OUTDIR/capture.log" 2>&1 &
FT_PID=$!

#    A2: 我们的 sysfs 电源轨 (每 200ms 一条, 跑满窗口)
( cd "$ROOT" && ./phonefarm perf --serial "$SERIAL" --source sysfs \
    --power-rail battery --rounds $((CAPDUR * 5)) --json ) \
    > "$OUTDIR/perf_sysfs.json" 2>"$OUTDIR/perf_sysfs.err" &
SYS_PID=$!

#    B: HiSmartPerf 通路 (设备端每秒一条)。同时把原始线格式落盘 ——
#       两边差几个百分点时, 逐秒原始序列是唯一能把「口径差异」与「窗口边缘效应」分开的证据。
( cd "$ROOT" && PHONEFARM_GPD_RAW="$OUTDIR/gp_realtime.txt" \
  ./phonefarm perf --serial "$SERIAL" --source smartperf \
    --app "$PKG" --rounds "$CAPDUR" --json ) \
    > "$OUTDIR/perf_smartperf.json" 2>"$OUTDIR/perf_smartperf.err" &
SP_PID=$!

#    参考: 窗口中点直读一次热区, 用来对 HiSmartPerf 报的温度
sleep $((CAPDUR / 2))
adb -s "$SERIAL" shell "su -c 'for z in /sys/class/thermal/thermal_zone*; do \
  printf \"%s %s\n\" \"\$(cat \$z/type)\" \"\$(cat \$z/temp)\"; done'" \
    > "$OUTDIR/thermal_mid.txt" 2>&1 || true

# 采集窗口末尾确认负载还活着。跑完就退的载体 (refbench 给定帧数) 会在窗口中途消失,
# 而两条通路对此的报错都是「没采到样本」—— 与通路本身坏掉长得一模一样。记一笔, 省得误判。
printf 'workload_alive_at_window_end=%s\n' \
  "$(adb -s "$SERIAL" shell "pidof $PKG" 2>/dev/null | tr -d '\r' | grep -q '[0-9]' && echo yes || echo NO)" \
  >> "$OUTDIR/test_conditions.txt"

wait "$FT_PID"  || echo "[$LABEL] 警告: ftrace 采集非零退出"
wait "$SYS_PID" || echo "[$LABEL] 提示: sysfs 通路非零退出 (可能是功耗不可信, 看 JSON 的 unavailable)"
wait "$SP_PID"  || echo "[$LABEL] 提示: HiSmartPerf 通路非零退出 (看 JSON 的 unavailable)"
[ -n "$WL_PID" ] && { wait "$WL_PID" || echo "[$LABEL] 警告: 负载脚本非零退出"; }

# 3) 拉回 ftrace 原始证据并解析
adb -s "$SERIAL" pull "/data/local/tmp/xcheck_$LABEL.txt" "$OUTDIR/trace.txt" >/dev/null 2>&1
adb -s "$SERIAL" shell "rm -f /data/local/tmp/xcheck_$LABEL.txt" >/dev/null 2>&1
python3 "$ROOT/loop_v1/tools/parse_trace.py" "$OUTDIR/trace.txt" --comm "$COMM" > "$OUTDIR/summary.json"

[ "$WORKLOAD" = refbench ] && adb -s "$SERIAL" shell "am force-stop $PKG" >/dev/null 2>&1 || true

# 4) 并排成一张表
python3 "$ROOT/loop_v1/tools/crosscheck_report.py" "$OUTDIR" | tee "$OUTDIR/crosscheck.txt"
