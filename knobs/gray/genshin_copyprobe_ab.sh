#!/bin/bash
# genshin_copyprobe_ab.sh — 1:1 拷贝探针验收 (FEASIBILITY 灰档改写 #2 的前置探针)
#
# 探针 (debug.knobs.copyprobe=1) 把超分要用的机制以最小形态全走一遍:
# 建 compute pipeline + 建 image + 注入 dispatch + 克隆并替换那一次描述符绑定,
# 算子是 texelFetch 逐纹素拷贝 —— 游戏的放大 shader 输入逐位相同, 画面应逐像素不变。
#
# 验收三条:
#   ① 反作弊放行: 探针臂能登录进大世界、跑满一轮负载不崩 (层自报 copies/subs 持续涨)
#   ② 画面逐像素: 探针臂与无层臂截帧差异 ≈ 两无层臂之间的差异 (控制对)
#   ③ 帧时: 探针臂 p50/p95 与无层臂相比不显著变化
#
# 臂: none1 none2 (无层控制对) + probe1..N (探针)。无层臂没有层 marker,
# 进世界判定改用截帧亮度方差 (登录页接近纯白低方差, 大世界高方差)。
set -uo pipefail

SERIAL="${SERIAL:-91253241019A}"
PKG=com.miHoYo.Yuanshen
ROOT="${ROOT:-/Users/mac/projects/phonefarm}"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${OUT:-$ROOT/loop_v1/runs_graylayer/copyprobe_$(date +%H%M%S)}"
PROBES="${PROBES:-3}"
GATE_X="${GATE_X:-1342}"; GATE_Y="${GATE_Y:-651}"
MARK=/storage/emulated/0/Android/data/$PKG/files/knobs_layer_out.json
ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }
export PATH="$PATH:/Users/mac/Library/Android/sdk/platform-tools"

mark_field() { ashell "su -c 'cat $MARK 2>/dev/null'" | tr -d '\r' | sed "s/.*\"$1\":\([0-9]*\).*/\1/"; }

# 有层臂: 用每帧 render pass 数判 (大世界 ≈30, 登录页 ≈12-17)
in_world_by_layer() {
  local f1 r1 f2 r2 df dr
  f1=$(mark_field frames); r1=$(mark_field render_pass_begins)
  sleep 10
  f2=$(mark_field frames); r2=$(mark_field render_pass_begins)
  [ -z "$f1" ] || [ -z "$f2" ] && return 1
  df=$((f2 - f1)); dr=$((r2 - r1)); [ "$df" -le 0 ] && return 1
  echo "    passes/帧=$((dr / df)) (>=25 判为大世界)" >&2
  [ $((dr / df)) -ge 25 ]
}

# 无层臂: 截帧亮度方差判 (登录页/门接近纯白, 大世界纹理方差大)
in_world_by_pixels() {
  adb -s "$SERIAL" exec-out screencap 2>/dev/null | python3 -c '
import sys, struct
d = sys.stdin.buffer.read()
if len(d) < 16: sys.exit(1)
w, h, fmt = struct.unpack("<III", d[:12])
if fmt != 1 or len(d) < 12 + w*h*4: sys.exit(1)
px = d[12:12+w*h*4]
# 隔 97 像素采样亮度, 算方差
vals = []
for i in range(0, w*h, 97):
    o = i*4
    vals.append(px[o]*2 + px[o+1]*3 + px[o+2])   # 近似 luma, 省浮点
m = sum(vals)/len(vals)
var = sum((v-m)**2 for v in vals)/len(vals)
print(f"    截帧亮度方差={var:.0f} (>=200000 判为大世界)", file=sys.stderr)
sys.exit(0 if var >= 200000 else 1)'
}

launch_to_world() {  # $1 = layered(0|1)
  local layered="$1" i
  if [ "$layered" = "1" ]; then
    for i in $(seq 1 40); do [ -n "$(mark_field frames)" ] && break; sleep 2; done
    [ -n "$(mark_field frames)" ] || { echo "  层没挂上, 本臂作废" >&2; return 1; }
  else
    for i in $(seq 1 30); do ashell "pidof $PKG" >/dev/null 2>&1 && [ -n "$(ashell 'pidof $PKG' | tr -d "\r")" ] && break; sleep 2; done
    sleep 25   # 等渲染起稳
  fi
  sleep 30
  ashell "input tap $GATE_X $GATE_Y"
  echo "    已点门, 等进大世界..." >&2
  for i in $(seq 1 24); do
    if [ "$layered" = "1" ]; then in_world_by_layer && { echo "    已进大世界" >&2; return 0; }
    else in_world_by_pixels && { echo "    已进大世界(像素判)" >&2; return 0; }; fi
  done
  echo "  等不到大世界, 本臂作废" >&2; return 1
}

shot() {  # $1 = 输出文件 (raw RGBA 截帧)
  adb -s "$SERIAL" exec-out screencap > "$1" 2>/dev/null
  python3 -c '
import sys, struct
d = open(sys.argv[1], "rb").read()
ok = len(d) > 16 and struct.unpack("<I", d[8:12])[0] == 1
sys.exit(0 if ok else 1)' "$1"
}

arm() { # $1=标签 $2=probe(0|1)
  local label="$1" probe="$2" dir="$OUT/$1"
  mkdir -p "$dir"
  echo "── 臂 $label (probe=$probe) ──"
  bash "$HERE/enable_layer.sh" off >/dev/null 2>&1
  ashell "am force-stop $PKG"
  ashell "su -c 'rm -f $MARK'"
  if [ "$probe" = "1" ]; then
    bash "$HERE/enable_layer.sh" copyprobe "$PKG" >/dev/null 2>&1 || { echo "  挂层失败"; return 1; }
    launch_to_world 1 || return 1
  else
    launch_to_world 0 || return 1   # 无层: 不推 .so, 干净基线
  fi
  WL="${WL:-$HERE/workload_spin_touch_v1.json}" \
    bash "$ROOT/loop_v1/tools/run_once.sh" "$label" "$dir" 2>&1 | tail -2
  # 负载结束人物站定后连拍三张, 离线比对用中间那张
  sleep 3
  for k in 1 2 3; do shot "$dir/shot$k.raw" || echo "  截帧 $k 失败"; sleep 2; done
  if [ "$probe" = "1" ]; then
    ashell "su -c 'cat $MARK'" | tr -d '\r' > "$dir/knobs_layer_out.json"
    python3 - "$dir/knobs_layer_out.json" <<'PY' 2>/dev/null || echo "  层自报: 解析失败"
import json,sys
d=json.load(open(sys.argv[1]))
cp=d.get("copyprobe")
print(f"  层自报: frames={d['readonly_stats']['frames']} copyprobe={cp}")
PY
  fi
  python3 - "$dir/summary.json" <<'PY' 2>/dev/null || echo "  帧数据: 解析失败"
import json,sys
d=json.load(open(sys.argv[1]))
print(f"  帧: fps={d['fps_mean']} p50={d['frame_p50']}ms p95={d['frame_p95']}ms spf={d['submits_per_frame']}")
PY
}

mkdir -p "$OUT"
arm none1 0 || echo "none1 作废"
arm none2 0 || echo "none2 作废"
for i in $(seq 1 "$PROBES"); do arm "probe$i" 1 || echo "probe$i 作废"; done
bash "$HERE/enable_layer.sh" off >/dev/null 2>&1
ashell "am force-stop $PKG" >/dev/null 2>&1
echo "全部完成, 证据: $OUT"
