#!/usr/bin/env bash
# Build the Innkeeper Flatpak bundle from the committed tree into
# out/release/<app>-<channel>.flatpak, and optionally install it.
#
#   packaging/flatpak/build.sh [snapshot|stable] [--install]
#
# A snapshot bundle (the default) has the version 0.0.<git rev-list --count
# HEAD>, and a stable bundle takes its version from the v* tag on HEAD. The
# bundle isn't signed or published in an OSTree repo. Users install the file
# and update by installing a newer file. The GNOME runtime comes from Flathub.
# --install reinstalls the bundle into FLATPAK_USER_DIR, which defaults to the
# user installation.
#
# flatpak-builder builds the desktop side inside the GNOME SDK from
# `git archive HEAD`. The guest side comes from out/stage (`mise run build`),
# which needs the NDK and the build container, and none of it links against the
# runtime. The script refuses to run with uncommitted changes.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
top=$(cd "$here/../.." && pwd)
out=$top/out/flatpak
rel=$top/out/release
manifest=$here/dev.sargunv.Innkeeper.yml
cd "$top"

channel=snapshot install=false
for arg in "$@"; do
  case $arg in
    snapshot | stable) channel=$arg ;;
    --install) install=true ;;
    *)
      echo "build.sh: usage: build.sh [snapshot|stable] [--install]" >&2
      exit 2
      ;;
  esac
done
id=$(sed -n 's/^id: //p' "$manifest")
if [[ $channel == stable ]]; then
  version=$(git describe --tags --exact-match --match 'v*' 2>/dev/null) || {
    echo "build.sh: a stable bundle needs a v* tag on HEAD" >&2
    exit 1
  }
  version=${version#v}
  subject="Release $version"
else
  version=0.0.$(git rev-list --count HEAD)
  subject="snapshot $(git rev-parse --short=7 HEAD)"
fi

if ! git diff --quiet HEAD --; then
  echo "build.sh: uncommitted changes; the Flatpak is built from HEAD" >&2
  exit 1
fi
[[ -f out/stage/share/innkeeper/manifest.tsv ]] || {
  echo "build.sh: run mise run build first" >&2
  exit 1
}

# The desktop side builds from a clean tree and its locked, vendored crates.
rm -rf "$out/src" "$out/vendor" "$out/prebuilt"
mkdir -p "$out/src"
git archive HEAD | tar -x -C "$out/src"
cargo vendor -q --locked --manifest-path "$out/src/Cargo.toml" "$out/vendor" >/dev/null

# The guest side is every staged file that the host module didn't write, with
# its rows of the stage manifest. The innkeeper module adds its own rows.
stage=out/stage
m=share/innkeeper/manifest.tsv
mkdir -p "$out/prebuilt/share/innkeeper"
awk -F'\t' '$2 != "host"' "$stage/$m" >"$out/prebuilt/$m"
cut -f1 "$out/prebuilt/$m" | (cd "$stage" && cpio -pdm --quiet "$out/prebuilt")

# Flatpak hides Builder's own installation inside the sandbox, so the SDK that
# Builder builds against has to live in a second installation.
tools=$out/tools sdk=$out/sdk
flathub=https://dl.flathub.org/repo/flathub.flatpakrepo
for d in "$tools" "$sdk"; do
  FLATPAK_USER_DIR=$d flatpak remote-add --user --if-not-exists flathub "$flathub"
done
FLATPAK_USER_DIR=$tools flatpak install --user -y --noninteractive --or-update \
  flathub org.flatpak.Builder

# build-bundle packs a commit from a repo, so export to a scratch repo first.
rm -rf "$rel"
mkdir -p "$rel/repo"
FLATPAK_USER_DIR=$tools flatpak run --user --env=FLATPAK_USER_DIR="$sdk" \
  --command=flatpak-builder org.flatpak.Builder \
  --user --install-deps-from=flathub --force-clean --disable-rofiles-fuse \
  --state-dir="$out/state" "$out/build" "$manifest"
flatpak build-export --subject="$version" --body="$subject" \
  "$rel/repo" "$out/build" "$channel"
# The bundle leaves out the runtime, which an install fetches from Flathub.
# There is no --repo-url because users update by installing a newer bundle.
bundle=$rel/$id-$channel.flatpak
flatpak build-bundle --runtime-repo="$flathub" "$rel/repo" "$bundle" "$id" "$channel"
rm -rf "$rel/repo"
echo "build.sh: $bundle ($id $channel $version, $subject)"

if $install; then
  flatpak remote-add --user --if-not-exists flathub "$flathub"
  flatpak install --user -y --noninteractive --reinstall "$bundle"
fi
