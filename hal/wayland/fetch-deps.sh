#!/usr/bin/env bash
# Fetch, verify and prepare the pinned Wayland client dependencies for the
# composer's Wayland presenter (Android x86_64, static):
#   - wayland 1.24.0: libwayland-client sources + core protocol
#   - wayland-protocols 1.45: xdg-shell, viewporter, presentation-time,
#     linux-dmabuf v1 (v4+ feedback), fractional-scale-v1, xdg-decoration,
#     xdg-activation-v1
#   - libffi 3.5.2: cross-compiled to a static lib with the NDK
# Protocol glue is generated with the host's wayland-scanner (>= 1.22).
#
# usage: fetch-deps.sh OUT_DIR NDK [API]
# Writes OUT_DIR/{dl,src,ffi,gen}. Idempotent (stamps).
set -euo pipefail
out=$1
NDK=$2
API=${3:-35}

pins=(
  "wayland-1.24.0.tar.xz https://gitlab.freedesktop.org/wayland/wayland/-/releases/1.24.0/downloads/wayland-1.24.0.tar.xz 82892487a01ad67b334eca83b54317a7c86a03a89cfadacfef5211f11a5d0536"
  "wayland-protocols-1.45.tar.xz https://gitlab.freedesktop.org/wayland/wayland-protocols/-/releases/1.45/downloads/wayland-protocols-1.45.tar.xz 4d2b2a9e3e099d017dc8107bf1c334d27bb87d9e4aff19a0c8d856d17cd41ef0"
  "libffi-3.5.2.tar.gz https://github.com/libffi/libffi/releases/download/v3.5.2/libffi-3.5.2.tar.gz f3a3082a23b37c293a4fcd1053147b371f2ff91fa7ea1b2a52e335676bac82dc"
)

mkdir -p "$out/dl" "$out/src"
for p in "${pins[@]}"; do
  read -r file url sha <<<"$p"
  dst=$out/dl/$file
  if [[ ! -f $dst ]] || ! echo "$sha  $dst" | sha256sum -c --quiet 2>/dev/null; then
    curl -fsSL -o "$dst.tmp" "$url"
    echo "$sha  $dst.tmp" | sha256sum -c --quiet || {
      echo "checksum mismatch: $file" >&2
      exit 1
    }
    mv "$dst.tmp" "$dst"
  fi
  name=${file%.tar.*}
  if [[ ! -f $out/src/$name/.extracted ]]; then
    rm -rf "${out:?}/src/$name"
    tar -C "$out/src" -xf "$dst"
    touch "$out/src/$name/.extracted"
  fi
done

wl=$out/src/wayland-1.24.0
wp=$out/src/wayland-protocols-1.45
ffi=$out/src/libffi-3.5.2
tc=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin

# libffi (static, PIC).
if [[ ! -f $out/ffi/.built ]]; then
  rm -rf "$out/ffi" "$out/ffi-build"
  mkdir -p "$out/ffi-build"
  (cd "$out/ffi-build" &&
    CC="$tc/x86_64-linux-android$API-clang" AR="$tc/llvm-ar" RANLIB="$tc/llvm-ranlib" \
      "$ffi/configure" --host=x86_64-linux-android --prefix="$out/ffi" \
      --disable-shared --enable-static --disable-docs --disable-multi-os-directory \
      --with-pic >configure.log 2>&1 &&
    make -j"$(nproc)" >make.log 2>&1 && make install >install.log 2>&1) ||
    {
      echo "libffi build failed; see $out/ffi-build/*.log" >&2
      exit 1
    }
  touch "$out/ffi/.built"
fi

# libwayland-client config and generated code.
gen=$out/gen
scanner=${WAYLAND_SCANNER:-$(command -v wayland-scanner || true)}
[[ -n $scanner ]] || {
  echo "wayland-scanner (>= 1.22) is required on PATH" >&2
  exit 1
}
rm -rf "$gen"
mkdir -p "$gen/include" "$gen/src"
cat >"$gen/config.h" <<'EOF'
/* libwayland config for Bionic (NDK API >= 30). */
#define HAVE_SYS_PRCTL_H 1
#define HAVE_ACCEPT4 1
#define HAVE_MKOSTEMP 1
#define HAVE_POSIX_FALLOCATE 1
#define HAVE_PRCTL 1
#define HAVE_MEMFD_CREATE 1
#define HAVE_MREMAP 1
#define HAVE_STRNDUP 1
#define HAVE_XUCRED_CR_PID 0
#define HAVE_BROKEN_MSG_CMSG_CLOEXEC 0
#define PACKAGE "wayland"
#define PACKAGE_VERSION "1.24.0"
EOF
sed -e 's/@WAYLAND_VERSION@/1.24.0/' -e 's/@WAYLAND_VERSION_MAJOR@/1/' \
  -e 's/@WAYLAND_VERSION_MINOR@/24/' -e 's/@WAYLAND_VERSION_MICRO@/0/' \
  "$wl/src/wayland-version.h.in" >"$gen/include/wayland-version.h"
"$scanner" -s client-header "$wl/protocol/wayland.xml" "$gen/include/wayland-client-protocol.h"
"$scanner" -s client-header -c "$wl/protocol/wayland.xml" "$gen/include/wayland-client-protocol-core.h"
"$scanner" -s public-code "$wl/protocol/wayland.xml" "$gen/src/wayland-protocol.c"

protocols=(
  "$wp/stable/xdg-shell/xdg-shell.xml"
  "$wp/stable/viewporter/viewporter.xml"
  "$wp/stable/presentation-time/presentation-time.xml"
  "$wp/stable/linux-dmabuf/linux-dmabuf-v1.xml"
  "$wp/staging/fractional-scale/fractional-scale-v1.xml"
  "$wp/unstable/xdg-decoration/xdg-decoration-unstable-v1.xml"
  "$wp/staging/xdg-activation/xdg-activation-v1.xml"
)
for x in "${protocols[@]}"; do
  n=$(basename "$x" .xml)
  "$scanner" client-header "$x" "$gen/include/$n-client-protocol.h"
  "$scanner" private-code "$x" "$gen/src/$n-protocol.c"
done
echo "wayland deps ready in $out ($("$scanner" --version 2>&1))"
