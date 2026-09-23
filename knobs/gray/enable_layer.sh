#!/bin/bash
# enable_layer.sh {probe|target <pkg>|status|off} — 挂/摘灰档只读层 (FEASIBILITY 问题 1/2)
#
#   probe            对 refbench 挂只读层并启动, 验"机制能挂上非 debuggable 应用" (问题 1)
#   loadop [pkg]     同上, 但打开 LoadOp LOAD->DONT_CARE 改写 (判据 5)
#   passdump [pkg]   只读 + 导出 render pass 形状表: 每个 pass 的宽高/格式/次序/draw 数,
#                    外加 swapchain 尺寸与一帧的有序 pass 序列。用来定位"低分辨率->放大"那一步。
#   target <pkg>     对目标游戏挂空层并启动, 看能否正常进游戏 (问题 2 反作弊)
#   --keep-prop      挂完保留全局属性 (默认是层一加载就清掉, 见下面的误伤警告)
#   status           只读打印当前挂载状态
#   off              摘层并删掉推进去的 .so, 还原
#
# ── 机制 (2026-09-23 在 NX809J / Android 16 / user 版 + Magisk root 上逐条实测) ──
#
# 生效只需要两件事, 缺一不可:
#   1. libVkLayer_refknobs.so 放进**目标应用自己的 nativeLibraryDir**
#      —— 加载器对每个应用都默认搜这个目录 (实测: 无关的系统应用 cn.nubia.gamelab
#         启动时也在搜它自己的 lib 目录), 所以不需要应用 debuggable
#   2. `setprop debug.vulkan.layers <层名>`
#
# 实测**不需要**的东西 (README 早先写的那套, 在这台 ROM 上一条都不生效):
#   - settings put global enable_gpu_debug_layers / gpu_debug_app /
#     gpu_debug_layers / gpu_debug_layer_app
#     → 框架侧 GraphicsEnvironment 因为应用非 debuggable 且 ro.debuggable=0 直接跳过,
#       logcat 里连 "GPU debug layers enabled" 都不打。全程无效。
#   - ro.debuggable=1 (resetprop 改过再改回, 对本机制无影响)
#   四格对照矩阵见 evidence/01_probe_refbench/matrix.txt
#
# ⚠⚠ debug.vulkan.layers 是**全局**属性, 而且会误伤别的应用 —— 这是实测到的, 不是理论风险。
#
#   属性置上之后启动的每个 Vulkan 应用, 加载器都会去找这个层名; 在自己 lib 目录里
#   找不到 .so 的应用, vkCreateInstance 直接失败。用 Vulkan 跑 HWUI 的应用会当场 abort。
#   2026-09-23 实测: 挂层期间厂商应用 cn.nubia.gameassist (此前已稳定运行 63 小时)
#   RenderThread 报 `HWUI: Assertion failed: err < 0` → SIGABRT, 之后每 ~2 秒崩溃重启一次,
#   直到属性被清掉。证据 evidence/02_target_genshin/logcat_genshin.fatal.txt。
#
#   所以本脚本默认**尽快收窄暴露窗口**: target/probe 挂完会等目标进程真的加载上层,
#   然后立刻把属性清掉 —— 层已经在目标进程里了, 清属性不影响它, 但新起的应用不再受害。
#   用 --keep-prop 可以保留属性 (例如你要手动重启目标应用), 但那期间别的应用可能会崩。
#   无论如何, 用完都要跑 off。
#
# 证据双通道: 应用外部 files 目录的 knobs_layer_out.json;
#   以及 `adb shell su -c 'logcat -b main' | grep refknobs`
#   (本机 logcat 只是对 shell uid 不可读, root 读一切正常 —— 立项时记的"logcat 哑掉"不准确)
set -uo pipefail

SERIAL="${REFBENCH_SERIAL:-91253241019A}"
LAYER_NAME="VK_LAYER_refknobs_readonly"
LOADOP_PROP="debug.knobs.loadop"          # 1 = 层把 loadOp LOAD 改写成 DONT_CARE
DUMP_PROP="debug.knobs.passdump"          # 1 = 层导出 render pass 形状表 (找放大那一步)
CP_PROP="debug.knobs.copyprobe"           # 1 = 1:1 拷贝探针 (改写! 探反作弊用, 隐含 passdump 追踪)
UPOP_PROP="debug.knobs.upop"              # 1 = 真超分算子 (gen1_loc3), dst=送显分辨率
SO="libVkLayer_refknobs.so"
STATE="/data/local/tmp/knobs_layer_state"       # 存"把 .so 推进了哪个包", 供 off 精确回滚
OUT="$(cd "$(dirname "$0")" && pwd)/build/out"
ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }

# 解析一个包的 nativeLibraryDir —— 加载器真正会搜的那个目录
native_lib_dir() {
  local pkg="$1" legacy
  legacy=$(ashell "dumpsys package $pkg | grep -m1 legacyNativeLibraryDir" \
           | sed 's/.*legacyNativeLibraryDir=//' | tr -d '\r')
  [ -n "$legacy" ] || return 1
  # 加载器搜的是 <legacyNativeLibraryDir>/<abi 子目录>。没有 arm64 子目录就说明这个包
  # 没有 64 位原生库 —— 直接退回父目录只会得到一个加载器根本不搜的路径, 然后"挂不上"
  # 却查不出原因。这里如实失败。
  if ashell "su -c 'test -d \"$legacy/arm64\"'" >/dev/null 2>&1; then
    echo "$legacy/arm64"
  else
    echo "$pkg 没有 arm64 原生库目录 ($legacy/arm64 不存在); 本机制要求目标是 arm64-v8a 应用" >&2
    return 1
  fi
}

mount_layer() {
  local pkg="$1" keep_prop="$2" dir
  [ -f "$OUT/$SO" ] || { echo "先跑 build_layer.sh 出 $SO" >&2; exit 1; }
  dir=$(native_lib_dir "$pkg") || { echo "解析不到 $pkg 的 nativeLibraryDir" >&2; exit 1; }
  adb -s "$SERIAL" push "$OUT/$SO" "/data/local/tmp/$SO" >/dev/null \
    || { echo "push 失败" >&2; exit 1; }
  # 属主与 SELinux 上下文对齐同目录既有 .so, 否则应用读不到。
  # 每一步都要回读核验 —— 静默失败会让后面"没挂上"的原因完全查不出来。
  ashell "su -c 'cp /data/local/tmp/$SO \"$dir/\" && chmod 755 \"$dir/$SO\" \
          && chown system:system \"$dir/$SO\" && chcon u:object_r:apk_data_file:s0 \"$dir/$SO\"'"
  ashell "su -c 'test -f \"$dir/$SO\"'" >/dev/null 2>&1 \
    || { echo "安装 .so 失败: $dir/$SO 不存在" >&2; exit 1; }
  ashell "su -c 'echo \"$pkg|$dir\" > $STATE'"
  [ -n "$(ashell "su -c 'cat $STATE 2>/dev/null'" | tr -d '\r')" ] \
    || { echo "写 state 失败, off 将无法精确回滚" >&2; exit 1; }

  # 目标必须重启才会走到加载器; 已在跑的进程不会凭空挂上层
  ashell "am force-stop $pkg"
  # 旧 marker 必须清掉, 否则分不清是这次挂上的还是上次留下的
  ashell "su -c 'rm -f /storage/emulated/0/Android/data/$pkg/files/knobs_layer_out.json \
          /data/data/$pkg/knobs_layer_out.json /data/local/tmp/knobs_layer_out.$pkg.json'"

  ashell "su -c 'setprop debug.vulkan.layers $LAYER_NAME'"
  [ "$(ashell 'getprop debug.vulkan.layers' | tr -d '\r')" = "$LAYER_NAME" ] \
    || { echo "setprop debug.vulkan.layers 没生效" >&2; exit 1; }

  ashell "su -c 'ls -laZ \"$dir/$SO\"'"
  echo "已挂层于 $pkg"
  echo "  .so:  $dir/$SO"
  echo "  prop: debug.vulkan.layers = $LAYER_NAME"
  if [ "$keep_prop" = "keep" ]; then
    echo "  ⚠ --keep-prop: 属性会一直留着。这期间启动的、lib 目录里没有本 .so 的"
    echo "     Vulkan 应用可能崩溃重启 (实测 cn.nubia.gameassist 会)。用完务必跑 off。"
  else
    echo "  正在启动 $pkg; 一旦检测到层加载成功就自动清掉全局属性, 收窄误伤窗口。"
    launch_pkg "$pkg"
    wait_and_narrow "$pkg"
  fi
  echo "查证据:"
  echo "  adb -s $SERIAL shell su -c 'cat /storage/emulated/0/Android/data/$pkg/files/knobs_layer_out.json'"
  echo "  adb -s $SERIAL shell su -c 'logcat -b main -d' | grep refknobs"
}

# 启动目标应用。
#
# 用 `am start -n <解析出的 activity>`, **不要用 monkey**。2026-09-23 无层对照实测:
#   am start -n  → frames_submitted=3600, clean_exit=true
#   monkey       → frames_submitted=1318, clean_exit=false
# monkey 连一个 layer 都没挂就能把 refbench 的运行搅坏 (它会注入事件), 用它启动等于
# 给每次测量掺进一个与被测对象无关的扰动源。
launch_pkg() {
  local pkg="$1" act
  act=$(ashell "cmd package resolve-activity --brief $pkg" | tail -1 | tr -d '\r')
  case "$act" in
    */*) ashell "am start -n $act" >/dev/null 2>&1 ;;
    *)   echo "  解析不到 $pkg 的启动 activity, 请手动启动它" >&2; return 1 ;;
  esac
}

# 等目标进程真的把层加载进去, 然后立刻清掉全局属性。
# 层已经在目标进程里了, 清属性不会把它卸下来; 但新起的应用不会再去找这个层。
wait_and_narrow() {
  local pkg="$1" i marker
  for i in $(seq 1 40); do
    marker=$(ashell "su -c 'cat /storage/emulated/0/Android/data/$pkg/files/knobs_layer_out.json 2>/dev/null; \
             cat /data/data/$pkg/knobs_layer_out.json 2>/dev/null'" | tr -d '\r')
    if [ -n "$marker" ]; then
      ashell "su -c 'setprop debug.vulkan.layers \"\"'"
      echo "  层已加载, 全局属性已清 (属性暴露窗口约 $((i*2)) 秒)"
      echo "  marker: $marker"
      return 0
    fi
    sleep 2
  done
  ashell "su -c 'setprop debug.vulkan.layers \"\"'"
  echo "  等不到 $pkg 的 marker; 已保险起见清掉全局属性。"
  echo "  排查: adb -s $SERIAL shell su -c 'logcat -b main -d' | grep -iE 'vulkan|refknobs'"
  return 1
}

# --keep-prop 出现在任意位置都算; 用数组重建位置参数, 不靠 $() 的词分割
KEEP=narrow
ARGS=()
for a in "$@"; do
  if [ "$a" = "--keep-prop" ]; then KEEP=keep; else ARGS+=("$a"); fi
done
set -- ${ARGS[@]+"${ARGS[@]}"}

case "${1:-}" in
  probe)   ashell "su -c 'setprop $LOADOP_PROP 0; setprop $DUMP_PROP 0; setprop $CP_PROP 0; setprop $UPOP_PROP 0'"; mount_layer "${2:-io.github.hgamey.refbench}" "$KEEP" ;;
  # 观测档: 只读 + 导出 pass 形状表 (尺寸/格式/次序/draw 数), 用来定位"低分辨率->放大"那一步
  passdump)
    ashell "su -c 'setprop $LOADOP_PROP 0; setprop $DUMP_PROP 1; setprop $CP_PROP 0; setprop $UPOP_PROP 0'"
    [ "$(ashell "getprop $DUMP_PROP" | tr -d '\r')" = "1" ] \
      || { echo "setprop $DUMP_PROP 没生效" >&2; exit 1; }
    mount_layer "${2:-com.miHoYo.Yuanshen}" "$KEEP" ;;
  # 改写档: 与 probe 唯一的差别就是这个属性, A/B 两臂只切它一个
  loadop)
    # 必须显式把 passdump 关掉: 上一轮 passdump 留下的 1 会让 A/B 两臂都挂上观测钩子,
    # 等于给对照组凭空加开销, 而自报里看不出来
    ashell "su -c 'setprop $LOADOP_PROP 1; setprop $DUMP_PROP 0; setprop $CP_PROP 0; setprop $UPOP_PROP 0'"
    # 回读: 写不进就静默退化成只读档, 而自报里 knob 会变成 gray_readonly_probe、
    # unavailable_reason 还是 null, harness 根本看不出这一轮没开改写
    [ "$(ashell "getprop $LOADOP_PROP" | tr -d '\r')" = "1" ] \
      || { echo "setprop $LOADOP_PROP 没生效, 拒绝按改写档继续" >&2; exit 1; }
    mount_layer "${2:-io.github.hgamey.refbench}" "$KEEP" ;;
  target)
    [ -n "${2:-}" ] || { echo "target 要给包名" >&2; exit 2; }
    ashell "su -c 'setprop $LOADOP_PROP 0; setprop $DUMP_PROP 0; setprop $CP_PROP 0; setprop $UPOP_PROP 0'"; mount_layer "$2" "$KEEP" ;;
  # 拷贝探针档: 1:1 拷贝算子把"建 pipeline/建 image/换描述符"全走一遍, 画面应逐像素不变。
  # 是**改写**, 不是只读 —— 探的就是反作弊对这种侵入放不放行。
  copyprobe)
    ashell "su -c 'setprop $LOADOP_PROP 0; setprop $DUMP_PROP 1; setprop $CP_PROP 1; setprop $UPOP_PROP 0'"
    [ "$(ashell "getprop $CP_PROP" | tr -d '\r')" = "1" ] \
      || { echo "setprop $CP_PROP 没生效, 拒绝按探针档继续" >&2; exit 1; }
    mount_layer "${2:-com.miHoYo.Yuanshen}" "$KEEP" ;;
  # 真超分算子档: gen1_loc3 (2x2 bilinear), dst=送显分辨率
  upop)
    ashell "su -c 'setprop $LOADOP_PROP 0; setprop $DUMP_PROP 1; setprop $CP_PROP 0; setprop $UPOP_PROP 1'"
    [ "$(ashell "getprop $UPOP_PROP" | tr -d '\r')" = "1" ] \
      || { echo "setprop $UPOP_PROP 没生效, 拒绝按算子档继续" >&2; exit 1; }
    mount_layer "${2:-com.miHoYo.Yuanshen}" "$KEEP" ;;
  status)
    echo "debug.vulkan.layers = [$(ashell 'getprop debug.vulkan.layers' | tr -d '\r')]"
    echo "$LOADOP_PROP  = [$(ashell "getprop $LOADOP_PROP" | tr -d '\r')]"
    echo "$DUMP_PROP  = [$(ashell "getprop $DUMP_PROP" | tr -d '\r')]"
    echo "$CP_PROP = [$(ashell "getprop $CP_PROP" | tr -d '\r')]"
    echo "$UPOP_PROP = [$(ashell "getprop $UPOP_PROP" | tr -d '\r')]"
    st=$(ashell "su -c 'cat $STATE 2>/dev/null'" | tr -d '\r')
    if [ -n "$st" ]; then
      echo "state = $st  (处于施加态)"
      ashell "su -c 'ls -laZ \"${st#*|}/$SO\" 2>&1'"
    else
      echo "state = <无>  (未施加)"
    fi ;;
  off)
    ashell "su -c 'setprop debug.vulkan.layers \"\"; setprop $LOADOP_PROP 0; setprop $DUMP_PROP 0; setprop $CP_PROP 0; setprop $UPOP_PROP 0'"
    st=$(ashell "su -c 'cat $STATE 2>/dev/null'" | tr -d '\r')
    if [ -n "$st" ]; then
      dir="${st#*|}"; pkg="${st%%|*}"
      # marker 里没有 run id, 留着的话下次 probe 分不清新旧, 一并清掉
      ashell "su -c 'rm -f /storage/emulated/0/Android/data/$pkg/files/knobs_layer_out.json \
              /data/data/$pkg/knobs_layer_out.json /data/local/tmp/knobs_layer_out.$pkg.json'"
      ashell "su -c 'rm -f \"$dir/$SO\"'"
      ashell "su -c 'test -e \"$dir/$SO\"'" >/dev/null 2>&1 \
        && echo "警告: $dir/$SO 没删掉" || echo "已删除 $dir/$SO"
    fi
    # 兜底扫一遍: state 文件只记得最后一次挂在哪, 手动推过 / 上一次异常退出留下的
    # 副本它不知道。.so 文件名是我们独有的, 按名字全盘清理不会误伤。
    stray=$(ashell "su -c 'find /data/app /data/data /data/local/tmp -name \"$SO\" 2>/dev/null'" | tr -d '\r')
    if [ -n "$stray" ]; then
      echo "$stray" | while read -r f; do
        [ -n "$f" ] && ashell "su -c 'rm -f \"$f\"'" && echo "已清理残留 $f"
      done
    fi
    ashell "su -c 'rm -f $STATE /data/local/tmp/$SO'"
    # 立项时那套 settings 本来就不生效, 但早先版本写过, 一并清干净
    ashell "settings delete global gpu_debug_layers" >/dev/null 2>&1
    ashell "settings delete global gpu_debug_layer_app" >/dev/null 2>&1
    ashell "settings delete global gpu_debug_app" >/dev/null 2>&1
    ashell "settings delete global enable_gpu_debug_layers" >/dev/null 2>&1
    echo "已摘层。回读:"
    echo "  debug.vulkan.layers = [$(ashell 'getprop debug.vulkan.layers' | tr -d '\r')]"
    echo "  state               = [$(ashell "su -c 'cat $STATE 2>/dev/null'" | tr -d '\r')]"
    for K in enable_gpu_debug_layers gpu_debug_app gpu_debug_layer_app gpu_debug_layers; do
      printf '  %-20s = %s\n' "$K" "$(ashell "settings get global $K" | tr -d '\r')"
    done ;;
  *)
    echo "用法: enable_layer.sh {probe|loadop [pkg]|passdump [pkg]|target <pkg>|status|off} [--keep-prop]" >&2
    exit 2 ;;
esac
