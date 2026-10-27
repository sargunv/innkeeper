#!/usr/bin/env bash
# Build the session's generated pieces into out/session/:
#   overlay/system/lib64/liblinuxapk_shim.so   (NDK; LD_PRELOADed into init)
#   overlay/system/bin/linuxapk-exec           (static; the exec relay, with
#   overlay/system/etc/init/linuxapk-exec.rc    its init service)
#   overlay/system/bin/linuxapk-broker         (Rust; the binder broker)
#   overlay/<partition>/...                    image files with relaxed modes
#   pivot-exec                                 (static; pivots and execs init)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
launcher=$repo/host/launcher/linuxapk-session
out=${LINUXAPK_OUT:-$repo/out}
# The image (linuxapk-session prepare passes these; default: what it selects).
img=${LINUXAPK_IMG:-} apex=${LINUXAPK_APEX:-}
if [[ -z $img || -z $apex ]]; then
  while IFS='=' read -r k v; do
    case $k in
      img) img=$v ;;
      apex) apex=$v ;;
    esac
  done < <("$launcher" image)
fi

ndk=${ANDROID_NDK_HOME:-}
if [[ -z $ndk ]]; then
  ndk=$(printf '%s\n' "$HOME"/Android/Sdk/ndk/* | sort -V | tail -1)
fi
cc="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang"
[[ -x $cc ]] || {
  echo "NDK clang not found (set ANDROID_NDK_HOME)" >&2
  exit 1
}

# Parts of the overlay are owned by subordinate ids (see below): remove it
# from inside a matching user namespace.
podman unshare rm -rf "$out/session/overlay"
mkdir -p "$out/session/overlay/system/lib64" "$out/session/overlay/system/bin" \
  "$out/session/overlay/system/etc/init"
"$cc" -shared -fPIC -O2 -Wall -Werror -fvisibility=hidden -Wl,-soname,liblinuxapk_shim.so \
  -o "$out/session/overlay/system/lib64/liblinuxapk_shim.so" "$here/shim/linuxapk_shim.c"
"$cc" -static -O2 -Wall -Werror -o "$out/session/pivot-exec" "$here/pivot-exec/pivot-exec.c" "$here/pivot-exec/bpfemu.c"
# The exec relay (init service + `linuxapk-session exec` client).
exec_src=$repo/android/system/exec
"$cc" -static -O2 -Wall -Werror -o "$out/session/overlay/system/bin/linuxapk-exec" "$exec_src/linuxapk-exec.c"
install -m 644 "$exec_src/linuxapk-exec.rc" "$out/session/overlay/system/etc/init/linuxapk-exec.rc"

# The binder broker (Rust, dynamic bionic): installed as
# /system/bin/linuxapk-broker. pivot-exec forks+execs it to serve the binder
# seccomp listener (pivot-exec/, broker/src/notify.rs). Built out of tree with
# the NDK like every other vendor/system binary.
ar="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar"
strip="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip"
# The parent execs the broker before init mounts /apex, and the seccomp filter
# traps every openat, so the broker must not depend on the APEX linker. Use
# Android's bootstrap linker (in /system), which loads bootstrap bionic from
# /system/lib64/bootstrap and is available from early boot.
(
  cd "$repo" &&
    env CC_x86_64_linux_android="$cc" AR_x86_64_linux_android="$ar" \
      CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$cc" \
      RUSTFLAGS="-C link-arg=-Wl,--dynamic-linker=/system/bin/bootstrap/linker64" \
      cargo build -q -p linuxapk-broker --release --target x86_64-linux-android --bin linuxapk-broker -j8
)
broker=$out/session/overlay/system/bin/linuxapk-broker
cp "$repo/target/x86_64-linux-android/release/linuxapk-broker" "$broker"
"$strip" --strip-unneeded "$broker"
chmod 0755 "$broker"

# The extracted image lost its fs_config ownership: every file is owned by
# the invoking user (root in the session), so files meant for e.g.
# "0550 logd logd" are unusable by their real owner. Copy such files into
# the overlay with world read/exec bits added.
relax() {
  local part=$1 src=$2
  [[ -d $src ]] || return 0
  (cd "$src" && find . -type f \( ! -perm -o+r -o \( -perm -u+x ! -perm -o+x \) \) -print0) |
    while IFS= read -r -d '' f; do
      mkdir -p "$out/session/overlay/$part/$(dirname "$f")"
      cp -p "$src/$f" "$out/session/overlay/$part/$f"
      chmod o+r,g+r "$out/session/overlay/$part/$f"
      if [[ -x $src/$f ]]; then chmod go+x "$out/session/overlay/$part/$f"; fi
    done
}
relax system "$img/system/system"
relax vendor "$img/vendor"
relax system_ext "$img/system_ext"
relax product "$img/product"
relax odm "$img/odm"

# fs_config ownership/modes that code verifies at runtime. Copies are chowned
# inside "podman unshare", whose id map (0 = us, 1.. = /etc/subuid) matches the
# session's, so e.g. 1029 here is AID_CLAT in Android.
#   path (relative to the flattened /apex)            mode   uid  gid
apex_fs_config=(
  "com.android.tethering/bin/for-system 0750 1029 1000"       # AID_CLAT:AID_SYSTEM
  "com.android.tethering/bin/for-system/clatd 6755 1029 1029" # ClatCoordinator checks
)
for e in "${apex_fs_config[@]}"; do
  read -r rel mode uid gid <<<"$e"
  src=$apex/$rel dst=$out/session/overlay/apex/$rel
  [[ -e $src ]] || continue
  mkdir -p "$(dirname "$dst")"
  if [[ -d $src ]]; then mkdir -p "$dst"; else cp -p "$src" "$dst"; fi
done
# Apply in reverse so files are done before their (possibly 0750) parents.
for ((i = ${#apex_fs_config[@]} - 1; i >= 0; i--)); do
  read -r rel mode uid gid <<<"${apex_fs_config[i]}"
  dst=$out/session/overlay/apex/$rel
  podman unshare sh -c '[ -e "$4" ] || exit 0; chown "$1:$2" "$4" && chmod "$3" "$4"' \
    _ "$uid" "$gid" "$mode" "$dst"
done
# Single-uid mode (docs/sandbox.md) can't use subordinate owners: there,
# overlay-su/ (layered above overlay/) has copies owned by us, with the
# Android owner in the user.linuxapk.owner xattr that the shim reports.
rm -rf "$out/session/overlay-su"
for e in "${apex_fs_config[@]}"; do
  read -r rel mode uid gid <<<"$e"
  src=$apex/$rel dst=$out/session/overlay-su/apex/$rel
  [[ -e $src ]] || continue
  mkdir -p "$(dirname "$dst")"
  if [[ -d $src ]]; then mkdir -p "$dst"; else cp -p "$src" "$dst"; fi
  chmod "$mode" "$dst"
  setfattr -n user.linuxapk.owner -v "$uid:$gid" "$dst"
done
echo "built $out/session"
