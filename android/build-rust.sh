#!/bin/sh
# Builds libmemo.so (crates/android) into app/src/main/jniLibs for arm64 phones and the x86_64
# emulator. Needs the NDK (ANDROID_NDK_HOME, default: the version below in ~/Android/Sdk) and
# `rustup target add aarch64-linux-android x86_64-linux-android`.
set -eu
cd "$(dirname "$0")"
NDK=${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/27.3.13750724}
if [ ! -d "$NDK" ]; then
  echo "no NDK at $NDK: install NDK 27.3.13750724 (README.md, Toolchain) or set ANDROID_NDK_HOME" >&2
  exit 1
fi
BIN=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
API=30

# 16 KB page alignment, required on newer devices (NDK r28+ does this by default).
export RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$BIN/aarch64-linux-android$API-clang
export CC_aarch64_linux_android=$BIN/aarch64-linux-android$API-clang
export AR_aarch64_linux_android=$BIN/llvm-ar
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=$BIN/x86_64-linux-android$API-clang
export CC_x86_64_linux_android=$BIN/x86_64-linux-android$API-clang
export AR_x86_64_linux_android=$BIN/llvm-ar

for pair in aarch64-linux-android:arm64-v8a x86_64-linux-android:x86_64; do
  target=${pair%%:*}
  abi=${pair##*:}
  cargo build --quiet --manifest-path ../Cargo.toml -p memo-android --profile android --target "$target"
  mkdir -p "app/src/main/jniLibs/$abi"
  cp "../target/$target/android/libmemo.so" "app/src/main/jniLibs/$abi/"
done
