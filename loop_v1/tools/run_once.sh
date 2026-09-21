#!/bin/bash
# run_once.sh <标签> <输出目录> [旋钮设置脚本] — 跑一轮"负载 + 采集", 落一份证据
#
# 时序设计:
#   t=0     起 phonefarm script (后台), 负载脚本自己有 3s 静置 + 3×12s 转镜头
#   t=LEAD  起 ftrace 采集, 落在"已经稳定转起来"的窗口内, 避开启动与收尾
#   t=..    等两边都结束, 拉回 trace, 解析成 summary.json
#
# 每轮都是独立目录, 原始 trace 一并归档 —— 判据 5 的离线回放靠的就是这份原始文本。
set -euo pipefail

LABEL="${1:?用法: run_once.sh <标签> <输出目录> [旋钮脚本]}"
OUTDIR="${2:?缺输出目录}"
KNOB="${3:-}"

SERIAL=91253241019A
PKG=com.miHoYo.Yuanshen
ROOT=/Users/mac/projects/phonefarm
WL="$ROOT/loop_v1/scripts/workload_spin_v1.json"
LEAD=6          # 负载起跑后等几秒再开采 (让转镜头进入稳态)
CAPDUR=30       # 采集时长, 必须 < 负载剩余时长
export PATH="$PATH:/Users/mac/Library/Android/sdk/platform-tools"

mkdir -p "$OUTDIR"
echo "── [$LABEL] 开始 ──"

# 旋钮: 有就先施加 (旋钮脚本自己负责幂等与记录)
if [ -n "$KNOB" ]; then
  echo "[$LABEL] 施加旋钮: $KNOB"
  adb -s "$SERIAL" shell "su -c 'sh $KNOB'" 2>&1 | sed "s/^/[$LABEL][旋钮] /"
fi

# 轮内起始快照 (证明旋钮确实生效, 也为回滚核验留底)
adb -s "$SERIAL" shell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUTDIR/snap_at_run.txt" 2>&1

# 1) 负载后台起跑
( cd "$ROOT" && ./phonefarm script --task "loop_v1_$LABEL" --serial "$SERIAL" \
    --app "$PKG" --no-screen "$WL" ) > "$OUTDIR/workload.log" 2>&1 &
WL_PID=$!

# 2) 等负载进入稳态后开采
sleep "$LEAD"
echo "[$LABEL] 采集 ${CAPDUR}s ..."
adb -s "$SERIAL" shell "su -c 'sh /data/local/tmp/ftrace_capture.sh $CAPDUR /data/local/tmp/loop_v1_$LABEL.txt'" \
    > "$OUTDIR/capture.log" 2>&1

# 3) 等负载收尾
wait "$WL_PID" || echo "[$LABEL] 警告: 负载脚本非零退出"

# 4) 拉回原始证据并解析
adb -s "$SERIAL" pull "/data/local/tmp/loop_v1_$LABEL.txt" "$OUTDIR/trace.txt" >/dev/null 2>&1
adb -s "$SERIAL" shell "rm -f /data/local/tmp/loop_v1_$LABEL.txt" >/dev/null 2>&1

python3 "$ROOT/loop_v1/tools/parse_trace.py" "$OUTDIR/trace.txt" > "$OUTDIR/summary.json"

# 5) 轮末快照 (与轮内对比, 确认采集器自己没留痕)
adb -s "$SERIAL" shell "su -c 'sh /data/local/tmp/device_snapshot.sh'" > "$OUTDIR/snap_after_run.txt" 2>&1

SZ=$(wc -c < "$OUTDIR/trace.txt" | tr -d ' ')
echo "[$LABEL] 完成: trace ${SZ}B"
python3 -c "
import json,sys
d=json.load(open('$OUTDIR/summary.json'))
print(f\"[$LABEL] fps={d['fps_mean']} spf={d['submits_per_frame']} \"
      f\"p50={d['frame_p50']}ms p95={d['frame_p95']}ms gpu={d['gpu_active_mean']}ms \"
      f\"bw={d['bw_median']} 热事件={d['n_thermal_events']}\")
"
