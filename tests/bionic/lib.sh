# shellcheck shell=bash
# Shared by the bionic test harnesses (tests/bionic/run.sh,
# ubinder/tests/run-binder-tests.sh): run Android binaries from the session's
# image in a throwaway root, without booting Android.
#
# Sourcing it sets:
#   repo     this checkout
#   out      LINUXAPK_OUT, default <main checkout>/out (where hal/build.sh and
#            gpu/build.sh install out/vendor-overlay)
#   scratch  $out/test/bionic: everything the harnesses write
#   bin      $scratch/bin: pivot-exec, the broker and the props shim

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
main=$(cd "$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
out=${LINUXAPK_OUT:-$main/out}
scratch=$out/test/bionic
bin=$scratch/bin

# Set img and apex to the selected image's partitions and flattened APEXes
# (the paths `linuxapk-session image` prints).
img="" apex=""
bionic_image() {
  local k v
  while IFS='=' read -r k v; do
    case $k in
      img) img=$v ;;
      apex) apex=$v ;;
    esac
  done < <(LINUXAPK_OUT=$out "$repo/session/linuxapk-session" image)
  [[ -d $img/system/system && -f $apex/apex-info-list.xml ]] || {
    echo "no prepared image at $img (run session/linuxapk-session prepare)" >&2
    return 1
  }
}

# Build what runs inside the root into $bin, when its sources changed:
#   pivot-exec           the session's: pivots, installs the seccomp filter
#                        and forks the broker
#   linuxapk-ubinder     the broker (binder in userspace)
#   libprops_shim.so     tests/bionic/props-shim.c
bionic_build() {
  local ndk=${ANDROID_NDK_HOME:-}
  [[ -n $ndk ]] || ndk=$(printf '%s\n' "$HOME"/Android/Sdk/ndk/* | sort -V | tail -1)
  local tc=$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin
  local cc=$tc/x86_64-linux-android30-clang
  [[ -x $cc ]] || {
    echo "NDK clang not found (set ANDROID_NDK_HOME)" >&2
    return 1
  }
  mkdir -p "$bin"
  local shim=$repo/tests/bionic/props-shim.c helper=$repo/session/helper
  [[ $bin/libprops_shim.so -nt $shim ]] ||
    "$cc" -shared -fPIC -O2 -Wall -Werror -fvisibility=hidden -o "$bin/libprops_shim.so" "$shim"
  if [[ ! -x $bin/pivot-exec || -n $(find "$helper" -newer "$bin/pivot-exec") ]]; then
    "$cc" -static -O2 -Wall -Werror -o "$bin/pivot-exec" "$helper/pivot-exec.c" "$helper/bpfemu.c"
  fi
  # The bootstrap linker, as in session/build.sh: the broker starts before
  # anything mounts /apex.
  (
    cd "$repo" &&
      env CC_x86_64_linux_android="$cc" AR_x86_64_linux_android="$tc/llvm-ar" \
        CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$cc" \
        RUSTFLAGS="-C link-arg=-Wl,--dynamic-linker=/system/bin/bootstrap/linker64" \
        cargo build -q -p ubinder --release --target x86_64-linux-android --bin linuxapk-ubinder -j8
  )
  cp -u "$repo/target/x86_64-linux-android/release/linuxapk-ubinder" "$bin/"
}

ro_bind() {
  mount --bind "$1" "$2"
  mount -o remount,bind,ro "$2"
}

# bionic_root ROOT VENDOR_DIR...: assemble an Android root at ROOT (run as
# root of a private user+mount namespace). /vendor is the VENDOR_DIRs stacked
# read-only, first on top. The props shim is in /system/lib64 and
# /vendor/lib64, so LD_PRELOAD=libprops_shim.so finds it in either linker
# namespace.
#
# Host safety (AGENTS.md): /dev is a tmpfs with a few individually bound
# nodes, /sys is an empty tmpfs (plus /sys/fs/bpf for pivot-exec's bpf
# emulator), and nothing from $HOME or /run/user is mounted.
bionic_root() {
  local R=$1
  shift
  mount --make-rprivate /
  mkdir -p "$R"
  mount -t tmpfs -o mode=0755 root "$R"
  mkdir -p "$R"/{system,system_ext,product,vendor,apex,linkerconfig,dev,proc,sys,tmp,data/local/tmp}
  ro_bind "$img/system_ext" "$R/system_ext"
  ro_bind "$img/product" "$R/product"
  ro_bind "$apex" "$R/apex"
  mkdir -p "$R/.shim/lib64"
  cp "$bin/libprops_shim.so" "$R/.shim/lib64/"
  # lowerdir+= per layer: fsconfig caps a single option value at 256 bytes.
  mount -t overlay overlay -o "ro,lowerdir+=$R/.shim,lowerdir+=$img/system/system" "$R/system"
  mount -t overlay overlay -o "ro,lowerdir+=$R/.shim$(printf ',lowerdir+=%s' "$@")" "$R/vendor"
  mount -t tmpfs -o mode=0755 dev "$R/dev"
  mkdir -p "$R/dev/socket"
  local n
  for n in null zero full random urandom; do
    touch "$R/dev/$n"
    mount --bind "/dev/$n" "$R/dev/$n"
  done
  mount -t proc proc "$R/proc"
  mount -t tmpfs -o mode=0755 sys "$R/sys"
  mkdir -p "$R/sys/fs/bpf"
  mount -t tmpfs -o mode=1777 bpf "$R/sys/fs/bpf"
  # Top-level symlinks of the system-as-root image (/bin, /etc, ...).
  local e
  for e in "$img/system"/*; do
    n=$(basename "$e")
    [[ -L $e && ! -e $R/$n ]] && cp -P "$e" "$R/$n"
  done
  return 0
}

# Readable stand-ins for libbinder's access("/dev/binder") check. The seccomp
# filter routes every open of them to the broker.
bionic_broker_nodes() {
  local d
  for d in binder hwbinder vndbinder; do
    : >"$1/dev/$d"
    chmod 0666 "$1/dev/$d"
  done
}

# Clear the environment and set what Android processes expect.
bionic_env() {
  local v
  for v in $(compgen -e); do unset "$v"; done
  export PATH=/system/bin:/vendor/bin ANDROID_ROOT=/system ANDROID_DATA=/data \
    ANDROID_ART_ROOT=/apex/com.android.art ANDROID_I18N_ROOT=/apex/com.android.i18n \
    ANDROID_TZDATA_ROOT=/apex/com.android.tzdata TMPDIR=/data/local/tmp
}
