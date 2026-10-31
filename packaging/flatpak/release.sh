#!/usr/bin/env bash
# Build a single-file Flatpak bundle into out/release/. Publishing it to a
# GitHub Release is the workflow's job; this script only builds.
#
#   packaging/flatpak/release.sh [snapshot|stable]
#
# snapshot (the default, and what a push to main publishes) names the bundle
# <app>-snapshot.flatpak and versions it 0.0.<git rev-list --count HEAD>.
# stable (a v* tag) names it <app>-stable.flatpak and versions it from the tag.
# There is no OSTree repo and no signing: a user installs the file, and a
# newer file is a reinstall. The GNOME runtime comes from Flathub.
#
# Needs `mise run build` already done (the workflow and package:release do
# that). The desktop side is still built by flatpak-builder from
# `git archive HEAD`, so uncommitted changes are refused, as in build.sh.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
top=$(cd "$here/../.." && pwd)
out=$top/out/flatpak
rel=$top/out/release
cd "$top"

manifest=$(echo "$here"/*.yml)
[[ $(echo "$here"/*.yml | wc -w) -eq 1 ]] || {
  echo "release.sh: expected one manifest in $here" >&2
  exit 1
}
id=$(sed -n 's/^id: //p' "$manifest")
runtime=$(sed -n 's/^runtime: //p' "$manifest")
runtime_version=$(sed -n 's/^runtime-version: "\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' "$manifest")
[[ -n $id && -n $runtime && -n $runtime_version ]] || {
  echo "release.sh: $manifest is missing id, runtime or runtime-version" >&2
  exit 1
}

channel=${1:-snapshot}
[[ $channel == snapshot || $channel == stable ]] || {
  echo "release.sh: channel must be snapshot or stable, not $channel" >&2
  exit 1
}
git_branch=$(git symbolic-ref --quiet --short HEAD || true)
short=$(git rev-parse --short=7 HEAD)
count=$(git rev-list --count HEAD)
if [[ $channel == stable ]]; then
  version=${INNKEEPER_RELEASE_VERSION:-${git_branch#v}}
  [[ $version != "$git_branch" && -n $version ]] || version=$(git describe --tags --exact-match 2>/dev/null || true)
  [[ -n $version ]] || {
    echo "release.sh: a stable export needs a version tag (or INNKEEPER_RELEASE_VERSION)" >&2
    exit 1
  }
  version=${version#v}
  subject="Release $version"
else
  version=0.0.$count
  subject="snapshot $short"
fi

if ! git diff --quiet HEAD --; then
  echo "release.sh: uncommitted changes; the Flatpak is built from HEAD" >&2
  exit 1
fi
[[ -f out/stage/share/innkeeper/manifest.tsv ]] || {
  echo "release.sh: run mise run build first" >&2
  exit 1
}

# The desktop side's sources: a clean tree, and the crates it locks.
rm -rf "$out/src" "$out/vendor" "$out/prebuilt"
mkdir -p "$out/src"
git archive HEAD | tar -x -C "$out/src"
cargo vendor -q --locked --manifest-path "$out/src/Cargo.toml" "$out/vendor" >/dev/null

# The Android side: every staged file the host modules didn't write, with
# their rows of the stage manifest (the innkeeper module adds its own).
stage=out/stage
m=share/innkeeper/manifest.tsv
mkdir -p "$out/prebuilt/share/innkeeper"
awk -F'\t' '$2 != "host" && $2 != "launcher"' "$stage/$m" >"$out/prebuilt/$m"
cut -f1 "$out/prebuilt/$m" | (cd "$stage" && cpio -pdm --quiet "$out/prebuilt")

# Builder runs from its own installation: Flatpak hides that installation
# inside the sandbox, so the SDK it builds against lives in another one.
tools=${INNKEEPER_FLATPAK_TOOLS:-$out/tools}
sdk=${INNKEEPER_FLATPAK_SDK:-$out/sdk}
flathub=https://dl.flathub.org/repo/flathub.flatpakrepo
for d in "$tools" "$sdk"; do
  FLATPAK_USER_DIR=$d flatpak remote-add --user --if-not-exists flathub "$flathub"
done
FLATPAK_USER_DIR=$tools flatpak install --user -y --noninteractive --or-update \
  flathub org.flatpak.Builder

# A scratch repo, only so build-bundle has a commit to pack. It is not
# published: there is no update remote.
rm -rf "$rel"
mkdir -p "$rel/repo"
FLATPAK_USER_DIR=$tools flatpak run --user --env=FLATPAK_USER_DIR="$sdk" \
  --command=flatpak-builder org.flatpak.Builder \
  --user --install-deps-from=flathub --force-clean --disable-rofiles-fuse \
  --state-dir="$out/state" "$out/build" "$manifest"
flatpak build-export --subject="$version" --body="$subject" \
  "$rel/repo" "$out/build" "$channel"

# The runtime is not in the bundle; an install fetches it from Flathub.
# No --repo-url: a newer bundle is installed again, not updated from a repo.
bundle=$rel/$id-$channel.flatpak
flatpak build-bundle --runtime-repo="$flathub" \
  "$rel/repo" "$bundle" "$id" "$channel"
rm -rf "$rel/repo"

echo "release.sh: $id $channel $version ($subject)"
echo "release.sh: $bundle"
echo "release.sh: runtime $runtime//$runtime_version from Flathub"
