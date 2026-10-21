#!/system/bin/sh
# vendor_override.sh — 用 bind mount 把厂商性能服务的一份配置换成改过的副本 (设备端, su 下跑)。
#
# 不删、不改原文件: 原文件只读复制一份留底, 改好的副本放 /data/local/tmp/o2/vendor_edit/,
# 用 bind mount 盖在原路径上; 还原 = umount, 原文件从头到尾没被写过。重启手机也自动还原
# (bind mount 不持久)。
#
#   vendor_override.sh plan    <原文件> <改好的副本> <服务>   只打印将做的事, 不改任何状态 (dry-run)
#   vendor_override.sh apply   <原文件> <改好的副本> <服务>   留底 → 贴标签 → bind mount → 重启服务 → 核对
#   vendor_override.sh status  <原文件>                       现在原路径读到的是哪份 (sha256), 有没有挂着
#   vendor_override.sh restore <原文件> <服务>                umount → 重启服务 → 核对读到的是原件
#
# <服务> 写法:
#   init:<init 服务名>   用 setprop ctl.restart 重启 (例 init:vendor.perf2-hal, 名字以 recon 的 init_rc.txt 为准)
#   pkg:<包名>           am force-stop (厂商游戏应用; 下次游戏启动时它重新读配置)
#   none                 不重启 (服务会自己定时重读时用)
# 挂载在 init (pid 1) 的 mount 命名空间里做: 厂商 HAL 跑在那个命名空间, su 的 shell 不一定是。
set -u

BASE=/data/local/tmp/o2
ORIG_DIR=$BASE/vendor_orig
LOG=$BASE/vendor_override.log

ns1() { nsenter -t 1 -m -- "$@"; }
sha() { sha256sum "$1" 2>/dev/null | cut -d' ' -f1; }
sha_ns1() { ns1 sha256sum "$1" 2>/dev/null | cut -d' ' -f1; }
flat() { echo "$1" | sed 's|^/||; s|/|%|g'; }
mounted() { grep -q " $1 " /proc/1/mountinfo 2>/dev/null; }

restart_svc() {
  case "$1" in
    init:*) n=${1#init:}; echo "setprop ctl.restart $n"; [ "${DRY:-0}" = 1 ] || setprop ctl.restart "$n" ;;
    pkg:*)  n=${1#pkg:};  echo "am force-stop $n";       [ "${DRY:-0}" = 1 ] || am force-stop "$n" ;;
    none)   echo "(不重启服务)" ;;
    *) echo "O2_REFUSE 服务写法不认识: $1"; exit 2 ;;
  esac
}

svc_pid() {
  case "$1" in
    init:*) getprop "init.svc_debug_pid.${1#init:}" ;;
    pkg:*)  pidof "${1#pkg:}" ;;
    *) echo "" ;;
  esac
}

act="${1:-}"
TG="${2:-}"
case "$act" in
  plan|apply)
    ED="${3:?缺改好的副本}"; SVC="${4:?缺服务}"
    [ -f "$TG" ] || { echo "O2_REFUSE 原文件不在: $TG"; exit 2; }
    [ -f "$ED" ] || { echo "O2_REFUSE 副本不在: $ED"; exit 2; }
    case "$ED" in $BASE/vendor_edit/*) ;; *) echo "O2_REFUSE 副本必须放在 $BASE/vendor_edit/ 下"; exit 2 ;; esac
    command -v nsenter >/dev/null 2>&1 || { echo "O2_REFUSE 没有 nsenter, 挂不进 init 命名空间"; exit 2; }
    if mounted "$TG"; then echo "O2_REFUSE $TG 上已经挂着东西 (见 status), 先 restore"; exit 2; fi
    so=$(sha "$TG"); se=$(sha "$ED")
    [ "$so" != "$se" ] || { echo "O2_REFUSE 副本与原文件内容相同, 没必要挂"; exit 2; }
    ctx=$(ls -Z "$TG" | awk '{print $1}')
    own=$(stat -c '%u:%g' "$TG"); mode=$(stat -c '%a' "$TG")
    keep="$ORIG_DIR/$(flat "$TG").${so%"${so#????????}"}"
    echo "原文件  $TG sha256=$so ctx=$ctx owner=$own mode=$mode"
    echo "副本    $ED sha256=$se"
    echo "留底    $keep (已存在则不再复制)"
    echo "挂载    nsenter -t 1 -m -- mount -o bind,ro $ED $TG"
    if [ "$act" = plan ]; then DRY=1 restart_svc "$SVC"; echo O2_PLAN_ONLY; exit 0; fi

    mkdir -p "$ORIG_DIR"
    [ -e "$keep" ] || cp -p "$TG" "$keep"
    [ "$(sha "$keep")" = "$so" ] || { echo "O2_REFUSE 留底副本 sha 对不上: $keep"; exit 2; }
    chcon "$ctx" "$ED" && chown "$own" "$ED" && chmod "$mode" "$ED" || { echo "O2_FAIL 副本贴标签/属主失败"; exit 1; }
    ns1 mount -o bind,ro "$ED" "$TG" || { echo "O2_FAIL bind mount 失败"; exit 1; }
    echo "$(date '+%F %T') apply $TG <- $ED orig_sha=$so edit_sha=$se svc=$SVC" >> "$LOG"
    restart_svc "$SVC"
    sleep 3
    got=$(sha_ns1 "$TG")
    pid=$(svc_pid "$SVC")
    gotp=""; [ -n "$pid" ] && gotp=$(nsenter -t "$pid" -m -- sha256sum "$TG" 2>/dev/null | cut -d' ' -f1)
    echo "init 命名空间读到 sha256=$got; 服务 pid=${pid:-?} 命名空间读到 sha256=${gotp:-?}"
    if [ "$got" = "$se" ] && { [ -z "$gotp" ] || [ "$gotp" = "$se" ]; }; then echo O2_OVERRIDE_APPLIED
    else echo "O2_OVERRIDE_UNSURE 服务读到的未必是副本, 别开测"; exit 1; fi
    ;;
  status)
    [ -n "$TG" ] || { echo "用法: $0 status <原文件>"; exit 2; }
    echo "sha256(init 命名空间)=$(sha_ns1 "$TG")"
    if mounted "$TG"; then echo "O2_MOUNTED"; grep " $TG " /proc/1/mountinfo; else echo "O2_NOT_MOUNTED"; fi
    ls "$ORIG_DIR" 2>/dev/null | grep "^$(flat "$TG")\." | sed "s|^|留底: $ORIG_DIR/|"
    ;;
  restore)
    SVC="${3:?缺服务}"
    keep=$(ls "$ORIG_DIR" 2>/dev/null | grep "^$(flat "$TG")\." | head -1)
    if mounted "$TG"; then
      ns1 umount "$TG" || { echo "O2_RESTORE_FAIL umount 失败"; exit 1; }
    fi
    mounted "$TG" && { echo "O2_RESTORE_FAIL 还挂着 (挂了不止一层?)"; exit 1; }
    restart_svc "$SVC"
    sleep 3
    got=$(sha_ns1 "$TG")
    echo "$(date '+%F %T') restore $TG sha_now=$got" >> "$LOG"
    if [ -n "$keep" ] && [ "$got" = "$(sha "$ORIG_DIR/$keep")" ]; then echo "O2_RESTORE_OK 读到的是原件 ($got)"
    elif [ -z "$keep" ]; then echo "O2_RESTORE_OK 未挂过 (没有留底记录); 现在 sha256=$got"
    else echo "O2_RESTORE_FAIL 现在读到 $got, 留底是 $(sha "$ORIG_DIR/$keep")"; exit 1; fi
    ;;
  *) echo "用法: $0 plan|apply <原文件> <副本> <服务> | status <原文件> | restore <原文件> <服务>"; exit 2 ;;
esac
