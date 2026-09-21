#!/bin/bash
# build_layer.sh — NDK 直编灰档只读层 libVkLayer_refknobs.so (arm64-v8a), 零交互。
# 依赖同 refbench: brew android-commandlinetools 的 ndk;28.2.13676358。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"          # gray/
SDK="${ANDROID_SDK_ROOT:-/opt/homebrew/share/android-commandlinetools}"
NDK="${REFBENCH_NDK:-$SDK/ndk/28.2.13676358}"
HOST_TAG=darwin-x86_64
[ -d "$NDK/toolchains/llvm/prebuilt/$HOST_TAG" ] || HOST_TAG=linux-x86_64
CXX="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin/aarch64-linux-android30-clang++"
[ -x "$CXX" ] || { echo "缺 NDK clang++: $CXX" >&2; exit 1; }

OUT="$ROOT/build/out"
mkdir -p "$OUT"
"$CXX" -O2 -Wall -fPIC -std=c++17 -fno-exceptions -fno-rtti \
    -shared -o "$OUT/libVkLayer_refknobs.so" "$ROOT/layer/vk_layer_refknobs.cpp" \
    -llog
cp "$ROOT/layer/VkLayer_refknobs.json" "$OUT/"
echo "layer: $OUT/libVkLayer_refknobs.so ($(wc -c < "$OUT/libVkLayer_refknobs.so" | tr -d ' ') B)"
"$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin/llvm-nm" -D "$OUT/libVkLayer_refknobs.so" \
    | grep -E "vkNegotiateLoaderLayerInterfaceVersion|vkGetInstanceProcAddr|vkGetDeviceProcAddr" \
    | sed 's/^/  export: /'
