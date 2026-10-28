#!/usr/bin/env bash
# Builds the Linux.apk Flatpak from the committed tree and installs it.
#
#   packaging/flatpak/build.sh
#
# What runs on the desktop is built by flatpak-builder inside the GNOME SDK
# from `git archive HEAD`. What runs inside Android (bionic and static
# binaries, the overlay, the system app, the image scripts) comes from
# `mise run build`, which needs the NDK and the build container; none of it
# links the runtime. Both come from HEAD: uncommitted changes are refused.
#
# FLATPAK_USER_DIR is the installation it installs into, and must be set:
# use a private one for tests. org.flatpak.Builder runs from
# LINUXAPK_FLATPAK_TOOLS (default out/flatpak/tools), another installation,
# since Flatpak hides the installation an app runs from inside its sandbox.
# Both get Flathub as a remote; the Builder, the SDK and its Rust extension
# come from there.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
top=$(cd "$here/../.." && pwd)
out=$top/out/flatpak
id=dev.linuxapk.Manager
: "${FLATPAK_USER_DIR:?set FLATPAK_USER_DIR to the installation to install into}"
tools=${LINUXAPK_FLATPAK_TOOLS:-$out/tools}
cd "$top"

if ! git diff --quiet HEAD --; then
  echo "build.sh: uncommitted changes; the Flatpak is built from HEAD" >&2
  exit 1
fi
mise run build

# The desktop side's sources: a clean tree, and the crates it locks.
rm -rf "$out/src" "$out/vendor" "$out/prebuilt"
mkdir -p "$out/src"
git archive HEAD | tar -x -C "$out/src"
cargo vendor -q --locked --manifest-path "$out/src/Cargo.toml" "$out/vendor" >/dev/null

# The Android side: every staged file the host modules didn't write, with
# their rows of the stage manifest (the linuxapk module adds its own).
stage=out/stage
m=share/linuxapk/manifest.tsv
mkdir -p "$out/prebuilt/share/linuxapk"
awk -F'\t' '$2 != "host" && $2 != "launcher"' "$stage/$m" >"$out/prebuilt/$m"
cut -f1 "$out/prebuilt/$m" | (cd "$stage" && cpio -pdm --quiet "$out/prebuilt")

target=$FLATPAK_USER_DIR
flathub=https://dl.flathub.org/repo/flathub.flatpakrepo
for d in "$tools" "$target"; do
  FLATPAK_USER_DIR=$d flatpak remote-add --user --if-not-exists flathub "$flathub"
done
FLATPAK_USER_DIR=$tools flatpak install --user -y --noninteractive --or-update \
  flathub org.flatpak.Builder
FLATPAK_USER_DIR=$tools flatpak run --user --env=FLATPAK_USER_DIR="$target" \
  --command=flatpak-builder org.flatpak.Builder \
  --user --install-deps-from=flathub --force-clean --disable-rofiles-fuse \
  --state-dir="$out/state" --repo="$out/repo" "$out/build" "$here/$id.yml"
flatpak remote-add --user --if-not-exists --no-gpg-verify linuxapk-local "$out/repo"
if flatpak info --user "$id" >/dev/null 2>&1; then
  flatpak update --user -y --noninteractive "$id"
else
  flatpak install --user -y --noninteractive linuxapk-local "$id"
fi
