#!/usr/bin/env bash
# ohos:build: the OpenHarmony session's layer in out/build/ohos/layer, kept
# out of out/stage and the Android build:
#   bin/innkeeper-brk      the broker, static musl (OH's root has no bionic)
#   sys_prod/             our files on OH's product partition (ohos/sandbox.sh)
#   vendor/               our VDIs, in place of the image's (ohos/vendor)
set -euo pipefail
top=$(cd "$(dirname "$0")/.." && pwd)
cd "$top"
out=out/build/ohos/layer
rm -rf "$out"
mkdir -p "$out/bin" "$out/sys_prod/lib64" "$out/vendor/lib64/passthrough"
cargo build -q --release --target x86_64-unknown-linux-musl -p innkeeper-broker --bin innkeeper-brk
llvm=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
install -m 755 -s --strip-program="$llvm/llvm-strip" \
  out/cargo/x86_64-unknown-linux-musl/release/innkeeper-brk "$out/bin/"
# Freestanding: no libc headers or link, so the NDK's clang builds it.
"$llvm/clang" --target=x86_64-linux-musl -O2 -fPIC -shared -nostdlib -ffreestanding \
  -fno-stack-protector -Wall -Wextra -Werror \
  -o "$out/sys_prod/lib64/libinnkeeper_ohos_shim.so" ohos/shim/ohos_shim.c
cp -r ohos/sys_prod/. "$out/sys_prod/"

# The VDIs link against the image's musl libc, libc++ (std::__h) and hilog,
# which OH's SDK would provide. Without it: musl's own headers (pinned
# release, headers only) and empty stand-ins that give each library's soname
# to the linker; the image's libraries resolve the symbols at load time.
musl_version=1.2.5
musl_sha256=a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4
musl=out/ohos/musl-$musl_version
if [[ ! -f $musl/include/stdio.h ]]; then
  rm -rf "$musl" "$musl.src"
  mkdir -p "$musl.src"
  curl -fsSL "https://musl.libc.org/releases/musl-$musl_version.tar.gz" -o "$musl.tar.gz"
  echo "$musl_sha256  $musl.tar.gz" | sha256sum -c --quiet
  tar -xzf "$musl.tar.gz" -C "$musl.src" --strip-components=1
  make -s -C "$musl.src" ARCH=x86_64 prefix= includedir=/include \
    DESTDIR="$top/$musl" install-headers
  rm -rf "$musl.src" "$musl.tar.gz"
fi
stubs=out/build/ohos/stubs
mkdir -p "$stubs"
for lib in libc.so libc++.so libhilog.so; do
  "$llvm/clang" --target=x86_64-linux-musl -shared -nostdlib -Wl,-soname,$lib \
    -o "$stubs/$lib" -x c /dev/null
done
cflags=(--target=x86_64-linux-musl -O2 -fPIC -nostdinc -isystem "$musl/include"
  -isystem "$("$llvm/clang" -print-resource-dir)/include"
  -I guest/bridge-client/include -D_GNU_SOURCE -fvisibility=hidden
  -Wall -Wextra -Werror -Wno-sign-compare) # musl's CMSG_NXTHDR compares signs
"$llvm/clang" "${cflags[@]}" -std=gnu17 -c -o "$stubs/bridge_client.o" \
  guest/bridge-client/bridge_client.c
vdi() {
  local so=$1
  shift
  "$llvm/clang++" "${cflags[@]}" -std=gnu++20 -nostdinc++ -fno-exceptions -fno-rtti \
    -shared -nostdlib -Wl,-z,now -Wl,-soname,"${so##*/}" -o "$so" "$@" \
    "$stubs/libc++.so" "$stubs/libhilog.so" "$stubs/libc.so"
  "$llvm/llvm-strip" "$so"
}
vdi "$out/vendor/lib64/passthrough/libdisplay_buffer_vdi_impl.z.so" \
  ohos/vendor/display/buffer.cpp
vdi "$out/vendor/lib64/libdisplay_composer_vdi_impl.z.so" \
  ohos/vendor/display/composer.cpp "$stubs/bridge_client.o"
