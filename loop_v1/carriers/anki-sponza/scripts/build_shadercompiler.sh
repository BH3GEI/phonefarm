#!/bin/bash
# build_shadercompiler.sh — 一次性的「宿主着色器编译」通道 (linux/amd64 容器内)
#
# 为什么需要它: AnKi 的 Android 构建要求先有一个**宿主平台**的 ShaderCompiler,
# 用来把 63 个 AnKi/Shaders/*.ankiprog (HLSL) 编成 *.ankiprogbin。这个工具在运行期
# dlopen DXC (AnKi/ShaderCompiler/Dxc.cpp:75), 而仓库只随包了三份 DXC 预编译库:
# WinX64 / WinArm64 / LinuxX64 —— **没有 macOS 版**, 上游 DXC 也不发布 macOS 二进制。
# 所以在这台 arm64 Mac 上没法原生跑 ShaderCompiler。
#
# 但这一步是**可解耦的**: AnKi/Shaders/CMakeLists.txt 只是把 ANKI_OVERRIDE_SHADER_COMPILER
# 当成一个命令行工具调用, 产物 *.ankiprogbin 就是普通 asset。所以只要在任意
# x86_64 Linux 上跑一次, 把 63 个 bin 缓存下来, 之后 macOS 侧的 Android 构建
# 就只剩纯 NDK 交叉编译 (不需要任何 DXC)。本脚本就是那「一次」。
#
# 用法: bash build_shadercompiler.sh [anki源码路径] [输出缓存目录]
# 产物: <缓存目录>/*.ankiprogbin  +  <缓存目录>/MANIFEST.txt (sha256 + 源码 commit)
set -euo pipefail

ANKI_SRC="${1:-/Users/mac/projects/thirdparty/anki-3d-engine}"
CACHE_DIR="${2:-/Users/mac/projects/thirdparty/anki-shaderbins}"
BUILD_DIR="${ANKI_BUILD_DIR:-/Users/mac/projects/thirdparty/anki-build-linux}"
IMAGE="anki-shadercompiler:bookworm-amd64"

[ -d "$ANKI_SRC/AnKi/Shaders" ] || { echo "找不到 AnKi 源码: $ANKI_SRC" >&2; exit 1; }
mkdir -p "$CACHE_DIR" "$BUILD_DIR"

# ── 磁盘下限守卫 ────────────────────────────────────────────────
# 本机与其它 agent 共用硬盘; 剩余空间**任何时候不得低于 8GiB**。
DISK_FLOOR_GIB="${DISK_FLOOR_GIB:-8}"
avail_gib() { df -g /System/Volumes/Data | awk 'NR==2{print $4}'; }
guard() {
  local a; a="$(avail_gib)"
  if [ "$a" -lt "$DISK_FLOOR_GIB" ]; then
    echo "!! 剩余磁盘 ${a}GiB < 下限 ${DISK_FLOOR_GIB}GiB —— 中止" >&2
    exit 9
  fi
  echo "   [disk] 剩余 ${a}GiB"
}
guard

# ── 1. 构建镜像 (只装编译 ShaderCompiler 需要的东西) ──────────────
docker build --platform linux/amd64 -t "$IMAGE" -f - "$ANKI_SRC" <<'DOCKEREOF'
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake ninja-build python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
DOCKEREOF
guard

# ── 2. 配置 + 只编 ShaderCompiler 这一个 target ────────────────────
# 注意 ANKI_SOURCE_DIRECTORY 会被编进二进制, 用于 dlopen
# <src>/ThirdParty/Dxc/Lib/LinuxX64/libdxcompiler.so, 所以容器里的挂载点
# (/src) 必须和运行时一致 —— 也因此 ShaderCompiler 只能在容器内跑。
docker run --rm --platform linux/amd64 \
  -v "$ANKI_SRC:/src" -v "$BUILD_DIR:/build" -v "$CACHE_DIR:/out" \
  -w /build "$IMAGE" bash -euxc '
    cmake /src -G Ninja \
      -DCMAKE_BUILD_TYPE=Release \
      -DANKI_BUILD_SAMPLES=OFF \
      -DANKI_BUILD_TESTS=OFF \
      -DANKI_BUILD_TOOLS=ON
    ninja ShaderCompiler
    ls -la /build/Binaries/ShaderCompiler
  '
guard

# ── 3. 跑编译: 63 个 .ankiprog → .ankiprogbin ─────────────────────
# 参数与 AnKi/Shaders/CMakeLists.txt 里 add_custom_command 的调用完全一致:
#   <compiler> -o <bin> -j N -I <ankiroot> -DANKI_PLATFORM_MOBILE=1 -spirv <prog>
docker run --rm --platform linux/amd64 \
  -v "$ANKI_SRC:/src" -v "$BUILD_DIR:/build" -v "$CACHE_DIR:/out" \
  -w /src "$IMAGE" bash -euc '
    n=0
    for p in /src/AnKi/Shaders/*.ankiprog; do
      b="/out/$(basename "$p")bin"
      /build/Binaries/ShaderCompiler -o "$b" -j 4 -I /src -DANKI_PLATFORM_MOBILE=1 -spirv "$p"
      n=$((n+1))
    done
    echo "编了 $n 个 ankiprogbin"
  '
guard

# ── 4. 记账: 产物清单 + 源码 commit, 供证据链复核 ──────────────────
{
  echo "# anki-sponza shader binary cache"
  echo "# anki commit: $(git -C "$ANKI_SRC" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "# built:       $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "# args:        -j 4 -I <src> -DANKI_PLATFORM_MOBILE=1 -spirv"
  ( cd "$CACHE_DIR" && shasum -a 256 *.ankiprogbin )
} > "$CACHE_DIR/MANIFEST.txt"

echo "缓存就绪: $CACHE_DIR ($(ls "$CACHE_DIR"/*.ankiprogbin 2>/dev/null | wc -l | tr -d ' ') 个)"
echo "构建目录 $BUILD_DIR 可以删了: rm -rf $BUILD_DIR"
