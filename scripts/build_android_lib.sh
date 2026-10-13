#!/bin/sh
# Build libraptor_ffi.so and drop it into the example app's jniLibs.
#
# Wraps the two steps that have to stay in sync:
#   1. cargo build --target aarch64-linux-android --release -p raptor-ffi
#   2. copy the .so to android/app/src/main/jniLibs/arm64-v8a/
#
# cargo-ndk would do both, but it is not required: the linker and
# FFMPEG_DIR are already pinned in .cargo/config.toml.
#
# Usage (from repo root):
#   sh scripts/build_android_lib.sh
#   DEBUG=1 sh scripts/build_android_lib.sh      # debug profile

set -e

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO_ROOT"

PROFILE=release
[ "${DEBUG:-0}" = "1" ] && PROFILE=debug

DEST="$REPO_ROOT/dart/raptor_flutter/example/android/app/src/main/jniLibs/arm64-v8a"
SRC="$REPO_ROOT/target/aarch64-linux-android/$PROFILE/libraptor_ffi.so"

echo "==> building raptor-ffi ($PROFILE) for aarch64-linux-android"
cargo build --target aarch64-linux-android --profile "$PROFILE" -p raptor-ffi

if [ ! -f "$SRC" ]; then
    echo "error: expected $SRC to exist after the build" >&2
    exit 1
fi

mkdir -p "$DEST"
cp "$SRC" "$DEST/libraptor_ffi.so"

echo "==> installed $(du -h "$DEST/libraptor_ffi.so" | cut -f1) -> $DEST"
echo
echo "Next: cd dart/raptor_flutter/example && flutter run \\"
echo "        --dart-define=RAPTOR_VIDEO=/path/to/video.mp4"
