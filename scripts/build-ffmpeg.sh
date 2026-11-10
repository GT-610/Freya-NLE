#!/usr/bin/env bash
# Build the FFmpeg the application links against.
#
# Every release used to take whatever FFmpeg the host package manager had. That
# made two builds of one commit differ: the Arch archive linked avcodec 63
# because Arch ships FFmpeg 9, the Windows archive linked avcodec 63 through
# vcpkg's overlay, and the macOS archive linked the Homebrew bottle that happened
# to be installed -- whose own dylibs were built for whatever macOS the bottle
# targeted. Pinning the source and the configure line is what makes the runtime
# floor a property of this repository instead of of the build host.
#
# Usage:
#   scripts/build-ffmpeg.sh --target <triple> [--prefix <dir>] [--arch <arch>]
#
#   --target   Rust target triple: x86_64-unknown-linux-gnu,
#              aarch64-apple-darwin, x86_64-apple-darwin
#   --prefix   Install prefix (default: build/ffmpeg/<triple>)
#   --jobs     Parallel make jobs (default: number of CPUs)
#
# The result is a dynamic FFmpeg (`libav*.so`/`.dylib`), which is what
# `ffmpeg-sys-the-third` links unless its `static` feature is enabled. Dynamic is
# deliberate: a user can swap in their own build, and the packaging step that
# already rewrites install names keeps working.
set -euo pipefail

FFMPEG_TAG="n9.0.2"
# The tarball GitHub generates for a tag is not guaranteed to stay byte
# identical, so the commit is the identity and the tag is only a human label.
FFMPEG_COMMIT="946fcce07b6dcd0331c8cc609192aeff5e1924f8"
FFMPEG_REPO="https://github.com/FFmpeg/FFmpeg.git"

X264_COMMIT="0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee"
X264_REPO="https://code.videolan.org/videolan/x264.git"

TARGET=""
PREFIX=""
JOBS=""
X264_PREFIX=""

usage() {
    sed -n '11,22p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --target) TARGET="${2:?--target needs a value}"; shift 2 ;;
        --prefix) PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
        --jobs)   JOBS="${2:?--jobs needs a value}"; shift 2 ;;
        -h|--help) usage 0 ;;
        *) echo "unknown argument: $1" >&2; usage 1 ;;
    esac
done

[ -n "$TARGET" ] || { echo "--target is required" >&2; usage 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${WORK_DIR:-$ROOT/.build_tmp/ffmpeg}"
SRC="$WORK/src/$TARGET"
PREFIX="${PREFIX:-$ROOT/build/ffmpeg/$TARGET}"
X264_PREFIX="$WORK/install/$TARGET/x264"
JOBS="${JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"

log() { printf '>>> %s\n' "$*"; }

# A build directory that already holds a finished install is reused; the CI
# cache relies on that to skip the twenty minutes this takes.
if [ -f "$PREFIX/lib/pkgconfig/libavcodec.pc" ] && [ "${FORCE_REBUILD:-0}" != "1" ]; then
    log "Reusing the FFmpeg install in $PREFIX"
    echo "FFMPEG_DIR=$PREFIX"
    exit 0
fi

case "$TARGET" in
    x86_64-unknown-linux-gnu)
        FF_OS="linux"
        FF_ARCH_FLAGS=(--arch=x86_64)
        FF_EXTRA=(--enable-pthreads --enable-pic)
        ;;
    aarch64-apple-darwin|x86_64-apple-darwin)
        FF_OS="darwin"
        if [ "$TARGET" = "aarch64-apple-darwin" ]; then
            FF_ARCH_FLAGS=(--arch=arm64 --cpu=armv8-a --extra-cflags=-arch\ arm64 --extra-ldflags=-arch\ arm64)
        else
            # Cross-compiling Intel on an arm64 runner: clang takes -arch, so no
            # separate cctools toolchain is needed, only the x86_64 nasm.
            FF_ARCH_FLAGS=(--arch=x86_64 --cpu=x86-64 --extra-cflags=-arch\ x86_64 --extra-ldflags=-arch\ x86_64)
        fi
        FF_EXTRA=(
            --enable-pthreads
            --enable-videotoolbox --enable-audiotoolbox
            --enable-avfoundation --enable-videotoolbox --enable-appkit
            --enable-coreimage --enable-audiotoolbox
            --install-name-dir=@rpath
        )
        ;;
    *)
        echo "unsupported --target: $TARGET" >&2
        exit 1
        ;;
esac

# Every macOS binary must agree on one deployment target, or the bundle's
# Info.plist is a fiction. FFmpeg is the largest such binary.
if [ "$FF_OS" = "darwin" ]; then
    export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
    FF_EXTRA+=(--extra-cflags=-mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET)
    FF_EXTRA+=(--extra-ldflags=-mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET)
fi

mkdir -p "$WORK/src" "$WORK/install/$TARGET" "$PREFIX"

# x86 assembly reaches FFmpeg and x264 through nasm, and both fall back to C
# kernels without it -- a build that silently loses every SIMD path is worse than
# a build that stops and says what to install, because the difference only shows
# up as a much slower encoder after release.
if [ "$FF_OS" = "darwin" ] || [ "$TARGET" = "x86_64-unknown-linux-gnu" ]; then
    if ! command -v nasm >/dev/null 2>&1; then
        echo "nasm is required to build FFmpeg for $TARGET (install nasm)" >&2
        exit 1
    fi
fi

fetch_at() {
    local repo="$1" commit="$2" destination="$3"
    if [ -d "$destination/.git" ] && [ "$(git -C "$destination" rev-parse HEAD 2>/dev/null)" = "$commit" ]; then
        return 0
    fi
    rm -rf "$destination"
    log "Fetching $repo@$commit"
    git init -q "$destination"
    git -C "$destination" remote add origin "$repo"
    # A shallow fetch of one commit: the history is not needed to build a tag.
    # Losing the network mid-fetch is common enough on a slow route that one
    # attempt is not a verdict.
    local attempt=1 status=1
    while [ "$attempt" -le 4 ]; do
        if git -C "$destination" fetch --depth 1 origin "$commit"; then
            status=0
            break
        fi
        log "Fetch attempt $attempt failed; retrying"
        attempt=$((attempt + 1))
        sleep $((attempt * 2))
    done
    if [ "$status" -ne 0 ]; then
        echo "could not fetch $repo@$commit after 4 attempts" >&2
        echo "If this host needs a proxy, export https_proxy/HTTPS_PROXY first." >&2
        exit 1
    fi
    git -C "$destination" checkout -q FETCH_HEAD
    local actual
    actual="$(git -C "$destination" rev-parse HEAD)"
    if [ "$actual" != "$commit" ]; then
        echo "expected $commit but checked out $actual" >&2
        exit 1
    fi
}

# -- x264: the only external encoder the release needs ---------------------
if [ ! -f "$X264_PREFIX/lib/libx264.a" ] && [ ! -f "$X264_PREFIX/lib/libx264.dylib" ]; then
    fetch_at "$X264_REPO" "$X264_COMMIT" "$SRC/x264"
    log "Building x264 for $TARGET"
    pushd "$SRC/x264" >/dev/null
    case "$FF_OS" in
        darwin)
            X264_FLAGS=(--enable-static --enable-pic)
            if [ "$TARGET" = "x86_64-apple-darwin" ]; then
                X264_FLAGS+=(--host=x86_64-apple-darwin)
            else
                X264_FLAGS+=(--host=aarch64-apple-darwin)
            fi
            # x264's configure drives nasm for the assembly kernels. clang spells
            # the arm64 architecture `arm64`, not the triple's `aarch64`.
            if [ "$TARGET" = "x86_64-apple-darwin" ]; then
                X264_ARCH="x86_64"
            else
                X264_ARCH="arm64"
            fi
            X264_FLAGS+=(--extra-cflags="-arch $X264_ARCH -mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET")
            ;;
        *)
            X264_FLAGS=(--enable-static --enable-pic --host=x86_64-linux-gnu)
            ;;
    esac
    ./configure --prefix="$X264_PREFIX" "${X264_FLAGS[@]}" >/dev/null
    make -j"$JOBS" >/dev/null
    make install >/dev/null
    popd >/dev/null
else
    log "Reusing x264 in $X264_PREFIX"
fi

# -- FFmpeg ----------------------------------------------------------------
fetch_at "$FFMPEG_REPO" "$FFMPEG_COMMIT" "$SRC/ffmpeg"
log "Configuring FFmpeg for $TARGET"
pushd "$SRC/ffmpeg" >/dev/null

# Dynamic, GPL, x264: the same feature set vcpkg's overlay produces on Windows,
# so the three platforms expose one encoder list to the application.
# Dynamic on purpose: ffmpeg-sys-the-third links dylibs unless its "static"
# feature is on, and a user must be able to swap in their own FFmpeg. The comment
# stays outside the array because backticks would be command substitution here.
CONFIGURE=(
    --prefix="$PREFIX"
    --disable-programs --disable-doc --disable-debug
    --disable-static --enable-shared
    --enable-pic --enable-runtime-cpudetect --disable-autodetect
    --enable-gpl --enable-version3
    --enable-libx264
    --extra-cflags="-I$X264_PREFIX/include"
    --extra-ldflags="-L$X264_PREFIX/lib"
    "${FF_ARCH_FLAGS[@]}"
    "${FF_EXTRA[@]}"
)
./configure "${CONFIGURE[@]}" >"$WORK/configure-$TARGET.log" 2>&1 || {
    echo "configure failed; see $WORK/configure-$TARGET.log" >&2
    tail -30 "$WORK/configure-$TARGET.log" >&2
    exit 1
}
log "Building FFmpeg for $TARGET (this takes a while)"
make -j"$JOBS" >"$WORK/make-$TARGET.log" 2>&1 || {
    echo "make failed; see $WORK/make-$TARGET.log" >&2
    tail -30 "$WORK/make-$TARGET.log" >&2
    exit 1
}
make install >"$WORK/install-$TARGET.log" 2>&1
popd >/dev/null

log "FFmpeg for $TARGET installed in $PREFIX"
# The application's build script reads this.
echo "FFMPEG_DIR=$PREFIX"
