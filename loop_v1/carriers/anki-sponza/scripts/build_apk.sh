#!/bin/bash
# build_apk.sh — macOS 侧出 arm64 Sponza APK。
#
# 前置: scripts/build_shadercompiler.sh 已经跑过一次, 着色器 bin 缓存就绪。
#       (为什么要分两步 —— 见 shadercompiler_shim.py 顶部和 ../README.md)
#
# 这一步**不需要 DXC**: 纯 NDK 交叉编译 + gradle 打包, 着色器由 shim 从缓存发货。
set -euo pipefail

ANKI_SRC="${ANKI_SRC:-/Users/mac/projects/thirdparty/anki-3d-engine}"
CACHE_DIR="${ANKI_SHADERBIN_CACHE:-/Users/mac/projects/thirdparty/anki-shaderbins}"
HERE="$(cd "$(dirname "$0")" && pwd)"
SHIM="$HERE/shadercompiler_shim.py"

SDK="${ANDROID_SDK_ROOT:-/opt/homebrew/share/android-commandlinetools}"
NDK_VER="${ANKI_NDK_VER:-28.2.13676358}"
JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home}"
export JAVA_HOME ANDROID_SDK_ROOT="$SDK"
export PATH="$JAVA_HOME/bin:$PATH"
export ANKI_SHADERBIN_CACHE="$CACHE_DIR"

DISK_FLOOR_GIB="${DISK_FLOOR_GIB:-8}"
avail_gib() { df -g /System/Volumes/Data | awk 'NR==2{print $4}'; }
guard() {
  local a; a="$(avail_gib)"
  [ "$a" -lt "$DISK_FLOOR_GIB" ] && { echo "!! 剩余磁盘 ${a}GiB < 下限 ${DISK_FLOOR_GIB}GiB —— 中止" >&2; exit 9; }
  echo "   [disk] 剩余 ${a}GiB"
}

# ── 0. 前置检查 ───────────────────────────────────────────────
[ -d "$ANKI_SRC/AnKi" ] || { echo "找不到 AnKi 源码: $ANKI_SRC" >&2; exit 1; }
n_bin=$(ls "$CACHE_DIR"/*.ankiprogbin 2>/dev/null | wc -l | tr -d ' ')
n_prog=$(ls "$ANKI_SRC"/AnKi/Shaders/*.ankiprog 2>/dev/null | wc -l | tr -d ' ')
if [ "$n_bin" != "$n_prog" ] || [ "$n_bin" = "0" ]; then
  echo "着色器缓存不完整: 有 $n_bin 个 bin, 需要 $n_prog 个" >&2
  echo "先跑: bash $HERE/build_shadercompiler.sh" >&2
  exit 1
fi
[ -d "$SDK/ndk/$NDK_VER" ] || { echo "缺 NDK: $SDK/ndk/$NDK_VER" >&2; exit 1; }
command -v gradle >/dev/null || { echo "缺 gradle —— brew install gradle" >&2; exit 1; }
[ -x "$JAVA_HOME/bin/java" ] || { echo "缺 JDK: $JAVA_HOME —— brew install openjdk@21" >&2; exit 1; }
guard

# ── 1. 生成 AndroidProject_Sponza ────────────────────────────
# 上游脚本会把模板拷成 <anki根>/AndroidProject_Sponza, 并把 shader compiler 路径
# 写进 app/build.gradle 的 ANKI_OVERRIDE_SHADER_COMPILER —— 我们指向 shim。
PROJ="$ANKI_SRC/AndroidProject_Sponza"
if [ ! -d "$PROJ" ]; then
  ( cd "$ANKI_SRC" && python3 ./Tools/Android/GenerateAndroidProject.py \
      -o . -t Sponza -a ./Samples/Sponza/Assets/ --shader-compiler "$SHIM" )
else
  echo "AndroidProject_Sponza 已存在, 跳过生成"
fi
guard

# ── 2. 把模板里过时的版本号顶上去 ────────────────────────────
# 上游模板写死 compileSdk 32 / ndkVersion 26.1.10909125; 本机 brew 的
# android-commandlinetools 只有 platforms/android-35 和 ndk/28.2.x。
# 这两处不改, gradle 会去下载(离线/限速环境下就卡死), 所以就地改成本机已有的。
GRADLE_FILE="$PROJ/app/build.gradle"
/usr/bin/sed -i '' \
  -e "s/compileSdk 32/compileSdk 35/" \
  -e "s/targetSdk 32/targetSdk 35/" \
  -e "s/ndkVersion \"26.1.10909125\"/ndkVersion \"$NDK_VER\"/" \
  "$GRADLE_FILE"
grep -E "compileSdk|targetSdk|ndkVersion" "$GRADLE_FILE"

# shim 得可执行, 而且 gradle 是直接 exec 它的 —— 要有 shebang
chmod +x "$SHIM"

# ── 3. gradle 出包 ───────────────────────────────────────────
( cd "$PROJ" && gradle --no-daemon assembleRelease )
guard

APK="$PROJ/app/build/outputs/apk/release/app-release.apk"
[ -f "$APK" ] || APK="$(find "$PROJ/app/build/outputs/apk" -name '*.apk' | head -1)"
echo "APK: $APK ($(wc -c < "$APK" | tr -d ' ') B)"
echo "装机: adb -s \${SERIAL} install -r $APK"
