#!/bin/bash
# run_refbench.sh <label> <outdir> <scene> <intensity> <loadop> <frames> [capdur]
# 驱动 refbench 白盒靶子跑一轮: 温度回落轮询 → 启动 → 采集 → 等自退 → 收证据。
#
# 退出码: 0=有效轮  2=硬失败  3=无效轮 (采集窗内有热事件或非干净退出, 证据保留)
# 时序全部用存活/状态轮询, 无时长盲等。
#
# 注: 这是 refbench 负载的专属驱动面, 刻意收在本文件里 —— harness v2 的负载插件
# 接口落地后, 本文件整体即插件实现, 编排侧不需要知道这里的任何细节。
set -euo pipefail

LABEL="${1:?用法: run_refbench.sh <label> <outdir> <scene> <intensity> <loadop> <frames> [capdur]}"
OUTDIR="${2:?缺输出目录}"
SCENE="${3:?缺 scene}"
INTEN="${4:?缺 intensity}"
LOADOP="${5:?缺 loadop}"
FRAMES="${6:?缺 frames}"
CAPDUR="${7:-12}"   # 12s ≈ 1200+ 帧, 够稳分位; 比 20s 少积热 → 更易拿到 0 热事件轮

SERIAL="${REFBENCH_SERIAL:-91253241019A}"
PKG=io.github.hgamey.refbench
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TOOLS="$ROOT/loop_v1/tools"
COOL_TO="${REFBENCH_COOL_MC:-40000}"   # kgsl 温度回落阈值, 毫摄氏度

ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }

mkdir -p "$OUTDIR"

# 1) 温度回落轮询 (有界: 360 次 × 2s), 保证每轮起点热态一致
T=999999
for _ in $(seq 1 360); do
  T=$(ashell "su -c 'cat /sys/class/kgsl/kgsl-3d0/temp'" 2>/dev/null | tr -d '\r' || true)
  [ -n "$T" ] && [ "$T" -le "$COOL_TO" ] && break
  sleep 2
done
echo "[$LABEL] 起跑 gpu_temp=${T}mC"

# 2) 轮内起始快照 + 清场 (上一轮的输出与开始标记都清掉)
APPFILES="/storage/emulated/0/Android/data/$PKG/files"
ashell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUTDIR/snap_at_run.txt" 2>&1
ashell "am force-stop $PKG" >/dev/null 2>&1 || true
ashell "rm -f $APPFILES/refbench_out.json $APPFILES/refbench_started" >/dev/null 2>&1 || true

# 3) 启动并等渲染真正开始 (靶子进渲染循环前落 refbench_started 标记;
#    不用 logcat —— 这台设备的 logcat 会整个哑掉, 进度信号全走文件系统)
ashell "am start -n $PKG/android.app.NativeActivity --es scene $SCENE --es run_id $LABEL --es frames $FRAMES --es intensity $INTEN --es knob.loadop $LOADOP" > "$OUTDIR/am.log" 2>&1
STARTED=0
for _ in $(seq 1 40); do
  M=$(ashell "cat $APPFILES/refbench_started 2>/dev/null" | tr -d '\r' || true)
  [ -n "$M" ] && { STARTED=1; break; }
  sleep 0.5
done
[ "$STARTED" = 1 ] || { echo "[$LABEL] 应用未进入渲染"; ashell "am force-stop $PKG" || true; exit 2; }

# 4) 稳态窗口内采集 (ftrace_capture.sh 自带四项状态存档还原)
ashell "su -c 'sh /data/local/tmp/ftrace_capture.sh $CAPDUR /data/local/tmp/rb_$LABEL.txt'" > "$OUTDIR/capture.log" 2>&1
adb -s "$SERIAL" pull "/data/local/tmp/rb_$LABEL.txt" "$OUTDIR/trace.txt" >/dev/null
ashell "rm -f /data/local/tmp/rb_$LABEL.txt" || true

# 5) 等应用渲染完固定帧数自退 (存活轮询, 有界 180s)
EXITED=0
for _ in $(seq 1 180); do
  P=$(ashell "pidof $PKG" 2>/dev/null | tr -d '\r' || true)
  [ -z "$P" ] && { EXITED=1; break; }
  sleep 1
done
[ "$EXITED" = 1 ] || { echo "[$LABEL] 应用超时未自退"; ashell "am force-stop $PKG" || true; exit 2; }

# 6) 收证据: 靶子自报 JSON + 轮末快照 + 解析 + 归因
adb -s "$SERIAL" pull "/storage/emulated/0/Android/data/$PKG/files/refbench_out.json" "$OUTDIR/refbench_out.json" >/dev/null
ashell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUTDIR/snap_after_run.txt" 2>&1
python3 "$TOOLS/parse_trace.py" "$OUTDIR/trace.txt" --comm RefbenchDrv > "$OUTDIR/summary.json"
python3 "$TOOLS/attribute.py" "$OUTDIR/summary.json" > "$OUTDIR/attribution.json"

# 7) 轮内有效性判定: 采集窗有热事件 / 非干净退出 → 无效 (证据保留, 不进样本)
NT=$(python3 -c "import json;print(json.load(open('$OUTDIR/summary.json'))['n_thermal_events'])")
CLEAN=$(python3 -c "import json;print(json.load(open('$OUTDIR/refbench_out.json'))['clean_exit'])")
if [ "$NT" != "0" ] || [ "$CLEAN" != "True" ]; then
  echo "n_thermal=$NT clean_exit=$CLEAN" > "$OUTDIR/INVALID"
  echo "[$LABEL] 无效轮: 热事件=$NT clean=$CLEAN"
  exit 3
fi

python3 - "$OUTDIR/summary.json" "$LABEL" <<'PYEOF'
import json, sys
d = json.load(open(sys.argv[1]))
print(f"[{sys.argv[2]}] fps={d['fps_mean']} spf={d['submits_per_frame']} "
      f"p50={d['frame_p50']}ms p95={d['frame_p95']}ms gpu={d['gpu_active_mean']}ms bw={d['bw_median']}")
PYEOF
