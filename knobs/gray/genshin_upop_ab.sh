#!/bin/bash
# genshin_upop_ab.sh — 真超分算子 (gen1_loc3) 在原神大世界的 A/B 验收
#
# 前置: 游戏内「渲染精度」已调到 `低` (pass 49 输出 1134x514), swapchain 2141x969。
#   A 臂 = 挂层 passdump (只读), 游戏自带放大
#   B 臂 = 挂层 upop, 我们的算子 (texelFetch 2x2 bilinear, dst=送显分辨率)
# 两臂都挂层且追踪开销一致 (upop 隐含 passdump), 差值干净对应"算子替换"这一件事。
#
# 采: 帧时 (run_once: fps/p50/p95) + 停充功耗 (phonefarm.new perf) + 电池温度 (臂首尾)
#     + 进大世界站定截帧 (画质比对用, UID 区域比对时排除)。
# 另有一个 ref 臂: 渲染精度极高 + passdump, 取原生参考帧 (需先把设置调回极高再跑,
# 由 REF_ARM=1 单独调用)。
set -uo pipefail

SERIAL="${SERIAL:-91253241019A}"
PKG=com.miHoYo.Yuanshen
ROOT="${ROOT:-/Users/mac/projects/phonefarm}"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${OUT:-$ROOT/loop_v1/runs_graylayer/upop_$(date +%H%M%S)}"
PAIRS="${PAIRS:-5}"
REF_ARM="${REF_ARM:-0}"
GATE_X="${GATE_X:-1342}"; GATE_Y="${GATE_Y:-651}"
MARK=/storage/emulated/0/Android/data/$PKG/files/knobs_layer_out.json
ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }
export PATH="$PATH:/Users/mac/Library/Android/sdk/platform-tools"

mark_field() { ashell "su -c 'cat $MARK 2>/dev/null'" | tr -d '\r' | sed "s/.*\"$1\":\([0-9]*\).*/\1/"; }

in_world() {
  local f1 r1 f2 r2 df dr
  f1=$(mark_field frames); r1=$(mark_field render_pass_begins)
  sleep 10
  f2=$(mark_field frames); r2=$(mark_field render_pass_begins)
  [ -z "$f1" ] || [ -z "$f2" ] && return 1
  df=$((f2 - f1)); dr=$((r2 - r1)); [ "$df" -le 0 ] && return 1
  echo "    passes/帧=$((dr / df)) (>=25 判为大世界)" >&2
  [ $((dr / df)) -ge 25 ]
}

launch_to_world() {
  local i
  for i in $(seq 1 40); do [ -n "$(mark_field frames)" ] && break; sleep 2; done
  [ -n "$(mark_field frames)" ] || { echo "  层没挂上, 本臂作废" >&2; return 1; }
  sleep 30
  ashell "input tap $GATE_X $GATE_Y"
  echo "    已点门, 等进大世界..." >&2
  for i in $(seq 1 24); do
    in_world && { echo "    已进大世界" >&2; return 0; }
  done
  echo "  等不到大世界, 本臂作废" >&2; return 1
}

batt_temp() { ashell "dumpsys battery" | sed -n 's/.*temperature: \([0-9]*\).*/\1/p' | head -1; }

shot() {
  adb -s "$SERIAL" exec-out screencap > "$1" 2>/dev/null
  python3 -c '
import sys, struct
d = open(sys.argv[1], "rb").read()
ok = len(d) > 16 and struct.unpack("<I", d[8:12])[0] == 1
sys.exit(0 if ok else 1)' "$1"
}

arm() { # $1=标签 $2=mode(passdump|upop)
  local label="$1" mode="$2" dir="$OUT/$1"
  mkdir -p "$dir"
  echo "── 臂 $label ($mode) ──"
  bash "$HERE/enable_layer.sh" off >/dev/null 2>&1
  ashell "am force-stop $PKG"
  ashell "su -c 'rm -f $MARK'"
  bash "$HERE/enable_layer.sh" "$mode" "$PKG" >/dev/null 2>&1 || { echo "  挂层失败"; return 1; }
  launch_to_world || return 1
  batt_temp > "$dir/temp_start.txt"
  # 站定截帧: 进世界后镜头固定在角色背后, 位置即登录点 —— 天然同位置同镜头
  sleep 5; shot "$dir/shot_idle.raw" || echo "  截帧失败"
  . "$ROOT/loop_v1/tools/pf_bin.sh"
  ( sleep 10; "$PF" perf --serial "$SERIAL" --app "$PKG" --rounds 8 \
      --power-rail battery --suspend-charging --json > "$dir/power.json" 2>/dev/null ) &
  local PW=$!
  WL="${WL:-$HERE/workload_spin_touch_v1.json}" \
    bash "$ROOT/loop_v1/tools/run_once.sh" "$label" "$dir" 2>&1 | tail -2
  wait $PW 2>/dev/null
  batt_temp > "$dir/temp_end.txt"
  ashell "su -c 'cat $MARK'" | tr -d '\r' > "$dir/knobs_layer_out.json"
  python3 - "$dir" <<'PY' 2>/dev/null || echo "  自报解析失败"
import json,sys
d=sys.argv[1]
m=json.load(open(d+"/knobs_layer_out.json"))
cp=m.get("copyprobe")
pt=[r for r in m.get("pass_table",[]) if r["w"]>=1000]
print(f"  层自报: frames={m['readonly_stats']['frames']} cp={cp}")
print(f"  主分辨率档 pass: {pt}")
p=json.load(open(d+"/power.json"))
print(f"  功耗: {p.get('power_watt')} W on_battery={p.get('meta',{}).get('on_battery')}")
s=json.load(open(d+"/summary.json"))
print(f"  帧: fps={s['fps_mean']} p50={s['frame_p50']} p95={s['frame_p95']} 热事件={s.get('thermal_events','?')}")
t0=open(d+"/temp_start.txt").read().strip(); t1=open(d+"/temp_end.txt").read().strip()
print(f"  电池温度: {int(t0)/10}°C -> {int(t1)/10}°C")
PY
}

mkdir -p "$OUT"
if [ "$REF_ARM" = "1" ]; then
  # 参考臂: 渲染精度极高 + 只读层, 取原生截帧
  arm ref passdump
else
  for i in $(seq 1 "$PAIRS"); do
    arm "a${i}_gameup" passdump || echo "a$i 作废"
    arm "b${i}_upop" upop || echo "b$i 作废"
  done
fi
bash "$HERE/enable_layer.sh" off >/dev/null 2>&1
ashell "am force-stop $PKG" >/dev/null 2>&1
echo "全部完成, 证据: $OUT"
