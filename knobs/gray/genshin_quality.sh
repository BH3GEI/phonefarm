#!/bin/bash
# genshin_quality.sh — 画质对照: 同位置同时辰截帧
# 用法:
#   bash genshin_quality.sh begin <passdump|upop> <label>   # 挂层重启进大世界, 打开时钟面板, 截图读当前时辰
#   SWIPE="x1 y1 x2 y2" bash genshin_quality.sh finish <label>  # 拖到12:00, 确认, 关菜单, 截世界帧
set -uo pipefail
SERIAL="${SERIAL:-91253241019A}"
PKG=com.miHoYo.Yuanshen
ROOT="${ROOT:-/Users/mac/projects/phonefarm}"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${OUT:-$ROOT/loop_v1/runs_graylayer/upop_141624/quality}"
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
  echo "    passes/帧=$((dr / df))" >&2
  [ $((dr / df)) -ge 25 ]
}

launch_to_world() {
  local i
  for i in $(seq 1 40); do [ -n "$(mark_field frames)" ] && break; sleep 2; done
  [ -n "$(mark_field frames)" ] || { echo "  层没挂上" >&2; return 1; }
  sleep 30
  ashell "input tap $GATE_X $GATE_Y"
  echo "    已点门, 等进大世界..." >&2
  for i in $(seq 1 24); do in_world && { echo "    已进大世界" >&2; return 0; }; done
  return 1
}

raw2png() { # $1=raw $2=png (半分辨率)
  python3 - "$1" "$2" <<'PY'
import struct, zlib, sys
d = open(sys.argv[1],'rb').read()
w,h,fmt = struct.unpack('<III', d[:12])
px = d[12:12+w*h*4]
ow,oh = w//2, h//2
out = bytearray()
for y in range(oh):
    out.append(0)
    row = y*2
    for x in range(ow):
        o = (row*w + x*2)*4
        out += px[o:o+3]
def chunk(t, data):
    c = struct.pack('>I', len(data)) + t + data
    return c + struct.pack('>I', zlib.crc32(t+data))
png = b'\x89PNG\r\n\x1a\n'
png += chunk(b'IHDR', struct.pack('>IIBBBBB', ow, oh, 8, 2, 0, 0, 0))
png += chunk(b'IDAT', zlib.compress(bytes(out)))
png += chunk(b'IEND', b'')
open(sys.argv[2],'wb').write(png)
PY
}

shot() { adb -s "$SERIAL" exec-out screencap > "$1" 2>/dev/null; }

begin() { # $1=mode $2=label
  local dir="$OUT/$2"; mkdir -p "$dir"
  echo "── 画质臂 $2 ($1) ──"
  bash "$HERE/enable_layer.sh" off >/dev/null 2>&1
  ashell "am force-stop $PKG"; ashell "su -c 'rm -f $MARK'"
  bash "$HERE/enable_layer.sh" "$1" "$PKG" >/dev/null 2>&1 || { echo "  挂层失败"; return 1; }
  launch_to_world || return 1
  # 站定后打开时钟面板: Paimon(190,90) -> 时钟(170,834)
  ashell "input tap 190 90"; sleep 3
  ashell "input tap 170 834"; sleep 3
  shot "$dir/clock_before.raw"; raw2png "$dir/clock_before.raw" "$dir/clock_before.png"
  echo "  时钟面板截图: $dir/clock_before.png — 读出当前时辰后用 SWIPE=... finish $2"
}

finish() { # $1=label, SWIPE="x1 y1 x2 y2"
  local dir="$OUT/$1"
  [ -n "${SWIPE:-}" ] || { echo "需要 SWIPE=\"x1 y1 x2 y2\""; return 1; }
  ashell "input swipe $SWIPE 2500"; sleep 1
  shot "$dir/clock_after.raw"; raw2png "$dir/clock_after.raw" "$dir/clock_after.png"
  ashell "input tap 1970 1134"   # 确认
  sleep 12                        # 跳时动画
  ashell "input tap 2486 80"      # X 关时钟面板
  sleep 3
  ashell "input tap 154 66"       # 返回大世界
  sleep 8                         # 等 toast 消退 / 画面稳定
  shot "$dir/shot.raw"; raw2png "$dir/shot.raw" "$dir/shot.png"
  ashell "su -c 'cat $MARK'" | tr -d '\r' > "$dir/knobs_layer_out.json"
  echo "  世界帧: $dir/shot.png  层自报: $(head -c 300 "$dir/knobs_layer_out.json")"
}

case "${1:-}" in
  begin)  begin "$2" "$3" ;;
  finish) finish "$2" ;;
  *) echo "usage: $0 begin <mode> <label> | SWIPE=... $0 finish <label>" ;;
esac
