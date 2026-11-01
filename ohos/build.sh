#!/usr/bin/env bash
# build:ohos: the OpenHarmony guest, into out/build/ohos/install:
#   share/innkeeper/ohos/sys_prod/  our files on OH's product partition
#                                   (the shim, the exec relay, init cfgs)
#   share/innkeeper/ohos/vendor/    our VDIs, in place of the image's
#   share/innkeeper/ohos/pin.toml   the image pin (image/fetch.sh fetches
#                                   it)
# Both guests share the broker, which build:broker builds.
set -euo pipefail
top=$(cd "$(dirname "$0")/.." && pwd)
cd "$top"
install=out/build/ohos/install
out=$install/share/innkeeper/ohos
rm -rf "$install"
mkdir -p "$out/sys_prod/lib64" "$out/sys_prod/bin" "$out/vendor/lib64/passthrough"
install -D -m 644 -t "$out" ohos/pin.toml
llvm=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
# The shim is freestanding, with no libc headers or link, so the NDK's clang
# can build it.
"$llvm/clang" --target=x86_64-linux-musl -O2 -fPIC -shared -nostdlib -ffreestanding \
  -fno-stack-protector -Wall -Wextra -Werror \
  -o "$out/sys_prod/lib64/libinnkeeper_ohos_shim.so" ohos/shim/ohos_shim.c
cp -r ohos/sys_prod/. "$out/sys_prod/"

# The VDIs link against the image's musl libc, libc++ (std::__h), hilog and
# begetutil (parameters), which OH's SDK would provide. Without the SDK, the
# build uses musl's headers from the build container, copied out once per
# Containerfile, and empty stand-ins that give the linker each library's
# soname. The image's libraries resolve the symbols at load time.
musl=out/build/ohos/musl
dirs=(include lib64)
want="$(sha256sum tools/container/Containerfile | cut -d' ' -f1) ${dirs[*]}"
if [[ $(cat "$musl/.stamp" 2>/dev/null) != "$want" ]]; then
  rm -rf "$musl"
  mkdir -p "$musl"
  tools/container/run.sh cp -r "${dirs[@]/#//usr/x86_64-linux-musl/}" "$musl/"
  echo "$want" >"$musl/.stamp"
fi
# The exec relay (android/system/exec) links statically against the
# container's musl. Its client runs in the session's sandbox, and its server
# runs under OH's init (sys_prod/etc/init/innkeeper_exec.cfg).
"$llvm/clang" --target=x86_64-linux-musl -O2 -static -nostdinc -nostdlib -isystem "$musl/include" \
  -isystem "$("$llvm/clang" -print-resource-dir)/include" -Wall -Werror \
  -o "$out/sys_prod/bin/innkeeper-exec" "$musl/lib64/crt1.o" "$musl/lib64/crti.o" \
  android/system/exec/innkeeper-exec.c "$musl/lib64/libc.a" "$musl/lib64/crtn.o"
"$llvm/llvm-strip" "$out/sys_prod/bin/innkeeper-exec"
stubs=out/build/ohos/stubs
mkdir -p "$stubs"
for lib in libc.so libc++.so libhilog.so libbegetutil.z.so libohaudio.so; do
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
  ohos/vendor/display/composer.cpp "$stubs/bridge_client.o" "$stubs/libbegetutil.z.so"
# The audio VDI is C. OH's IDL-to-VDI adapter loads it under this name.
so=$out/vendor/lib64/libaudio_primary_impl.z.so
"$llvm/clang" "${cflags[@]}" -std=gnu17 -DINNKEEPER_HILOG -I guest/audio-client/include \
  -shared -nostdlib -Wl,-z,now -Wl,-soname,"${so##*/}" -o "$so" \
  ohos/vendor/audio/primary.c guest/audio-client/audio.c guest/audio-client/pcm.c \
  "$stubs/bridge_client.o" "$stubs/libhilog.so" "$stubs/libc.so"
"$llvm/llvm-strip" "$so"
# The e2e audio checks run this tool in OH (ohos/tests/tone.c). It is a test
# artifact and stays out of the layer.
mkdir -p out/test/ohos
# The stubs define no symbols, so the link leaves them for the loader.
"$llvm/clang" "${cflags[@]}" -std=gnu17 -pie -nostdlib -Wl,--unresolved-symbols=ignore-all \
  -Wl,-z,now -Wl,--dynamic-linker=/lib/ld-musl-x86_64.so.1 -o out/test/ohos/tone \
  "$musl/lib64/Scrt1.o" "$musl/lib64/crti.o" ohos/tests/tone.c \
  "$stubs/libohaudio.so" "$stubs/libc.so" "$musl/lib64/crtn.o"
