#!/bin/sh
# 一条命令复现自研超分 + 插帧 (灰档 Vulkan 层) 对游戏的画质/帧率影响, 不改游戏内任何设置:
#   sh loop_v1/selfsr/repro.sh <serial> [配对数, 缺省 6] [输出目录] [包名, 缺省 com.miHoYo.Yuanshen]
# 依次跑 (每组都是冷启动 ABBA, 精确符号翻转检验):
#   1. native  vs layer                          透传层本身的开销
#   2. native  vs sr:$SR_SCALE:$SR_OUT           Game Mode 降分辨率 + 层内超分
#   3. layer   vs ip:$IP_MODE:$IP_FPS             层内插帧
#   4. native  vs srip:...                       超分 + 插帧叠加
# 环境变量: SR_SCALE (0.7) SR_OUT (2141x969:0.8, 宽x高:锐化) IP_MODE (bm) IP_FPS (120) SETS ("1 2 3 4")
# 前提: 手机已 root、游戏已装好并登录过 (onboard 能进大世界)。层用 knobs/gray/build/build_layer.sh 现编。
# 每组结束 (含 Ctrl-C) 都把 Game Mode、debug.knobs.selfsr/interp*、屏幕超时恢复并回读核对, 层由 gray-layer session 摘除。
set -eu
SERIAL="${1:?用法: sh loop_v1/selfsr/repro.sh <serial> [配对数] [输出目录] [包名]}"
ROUNDS="${2:-6}"
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT="${3:-$ROOT/_runs/selfsr_$(date +%Y%m%d-%H%M%S)}"
PKG="${4:-com.miHoYo.Yuanshen}"
SR_SCALE="${SR_SCALE:-0.7}"; SR_OUT="${SR_OUT:-2141x969:0.8}"
IP_MODE="${IP_MODE:-bm}"; IP_FPS="${IP_FPS:-120}"
SETS="${SETS:-1 2 3 4}"
BIN="${PHONEFARM_BIN:-}"
if [ -z "$BIN" ]; then
  BIN="$ROOT/src/target/release/phonefarm"
  [ -x "$BIN" ] || (cd "$ROOT/src" && cargo build --release)
  codesign --force --sign - "$BIN" 2>/dev/null || true
fi
[ "$(adb -s "$SERIAL" shell su -c id 2>/dev/null | grep -c 'uid=0')" = 1 ] || { echo "这台手机没有 root, 层装不进游戏"; exit 2; }
bash "$ROOT/knobs/gray/build/build_layer.sh" >/dev/null
mkdir -p "$OUT"
SO="$OUT/libVkLayer_refknobs.so"
cp "$ROOT/knobs/gray/build/out/libVkLayer_refknobs.so" "$SO"
SRIP_OUT=$(echo "$SR_OUT" | sed 's/:.*//'); SRIP_SHARP=$(echo "$SR_OUT" | sed -n 's/.*://p')
run() {  # run <组名> <A> <B>
  PHONEFARM_BIN="$BIN" python3 -I "$ROOT/loop_v1/selfsr/selfsr_ab.py" run --serial "$SERIAL" --pkg "$PKG" \
    --out "$OUT/$1" --a "$2" --b "$3" --rounds "$ROUNDS" --so "$SO" --phonefarm "$BIN"
}
for g in $SETS; do
  case $g in
    1) run 1_layer native layer ;;
    2) run 2_sr native "sr:$SR_SCALE:$SR_OUT" ;;
    3) run 3_interp layer "ip:$IP_MODE:$IP_FPS" ;;
    4) run 4_sr_interp native "srip:$SR_SCALE:$SRIP_OUT:${SRIP_SHARP:-0}:$IP_MODE:$IP_FPS" ;;
  esac
done
{
  echo "# 自研超分 + 插帧复现: $PKG ($SERIAL)"; echo
  for d in "$OUT"/*/; do [ -f "$d/report.md" ] && { sed 's/^# /## /' "$d/report.md"; echo; }; done
} > "$OUT/summary.md"
echo "报告: $OUT/summary.md"
