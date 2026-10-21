#!/bin/bash
# genshin_fpscap.sh — 原神 (国服) 游戏内「帧率」档位: 查 / 设 / 还原。不登录, 不进游戏菜单。
#
# 办法 (沿用 2026-10-20 NPU 实验的做法, 见 docs/experiments/2026-10-20-npu-sr/set_prefs.sh):
#   游戏停着时, 只改偏好设置文件里帧率那一项的 index 一个数字, 其余字节原样保留;
#   用 `cat 新文件 > 原文件` 写回 (保留属主和 SELinux 标签)。游戏下次启动读到新档位。
#   每次改之前把当时的整份文件拉回本地存成新文件 (不覆盖), 改完回读核对「只有这一个数字不同」。
# 还原: 把帧率 index 写回记录的原值 (同样只改这一个数字 —— 游戏运行期间自己写进去的其它字段原样保留),
#   不是拿旧文件整份盖回去。
#
# 档位 (index → 帧率): 2=60 (推断, 有旁证), 1=30 (推断)。可用 FPS_INDEX_30 / FPS_INDEX_60 改。
# 本脚本只核对文件; 实际帧率由 run_refresh_ab.sh 进大世界后用上屏帧间隔核对, 对不上就停。
#
# 注意: 偏好设置文件里有账号 UID (__LastUid__), 证据目录只放本机, 不上传。
#
# 用法:
#   bash genshin_fpscap.sh status <证据目录>
#   bash genshin_fpscap.sh set <30|60> <证据目录>      # 首次 set 时记下原 index 到 <证据目录>/fpscap_orig.txt
#   bash genshin_fpscap.sh restore <证据目录>          # 写回 fpscap_orig.txt 里的原 index
# 都会先 am force-stop 原神 (游戏在跑时它退出会把内存里的设置写回文件, 改了也白改)。
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/refresh_lib.sh"

PREFS="/data/data/$PKG/shared_prefs/$PKG.v2.playerprefs.xml"
FPS_INDEX_30="${FPS_INDEX_30:-1}"
FPS_INDEX_60="${FPS_INDEX_60:-2}"
EDIT="python3 -I $HERE/genshin_fps_prefs.py"

action="${1:-}"; shift || true

pull_prefs() { # <本地新文件>
  new_file "$1"
  if [ "$DRY_RUN" = 1 ]; then echo "[dry-run] adb exec-out su -c 'cat $PREFS' > $1" >&2; return 0; fi
  "$ADB" -s "$SERIAL" exec-out "su -c 'cat $PREFS'" > "$1" </dev/null
  [ -s "$1" ] || die "拉不到 $PREFS"
}

push_prefs() { # <本地文件>
  local dst="$DEV_TMP/o1_gsprefs_$(date +%Y%m%d-%H%M%S)_$RANDOM.xml"
  if [ "$DRY_RUN" = 1 ]; then echo "[dry-run] adb push $1 $dst; su -c 'cat $dst > $PREFS'" >&2; return 0; fi
  [ -n "$(dsh "ls $dst 2>/dev/null")" ] && die "$dst 已存在"
  "$ADB" -s "$SERIAL" push "$1" "$dst" </dev/null >/dev/null || die "push 失败"
  dsu "cat $dst > $PREFS" || die "写回 $PREFS 失败"
  dsu "ls -lZ $PREFS" >&2
}

stop_game() { dsh "am force-stop $PKG" >/dev/null; sleep 2; }

idx_of() { $EDIT get "$1" | sed -n 's/^fps_index=\([0-9]*\).*/\1/p'; }

case "$action" in
  status)
    D="${1:?缺证据目录}"; mkdir -p "$D"
    f="$D/prefs_status_$(date +%Y%m%d-%H%M%S).xml"
    pull_prefs "$f"
    [ "$DRY_RUN" = 1 ] && { echo "fps_index=?"; exit 0; }
    $EDIT get "$f"
    ;;

  set)
    CAP="${1:?缺 30|60}"; D="${2:?缺证据目录}"; mkdir -p "$D"
    case "$CAP" in 30) want="$FPS_INDEX_30";; 60) want="$FPS_INDEX_60";; *) die "只支持 30 / 60";; esac
    stop_game
    ts=$(date +%Y%m%d-%H%M%S)
    cur="$D/prefs_before_set${CAP}_$ts.xml"
    pull_prefs "$cur"
    [ "$DRY_RUN" = 1 ] && { log "dry-run: 会把帧率 index 改成 $want"; exit 0; }
    have=$(idx_of "$cur"); [ -n "$have" ] || die "偏好设置里找不到唯一的帧率项"
    if [ ! -e "$D/fpscap_orig.txt" ]; then
      echo "orig_fps_index=$have" > "$D/fpscap_orig.txt"
      echo "orig_file=$cur" >> "$D/fpscap_orig.txt"
      log "记下原帧率 index=$have"
    fi
    if [ "$have" = "$want" ]; then log "帧率 index 已是 $want ($CAP 帧), 不写"; exit 0; fi
    new="$D/prefs_set${CAP}_$ts.xml"
    $EDIT set "$cur" "$new" "$want" >/dev/null || die "改不了帧率项"
    $EDIT same-except-fps "$cur" "$new" >/dev/null || die "改出来的文件不止帧率一处不同, 不写"
    push_prefs "$new"
    back="$D/prefs_readback_set${CAP}_$ts.xml"
    pull_prefs "$back"
    cmp -s "$new" "$back" || die "回读与写入不一致"
    log "帧率 index $have → $want ($CAP 帧) 已写入并回读一致"
    ;;

  restore)
    D="${1:?缺证据目录}"
    [ -e "$D/fpscap_orig.txt" ] || { log "没有 fpscap_orig.txt: 本次没改过帧率, 不用还原"; exit 0; }
    orig=$(sed -n 's/^orig_fps_index=//p' "$D/fpscap_orig.txt")
    stop_game
    ts=$(date +%Y%m%d-%H%M%S)
    cur="$D/prefs_before_restore_$ts.xml"
    pull_prefs "$cur"
    [ "$DRY_RUN" = 1 ] && { log "dry-run: 会把帧率 index 写回 $orig"; exit 0; }
    have=$(idx_of "$cur")
    if [ "$have" = "$orig" ]; then echo "FPSCAP_RESTORED index=$orig (本来就是)"; exit 0; fi
    new="$D/prefs_restore_$ts.xml"
    $EDIT set "$cur" "$new" "$orig" >/dev/null || die "改不了帧率项"
    $EDIT same-except-fps "$cur" "$new" >/dev/null || die "还原文件不止帧率一处不同, 不写"
    push_prefs "$new"
    back="$D/prefs_readback_restore_$ts.xml"
    pull_prefs "$back"
    if [ "$(idx_of "$back")" = "$orig" ]; then echo "FPSCAP_RESTORED index=$orig"
    else echo "FPSCAP_RESTORE_FAIL 想写 $orig, 回读 $(idx_of "$back")"; exit 4; fi
    ;;

  *) sed -n '2,22p' "$0"; exit 2 ;;
esac
