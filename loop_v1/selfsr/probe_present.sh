#!/bin/sh
# probe_present.sh — 只读摸清游戏怎么出图 (自研超分/插帧第 1 步): 图形接口、送显分辨率、合成路径。
#   sh loop_v1/selfsr/probe_present.sh <serial> <输出目录> [包名]
# 前提: 游戏已在前台 (大世界)。手机上不写任何东西。
set -u
S="${1:?serial}"; OUT="${2:?outdir}"; PKG="${3:-com.miHoYo.Yuanshen}"
mkdir -p "$OUT"
a() { adb -s "$S" shell "$@" 2>&1 | tr -d '\r'; }
PID=$(a pidof "$PKG" | awk '{print $1}')
[ -n "$PID" ] || { echo "游戏没在跑: $PKG"; exit 2; }
echo "pid=$PID" > "$OUT/pid.txt"
# 图形接口: 进程里映射了哪些图形驱动库 (Vulkan ICD = vulkan.adreno.so; GLES = libGLESv2_adreno.so)
a su -c "cat /proc/$PID/maps" | awk '{print $6}' | grep -iE 'vulkan|GLES|EGL|adreno|libgsl|unity|il2cpp' | sort -u > "$OUT/gfx_libs.txt"
# 进程的渲染线程与 GPU 上下文
a su -c "ls /proc/$PID/task | while read t; do cat /proc/$PID/task/\$t/comm; done" | sort | uniq -c | sort -rn > "$OUT/threads.txt"
a su -c "cat /sys/kernel/debug/kgsl/proc/$PID/mem 2>/dev/null | head -5; ls /sys/kernel/debug/kgsl/proc/$PID/ 2>/dev/null" > "$OUT/kgsl_proc.txt"
# SurfaceFlinger: 图层列表、缓冲尺寸、HWC 合成方式
a dumpsys SurfaceFlinger > "$OUT/sf_full.txt"
grep -A40 -E 'HWC layers|Display .* HWC layers' "$OUT/sf_full.txt" | head -80 > "$OUT/sf_hwc_layers.txt"
a dumpsys SurfaceFlinger --list > "$OUT/sf_list.txt"
a dumpsys window displays | grep -E 'init=|cur=|app=|mCurrentRefreshRate|DisplayFrames' | head -10 > "$OUT/display.txt"
a dumpsys display | grep -E 'mActiveSfDisplayMode|renderFrameRate|mDisplayModeSpecs|width=.*height' | head -10 >> "$OUT/display.txt"
a dumpsys gfxinfo "$PKG" | head -40 > "$OUT/gfxinfo.txt"
# Android 游戏模式与降分辨率干预现状 (第 3 步要用)
a cmd game list-modes "$PKG" > "$OUT/game_modes.txt"
a cmd game list-configs "$PKG" >> "$OUT/game_modes.txt"
a dumpsys game >> "$OUT/game_modes.txt"
# 已有图层类调试开关 (不改, 只记)
for k in enable_gpu_debug_layers gpu_debug_app gpu_debug_layers gpu_debug_layer_app game_driver_opt_in_apps; do
  echo "global.$k=$(a settings get global $k)"
done > "$OUT/debug_layer_settings.txt"
for p in debug.vulkan.layers debug.gles.layers ro.debuggable ro.hardware.vulkan ro.hardware.egl debug.hwui.renderer; do
  echo "$p=$(a getprop $p)"
done >> "$OUT/debug_layer_settings.txt"
echo "OK $OUT"
