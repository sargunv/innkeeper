#!/usr/bin/env bash
# ohos:build: the OpenHarmony session's layer in out/build/ohos/layer, kept
# out of out/stage and the Android build:
#   bin/linuxapk-broker   the broker, static musl (OH's root has no bionic)
#   sys_prod/             our files on OH's product partition (ohos/sandbox.sh)
set -euo pipefail
top=$(cd "$(dirname "$0")/.." && pwd)
cd "$top"
out=out/build/ohos/layer
rm -rf "$out"
mkdir -p "$out/bin" "$out/sys_prod/lib64"
cargo build -q --release --target x86_64-unknown-linux-musl -p linuxapk-broker --bin linuxapk-broker
llvm=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
install -m 755 -s --strip-program="$llvm/llvm-strip" \
  out/cargo/x86_64-unknown-linux-musl/release/linuxapk-broker "$out/bin/"
# Freestanding: no libc headers or link, so the NDK's clang builds it.
"$llvm/clang" --target=x86_64-linux-musl -O2 -fPIC -shared -nostdlib -ffreestanding \
  -fno-stack-protector -Wall -Wextra -Werror \
  -o "$out/sys_prod/lib64/liblinuxapk_ohos_shim.so" ohos/shim/ohos_shim.c
cp -r ohos/sys_prod/. "$out/sys_prod/"
