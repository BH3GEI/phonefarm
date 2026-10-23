#!/bin/sh
# 一条命令复现「整机插帧+超分」对原神的显示帧率/画质提升 (同局 A/B, 不改游戏设置):
#   sh loop_v1/gfrc/repro.sh <serial> [配对数, 缺省 6] [输出目录]
# 前提: 手机已 root、游戏已装好并登录过 (onboard 能进大世界)、vendor 有 G-FRC+ (ro.vendor.feature.zte_feature_gfrc=true)。
# 结束 (含 Ctrl-C) 时 settings global game_gfrc_mode 写回原值并回读核对。
set -eu
SERIAL="${1:?用法: sh loop_v1/gfrc/repro.sh <serial> [配对数] [输出目录]}"
ROUNDS="${2:-6}"
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT="${3:-$ROOT/_runs/gfrc_ab_$(date +%Y%m%d-%H%M%S)}"
BIN="${PHONEFARM_BIN:-}"
if [ -z "$BIN" ]; then
  BIN="$ROOT/src/target/release/phonefarm"
  [ -x "$BIN" ] || (cd "$ROOT/src" && cargo build --release)
fi
[ "$(adb -s "$SERIAL" shell getprop ro.vendor.feature.zte_feature_gfrc | tr -d '\r')" = true ] \
  || { echo "这台机器没有 G-FRC+ (ro.vendor.feature.zte_feature_gfrc != true)"; exit 2; }
PHONEFARM_BIN="$BIN" "$ROOT/tools/devlock" run gfrc-repro -- \
  python3 -I "$ROOT/loop_v1/gfrc/gfrc_ab.py" --serial "$SERIAL" --rounds "$ROUNDS" --out "$OUT" --phonefarm "$BIN"
echo "报告: $OUT/report.md"
