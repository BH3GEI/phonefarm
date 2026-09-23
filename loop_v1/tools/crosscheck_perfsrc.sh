#!/bin/bash
# crosscheck_perfsrc.sh <标签> <输出目录> — 同一段负载窗口内, 两条采集通路并排采一次
#
# 为什么要并排采而不是先后采: 帧率/功耗/温度全都随热状态漂移。先跑 A 再跑 B,
# 两组数字的差里混着「这两分钟机器变热了」, 分不清是通路口径差异还是设备状态差异。
# 所以两条通路**在同一个 30 秒窗口里同时跑** —— 它们都是只读采集 (一个读 tracefs,
# 一个读 sysfs + 设备端 socket), 互不写对方的东西。
#
#   通路 A (我们的): raw ftrace 的 kgsl 事件 → 逐帧间隔 / GPU 执行时长
#                    + /sys/class/power_supply 电源轨 → 功耗
#                    + /sys/class/thermal → 温度
#   通路 B (HiSmartPerf): 设备端 GamePerfToolCollector + adb forward socket 实时流
#                    → 每秒一条 {fps,gpuUsage,current,voltage,soc,gpuTemp,batTemp,…}
#
# 时序与 run_once.sh 对齐 (LEAD 秒后开采, 避开启动与收尾)。
set -euo pipefail

LABEL="${1:?用法: crosscheck_perfsrc.sh <标签> <输出目录>}"
OUTDIR="${2:?缺输出目录}"

SERIAL=91253241019A
PKG=com.miHoYo.Yuanshen
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WL="$ROOT/loop_v1/scripts/workload_spin_v1.json"
LEAD=6          # 负载起跑后等几秒再开采 (让转镜头进入稳态)
CAPDUR=30       # 采集窗口, 两条通路共用这一段
export PATH="$PATH:/Users/mac/Library/Android/sdk/platform-tools"

mkdir -p "$OUTDIR"
echo "── [$LABEL] 两通路并排采集 ${CAPDUR}s ──"

adb -s "$SERIAL" shell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUTDIR/snap_at_run.txt" 2>&1 || true

# 1) 负载后台起跑
( cd "$ROOT" && ./phonefarm script --task "xcheck_$LABEL" --serial "$SERIAL" \
    --app "$PKG" --no-screen "$WL" ) > "$OUTDIR/workload.log" 2>&1 &
WL_PID=$!

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

#    B: HiSmartPerf 通路 (设备端每秒一条)
( cd "$ROOT" && ./phonefarm perf --serial "$SERIAL" --source smartperf \
    --app "$PKG" --rounds "$CAPDUR" --json ) \
    > "$OUTDIR/perf_smartperf.json" 2>"$OUTDIR/perf_smartperf.err" &
SP_PID=$!

#    参考: 窗口中点直读一次热区, 用来对 HiSmartPerf 报的温度
sleep $((CAPDUR / 2))
adb -s "$SERIAL" shell "su -c 'for z in /sys/class/thermal/thermal_zone*; do \
  printf \"%s %s\n\" \"\$(cat \$z/type)\" \"\$(cat \$z/temp)\"; done'" \
    > "$OUTDIR/thermal_mid.txt" 2>&1 || true

wait "$FT_PID"  || echo "[$LABEL] 警告: ftrace 采集非零退出"
wait "$SYS_PID" || echo "[$LABEL] 提示: sysfs 通路非零退出 (可能是功耗不可信, 看 JSON 的 unavailable)"
wait "$SP_PID"  || echo "[$LABEL] 提示: HiSmartPerf 通路非零退出 (看 JSON 的 unavailable)"
wait "$WL_PID"  || echo "[$LABEL] 警告: 负载脚本非零退出"

# 3) 拉回 ftrace 原始证据并解析
adb -s "$SERIAL" pull "/data/local/tmp/xcheck_$LABEL.txt" "$OUTDIR/trace.txt" >/dev/null 2>&1
adb -s "$SERIAL" shell "rm -f /data/local/tmp/xcheck_$LABEL.txt" >/dev/null 2>&1
python3 "$ROOT/loop_v1/tools/parse_trace.py" "$OUTDIR/trace.txt" > "$OUTDIR/summary.json"

# 4) 并排成一张表
python3 "$ROOT/loop_v1/tools/crosscheck_report.py" "$OUTDIR" | tee "$OUTDIR/crosscheck.txt"
