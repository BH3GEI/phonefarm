#!/bin/bash
# build_layer.sh — NDK 直编灰档只读层 libVkLayer_refknobs.so (arm64-v8a), 零交互。
# 依赖同 refbench: brew android-commandlinetools 的 ndk;28.2.13676358。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"          # gray/
SDK="${ANDROID_SDK_ROOT:-/opt/homebrew/share/android-commandlinetools}"
NDK="${REFBENCH_NDK:-$SDK/ndk/28.2.13676358}"
# 默认位置没有时依次找: ANDROID_NDK_HOME, Android Studio 的 SDK, 仓库外的 tools/ndk (都要求 r28c = 28.2.13676358)
if [ ! -d "$NDK" ]; then
    for c in "${ANDROID_NDK_HOME:-}" "$HOME/Library/Android/sdk/ndk/28.2.13676358" \
             "$ROOT/../../../../tools/ndk/android-ndk-r28c" "$ROOT/../../../../../tools/ndk/android-ndk-r28c"; do
        [ -n "$c" ] && [ -d "$c" ] && { NDK="$(cd "$c" && pwd)"; break; }
    done
fi
HOST_TAG=darwin-x86_64
[ -d "$NDK/toolchains/llvm/prebuilt/$HOST_TAG" ] || HOST_TAG=linux-x86_64
CXX="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin/aarch64-linux-android30-clang++"
[ -x "$CXX" ] || { echo "缺 NDK clang++: $CXX" >&2; exit 1; }

OUT="$ROOT/build/out"
mkdir -p "$OUT"
# -static-libstdc++ 是硬要求, 不是优化: 层被宿主应用的 Vulkan 加载器 dlopen, 而宿主
# 应用的 lib 目录里通常没有 libc++_shared.so。动态链 STL 会在真机上直接
#   dlopen failed: library "libc++_shared.so" not found
# (2026-09-23 refbench 实测过这个失败)。静态进去 → .so 自包含, 任意目标都能挂。
# --exclude-libs ALL 把静态进来的 STL 符号藏起来, 不污染宿主的符号表。
# copyprobe 的 1:1 拷贝算子编成 SPIR-V 并内嵌成 C 头 —— 层要自包含, 不能把 .spv
# 当运行时文件发进去 (宿主应用的沙箱里读不到我们的路径)。
GLSLC="$NDK/shader-tools/$HOST_TAG/glslc"
[ -x "$GLSLC" ] || { echo "缺 glslc: $GLSLC" >&2; exit 1; }
"$GLSLC" -O "$ROOT/layer/copyprobe_copy.comp" -o "$OUT/copyprobe_copy.spv"
"$GLSLC" -O "$ROOT/layer/upop.comp" -o "$OUT/upop.spv"
# npuop: 画面 -> NPU 输入张量 / NPU 输出张量 -> 送显分辨率 dst
"$GLSLC" -O "$ROOT/layer/npu_pack.comp" -o "$OUT/npu_pack.spv"
"$GLSLC" -O "$ROOT/layer/npu_unpack.comp" -o "$OUT/npu_unpack.spv"
python3 - "$OUT/copyprobe_copy.spv" "$OUT/upop.spv" "$OUT/npu_pack.spv" "$OUT/npu_unpack.spv" > "$OUT/copy_spv.h" <<'PYEOF'
import sys
def emit(name, path):
    d = open(path, "rb").read()
    print(f"static const unsigned char {name}[] = {{")
    for i in range(0, len(d), 16):
        print("  " + ",".join(str(b) for b in d[i:i+16]) + ",")
    print("};")
    print(f"static const unsigned int {name}Len = {len(d)};")
emit("kCopySpv", sys.argv[1])
emit("kUpopSpv", sys.argv[2])
emit("kNpuPackSpv", sys.argv[3])
emit("kNpuUnpackSpv", sys.argv[4])
PYEOF
echo "shader: copy=$(wc -c < "$OUT/copyprobe_copy.spv" | tr -d ' ')B upop=$(wc -c < "$OUT/upop.spv" | tr -d ' ')B" \
     "npu_pack=$(wc -c < "$OUT/npu_pack.spv" | tr -d ' ')B npu_unpack=$(wc -c < "$OUT/npu_unpack.spv" | tr -d ' ')B -> copy_spv.h"

"$CXX" -O2 -Wall -fPIC -std=c++17 -fno-exceptions -fno-rtti \
    -static-libstdc++ -Wl,--exclude-libs,ALL \
    -I"$OUT" -I"$ROOT/layer" \
    -shared -o "$OUT/libVkLayer_refknobs.so" "$ROOT/layer/vk_layer_refknobs.cpp" "$ROOT/layer/npuprobe.cpp" \
    -llog -ldl
cp "$ROOT/layer/VkLayer_refknobs.json" "$OUT/"
echo "layer: $OUT/libVkLayer_refknobs.so ($(wc -c < "$OUT/libVkLayer_refknobs.so" | tr -d ' ') B)"
NM="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin/llvm-nm"
"$NM" -D --defined-only "$OUT/libVkLayer_refknobs.so" | grep " T " | sed 's/^/  export: /'

# 自检 1: 必需导出符号一个都不能少。
# vkEnumerateInstance{Layer,Extension}Properties 尤其关键 —— Android 的加载器靠 dlsym
# 发现层(不读 JSON manifest), 少了它们只会在真机上报
#   E vulkan: layer library '...' missing some instance enumeration functions
# 然后整个丢弃这个层。2026-09-23 实测踩过, 所以在这里挡住。
for sym in vkNegotiateLoaderLayerInterfaceVersion vkGetInstanceProcAddr vkGetDeviceProcAddr \
           vkEnumerateInstanceLayerProperties vkEnumerateInstanceExtensionProperties; do
    "$NM" -D --defined-only "$OUT/libVkLayer_refknobs.so" | grep -q " T $sym\$" \
        || { echo "自检失败: 缺导出符号 $sym" >&2; exit 1; }
done

# 自检: 任何残留的 DT_NEEDED 动态依赖都必须是宿主进程一定已加载的系统库。
# libc++_shared 出现在这里 = 上机必炸, 当场失败而不是等真机报。
NEEDED=$("$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin/llvm-readelf" -d "$OUT/libVkLayer_refknobs.so" \
    | sed -n 's/.*NEEDED.*\[\(.*\)\]/\1/p')
echo "  needed: $(echo "$NEEDED" | tr '\n' ' ')"
if echo "$NEEDED" | grep -q "libc++_shared"; then
    echo "自检失败: 仍依赖 libc++_shared.so, 宿主应用 lib 目录里没有它, 上机会 dlopen 失败" >&2
    exit 1
fi
