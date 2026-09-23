#!/bin/bash
# genshin_loadop_ab.sh — 原神上 LoadOp 改写的 A/B 交替采集 (判据 5)
#
# 两臂**都挂层**, 唯一差别是属性 debug.knobs.loadop:
#   ctrl = 层 + 改写关 (只读档)      knob = 层 + 改写开
#
# 为什么不是"有层/无层"对照: 要判的是**这条改写**值不值, 不是"挂层这件事"值不值。
# 两臂都挂层才能把层本身的开销消掉, 差值才干净地对应 LoadOp 这一个改动。
# 层本身的开销另有 runs_graylayer/probe1 (挂层跑满一轮) 与无层历史基线可比。
# 附带好处: 两臂都有层的 marker, "进没进大世界"的判定逻辑对两臂完全一致。
#
# 每臂都要重启原神 —— loadOp 是 render pass **创建期**烘进去的, 属性也是
# CreateInstance 时读的, 跑起来之后切不动。
#
# 原神里**不按 A 键、不按 BACK**: 全程只在登录页点一次"门", 负载只在画面空白处拖视角。
#
# 负载用 knobs 自带的 workload_spin_touch_v1.json 而不是 loop_v1/scripts/workload_spin_v1.json:
# 后者靠虚拟手柄右摇杆转视角, 2026-09-23 原神更新到 7.1.0 后**注入已失效** ——
# 跑的时候 /proc/bus/input/devices 里根本不出现虚拟手柄, 画面全程静止。
# 用它采到的轮次测的是静止画面, 不是预定负载, 已全部作废。
# 触控版在画面空白区 (900,400)->(2000,400) 做 3 x 12s 匀速拖拽, 实测能稳定转视角、
# 人物位置不变。起止点避开左侧移动摇杆区、右下技能键与右上角色头像, 避免误触。
set -uo pipefail

SERIAL="${SERIAL:-91253241019A}"
PKG=com.miHoYo.Yuanshen
ROOT="${ROOT:-/Users/mac/projects/phonefarm}"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${OUT:-$ROOT/loop_v1/runs_graylayer}"
PAIRS="${PAIRS:-5}"
GATE_X="${GATE_X:-1342}"; GATE_Y="${GATE_Y:-651}"     # 登录页那扇"门"
MARK=/storage/emulated/0/Android/data/$PKG/files/knobs_layer_out.json
ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }

mark_field() { ashell "su -c 'cat $MARK 2>/dev/null'" | tr -d '\r' | sed "s/.*\"$1\":\([0-9]*\).*/\1/"; }

# 进没进大世界: 用**每帧 render pass 数**判, 不用帧率 (本机原神登录页和大世界都被限在 30fps)。
# 实测 登录/加载 ≈ 12-17 passes/帧, 大世界 ≈ 30 passes/帧, 余量很大。
in_world() {
  local f1 r1 f2 r2 df dr
  f1=$(mark_field frames); r1=$(mark_field render_pass_begins)
  sleep 10
  f2=$(mark_field frames); r2=$(mark_field render_pass_begins)
  [ -z "$f1" ] || [ -z "$f2" ] && return 1
  df=$((f2 - f1)); dr=$((r2 - r1))
  [ "$df" -le 0 ] && return 1
  local ppf=$((dr / df))
  echo "    passes/帧=$ppf (>=25 判为大世界)" >&2
  [ "$ppf" -ge 25 ]
}

launch_to_world() {
  local i
  # 等渲染起来 (marker 出现)
  for i in $(seq 1 40); do [ -n "$(mark_field frames)" ] && break; sleep 2; done
  [ -n "$(mark_field frames)" ] || { echo "  层没挂上, 本臂作废" >&2; return 1; }
  # 等登录页的门出现再点; 点早了会落空
  sleep 30
  ashell "input tap $GATE_X $GATE_Y"
  echo "    已点门, 等进大世界..." >&2
  for i in $(seq 1 18); do
    in_world && { echo "    已进大世界" >&2; return 0; }
  done
  echo "  等不到大世界, 本臂作废" >&2; return 1
}

arm() { # $1=标签 $2=rewrite(0|1) $3=输出目录
  local label="$1" rw="$2" dir="$3"
  echo "── 臂 $label (rewrite=$rw) ──"
  bash "$HERE/enable_layer.sh" off >/dev/null 2>&1
  ashell "am force-stop $PKG"
  ashell "su -c 'rm -f $MARK'"
  if [ "$rw" = "1" ]; then
    bash "$HERE/enable_layer.sh" loadop "$PKG" >/dev/null 2>&1 || { echo "  挂层失败"; return 1; }
  else
    bash "$HERE/enable_layer.sh" target "$PKG" >/dev/null 2>&1 || { echo "  挂层失败"; return 1; }
  fi
  launch_to_world || return 1
  # 功耗与帧数据同窗口采: run_once 的采集窗口是 t=6..36s, 这里 t=10 起采约 12s, 落在窗口内。
  # 必须 --suspend-charging: 插着电时电池轨读到的是"充电电流与系统耗电相抵后的余量",
  # 看起来完全合理却不是整机功耗 (PR #27)。电量 100% 远高于 30% 的停充下限。
  ( sleep 10; "$ROOT/phonefarm.new" perf --serial "$SERIAL" --app "$PKG" --rounds 8 \
      --power-rail battery --suspend-charging --json > "$dir/power.json" 2>/dev/null ) &
  local PW=$!
  WL="${WL:-$HERE/workload_spin_touch_v1.json}" \
    bash "$ROOT/loop_v1/tools/run_once.sh" "$label" "$dir" 2>&1 | tail -2
  wait $PW 2>/dev/null
  python3 - "$dir/power.json" <<'PY' 2>/dev/null || echo "  功耗: 采失败"
import json,sys
d=json.load(open(sys.argv[1]))
m=d.get('meta',{})
print(f"  功耗: {d.get('power_watt')} W  on_battery={m.get('on_battery')} status={m.get('battery_status')} 电量={m.get('battery_capacity_pct')}%")
PY
  ashell "su -c 'cat $MARK'" | tr -d '\r' > "$dir/knobs_layer_out.json"
  echo "  层自报: $(head -c 320 "$dir/knobs_layer_out.json")"
  # 轮次有效性自检。原神每帧发 2 次 cmdbatch (loop_v1 README 有记), 所以 spf 必须是 2。
  # 帧时按**游戏内帧率设置**分两档判: 60 帧档 ~16.7ms, 30 帧档 ~33.3ms。
  # 早先只认 30 帧档, 用户把游戏调到 60 帧后整批轮次被误判成"可疑" —— 阈值的问题, 不是数据的问题。
  python3 - "$dir/summary.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1])); p=d['frame_p50']
ok = d['submits_per_frame']==2 and (15.5<=p<=18.0 or 31.0<=p<=36.0)
print(f"  自检: spf={d['submits_per_frame']} p50={p}ms fps={d['fps_mean']} -> {'有效' if ok else '可疑, 该轮建议丢弃'}")
PY
}

mkdir -p "$OUT"
for i in $(seq 1 "$PAIRS"); do
  arm "ctrl$i" 0 "$OUT/ctrl$i" || echo "ctrl$i 作废"
  arm "knob$i" 1 "$OUT/knob$i" || echo "knob$i 作废"
done
bash "$HERE/enable_layer.sh" off >/dev/null 2>&1
echo "全部完成, 证据: $OUT"
