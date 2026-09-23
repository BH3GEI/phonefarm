#!/bin/bash
# run_megacity.sh <label> <outdir> <scene> <route> <frames> [capdur]
#
# 驱动 megacity 载体跑一轮: 温度回落轮询 → 推路线 → 启动 → 采集 → 等自退 → 收证据。
# 结构刻意与 loop_v1/refbench/run_refbench.sh 对齐, 只在载体差异处分叉。
#
# 退出码: 0=有效轮  2=硬失败  3=无效轮 (采集窗内有热事件或非干净退出, 证据保留)
# 时序全部用存活/状态轮询, 无时长盲等。
#
# !! 状态: 未实跑验证 !! APK 已经有了, 但本脚本还没跑过一次。
#
# 测试条件(影响可比性, 别混批):
#   - 红魔内置风扇全程开启
#   - 设备共享, 外面套 devlock:
#       /private/tmp/claude-501/devlock run wb-megacity -- bash run_megacity.sh ...
set -euo pipefail

LABEL="${1:?用法: run_megacity.sh <label> <outdir> <scene> <route> <frames> [capdur]}"
OUTDIR="${2:?缺输出目录}"
SCENE="${3:?缺 scene (city_flythrough|city_static)}"
ROUTE="${4:?缺 route 名}"
FRAMES="${5:?缺 frames}"
CAPDUR="${6:-12}"

SERIAL="${MEGACITY_SERIAL:-91253241019A}"
PKG=com.unity.megacity.metro
ACT=com.unity.megacity.MegacityMetro
WARMUP="${MEGACITY_WARMUP:-600}"
RESSCALE="${MEGACITY_RESSCALE:-1.0}"
VSYNC="${MEGACITY_VSYNC:-off}"

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
TOOLS="$ROOT/loop_v1/tools"
HERE="$(cd "$(dirname "$0")" && pwd)"
COOL_TO="${MEGACITY_COOL_MC:-40000}"

ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }

mkdir -p "$OUTDIR"
APPFILES="/storage/emulated/0/Android/data/$PKG/files"

# 0) 路线文件必须真实存在 —— 没有就停, 不拿默认相机顶
ROUTE_SRC="$HERE/routes/$ROUTE.json"
[ -f "$ROUTE_SRC" ] || { echo "[$LABEL] 路线文件不存在: $ROUTE_SRC (见 README 的路线标定一节)"; exit 2; }

# 1) 温度回落轮询 (有界 360×2s), 保证每轮起点热态一致
T=999999
for _ in $(seq 1 360); do
  T=$(ashell "su -c 'cat /sys/class/kgsl/kgsl-3d0/temp'" 2>/dev/null | tr -d '\r' || true)
  [ -n "$T" ] && [ "$T" -le "$COOL_TO" ] && break
  sleep 2
done
echo "[$LABEL] 起跑 gpu_temp=${T}mC"

# 2) 轮内起始快照 + 清场
ashell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUTDIR/snap_at_run.txt" 2>&1
ashell "am force-stop $PKG" >/dev/null 2>&1 || true
ashell "rm -f $APPFILES/megacity_out.json $APPFILES/megacity_started" >/dev/null 2>&1 || true

# 3) 推路线。放 app files 下, 换路线不用重新出包; 同时留一份进证据目录, 便于事后核对
ashell "mkdir -p $APPFILES/routes" >/dev/null 2>&1 || true
adb -s "$SERIAL" push "$ROUTE_SRC" "$APPFILES/routes/$ROUTE.json" >/dev/null
cp "$ROUTE_SRC" "$OUTDIR/route.json"

# 4) 启动并等渲染真正进入被测窗口 (harness 预热结束才落 megacity_started;
#    同样不用 logcat —— 这台设备的 logcat 会整个哑掉)
ashell "am start -W -n $PKG/$ACT \
  --es scene $SCENE --es run_id $LABEL --es frames $FRAMES --es route $ROUTE \
  --es warmup $WARMUP --es resscale $RESSCALE --es vsync $VSYNC" > "$OUTDIR/am.log" 2>&1

# 预热窗口可能很长 (subscene 流式加载), 所以这里的上界比 refbench 宽: 300×0.5s = 150s
STARTED=0
for _ in $(seq 1 300); do
  M=$(ashell "cat $APPFILES/megacity_started 2>/dev/null" | tr -d '\r' || true)
  [ -n "$M" ] && { STARTED=1; break; }
  P=$(ashell "pidof $PKG" 2>/dev/null | tr -d '\r' || true)
  [ -z "$P" ] && { echo "[$LABEL] 应用在进入被测窗口前就退了"; break; }
  sleep 0.5
done
[ "$STARTED" = 1 ] || { echo "[$LABEL] 应用未进入被测窗口"; ashell "am force-stop $PKG" || true; exit 2; }

# 5) 稳态窗口内采集
ashell "su -c 'sh /data/local/tmp/ftrace_capture.sh $CAPDUR /data/local/tmp/mc_$LABEL.txt'" > "$OUTDIR/capture.log" 2>&1
adb -s "$SERIAL" pull "/data/local/tmp/mc_$LABEL.txt" "$OUTDIR/trace.txt" >/dev/null
ashell "rm -f /data/local/tmp/mc_$LABEL.txt" || true

# 6) 等自退 (存活轮询, 有界 300s —— 帧数多时窗口比 refbench 长)
EXITED=0
for _ in $(seq 1 300); do
  P=$(ashell "pidof $PKG" 2>/dev/null | tr -d '\r' || true)
  [ -z "$P" ] && { EXITED=1; break; }
  sleep 1
done
[ "$EXITED" = 1 ] || { echo "[$LABEL] 应用超时未自退"; ashell "am force-stop $PKG" || true; exit 2; }

# 7) 收证据
adb -s "$SERIAL" pull "$APPFILES/megacity_out.json" "$OUTDIR/megacity_out.json" >/dev/null
ashell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUTDIR/snap_after_run.txt" 2>&1

# 提交线程名由载体自报, 不写死 —— Unity 的图形线程名不归我们改, 只归我们如实记录
COMM=$(python3 -c "import json;print(json.load(open('$OUTDIR/megacity_out.json'))['render_thread_comm'])")
case "$COMM" in
  UNRESOLVED*) echo "[$LABEL] 载体没能定位图形提交线程: $COMM"; exit 2 ;;
esac
echo "[$LABEL] comm=$COMM"

python3 "$TOOLS/parse_trace.py" "$OUTDIR/trace.txt" --comm "$COMM" > "$OUTDIR/summary.json"
python3 "$TOOLS/attribute.py" "$OUTDIR/summary.json" > "$OUTDIR/attribution.json"

# 8) 轮内有效性判定
python3 - "$OUTDIR" "$LABEL" <<'PYEOF'
import json, sys, os
out, label = sys.argv[1], sys.argv[2]
s = json.load(open(os.path.join(out, "summary.json")))
m = json.load(open(os.path.join(out, "megacity_out.json")))

bad = []
if s["n_thermal_events"] != 0:            bad.append(f"热事件={s['n_thermal_events']}")
if not m["clean_exit"]:                   bad.append("clean_exit=false:" + m.get("abort_reason", "?"))
if m["graphics"]["api"] != "Vulkan":      bad.append("图形 API=" + m["graphics"]["api"] + " (不是 Vulkan, 这包白打了)")
if m["frames_rendered"] != m["params"]["frames"]:
    bad.append(f"帧数不足 {m['frames_rendered']}/{m['params']['frames']}")

if bad:
    open(os.path.join(out, "INVALID"), "w").write("; ".join(bad) + "\n")
    print(f"[{label}] 无效轮: " + "; ".join(bad))
    sys.exit(3)

print(f"[{label}] fps={s['fps_mean']} spf={s['submits_per_frame']} "
      f"p50={s['frame_p50']}ms p95={s['frame_p95']}ms gpu={s['gpu_active_mean']}ms bw={s['bw_median']} "
      f"route={m['route']['name']}@{m['route']['sha256'][:12]}")
PYEOF
