#!/bin/bash
# enable_layer.sh {probe|target|off} [pkg] — 挂/摘灰档只读层 (FEASIBILITY 问题 1/2)
#
#   probe            对 refbench 挂层, 验"机制能挂上非 debuggable 应用" (问题 1)
#   target <pkg>     对目标游戏挂空层, 看能否正常进游戏 (问题 2 反作弊, 几分钟出结果)
#   off              摘掉所有 gpu_debug 设置, 还原
#
# 层 .so 供给: root 直接把 libVkLayer_refknobs.so 推进"debug_layer_app"能被加载器搜到的
# 位置。Android 的 gpu_debug 机制在不同版本对 .so 命名/搜索路径有分歧, 这里编码最常见的
# 一种并把另一种写进注释 —— 哪种真生效正是问题 1 要验的, 不假装已知。
set -uo pipefail

SERIAL="${REFBENCH_SERIAL:-91253241019A}"
LAYER_NAME="VK_LAYER_refknobs_readonly"
SO="libVkLayer_refknobs.so"
OUT="$(cd "$(dirname "$0")" && pwd)/build/out"
ashell() { adb -s "$SERIAL" shell "$@" </dev/null; }

push_so() { # 把 .so 放到目标应用私有 lib 目录 (root)
  local pkg="$1"
  [ -f "$OUT/$SO" ] || { echo "先 build_layer.sh 出 $SO"; exit 1; }
  adb -s "$SERIAL" push "$OUT/$SO" /data/local/tmp/$SO >/dev/null
  # 路径 A (本脚本采用): 塞进目标应用 code_cache, 用 gpu_debug_layer_app 指向该应用自己
  ashell "su -c 'run-as $pkg cp /data/local/tmp/$SO ./ 2>/dev/null' || \
          su -c 'cp /data/local/tmp/$SO /data/data/$pkg/ && chmod 755 /data/data/$pkg/$SO'"
  # 路径 B (备选, 若 A 加载器搜不到): 放进 /data/local/debug/vulkan/, 见 FEASIBILITY
}

case "${1:-}" in
  probe|target)
    PKG="${2:-io.github.hgamey.refbench}"
    push_so "$PKG"
    ashell "settings put global enable_gpu_debug_layers 1"
    ashell "settings put global gpu_debug_app $PKG"
    ashell "settings put global gpu_debug_layer_app $PKG"
    ashell "settings put global gpu_debug_layers $LAYER_NAME"
    echo "已挂层于 $PKG。启动它, 然后查:"
    echo "  adb -s $SERIAL shell run-as $PKG cat files/knobs_layer_out.json  (或 root cat 外部 files 目录)"
    echo "  layer_loaded=true → 问题1 机制成立; 游戏能进大世界 → 问题2 反作弊放行"
    ;;
  off)
    ashell "settings delete global gpu_debug_layers"
    ashell "settings delete global gpu_debug_layer_app"
    ashell "settings delete global gpu_debug_app"
    ashell "settings put global enable_gpu_debug_layers 0"
    echo "已摘层。"
    ;;
  *)
    echo "用法: enable_layer.sh {probe|target <pkg>|off}" >&2
    exit 2
    ;;
esac
