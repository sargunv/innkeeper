#!/usr/bin/env bash
# Export a release of the Linux.apk Flatpak: an OSTree repo and a single-file
# bundle, both under out/release/. Publishing (GitHub Release, Pages) is the
# workflow's job; this script only builds.
#
#   packaging/flatpak/release.sh
#
# Needs `mise run build` already done (the workflow and package:release do
# that). The desktop side is still built by flatpak-builder from
# `git archive HEAD`, so uncommitted changes are refused, as in build.sh.
#
# out/release/repo is the OSTree repo. A previous repo restored there (the
# gh-pages history, in CI) is committed into, so updates stay in one history.
# out/release/publish/ is what a static host serves: the repo, a .flatpakrepo,
# a .flatpakref and index.html. out/release/<app>.flatpak is the bundle.
#
# FLATPAK_GPG_PRIVATE_KEY (ASCII-armored) and FLATPAK_GPG_KEY_ID sign the
# commit and the repo summary. Both absent: the repo is unsigned and the
# .flatpakrepo says no GpgVerify (a local dry run). One of them set is an
# error. The key stays in a temporary GNUPGHOME under out/release/.
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
# The remote name is the last component of the app id, lowercased, so a rename
# of the id renames the remote with it.
remote=${id##*.}
remote=${remote,,}

origin=$(git remote get-url origin)
repo_name=${origin%.git}
repo_name=${repo_name##*/}
repo_name=${repo_name##*:}
[[ -n $repo_name ]] || {
  echo "release.sh: could not derive the repository name from $origin" >&2
  exit 1
}
# Project Pages on the account's custom domain: /<repository>/.
pages_url=https://code.sargunv.dev/$repo_name
branch=$(git symbolic-ref --quiet --short HEAD || true)
version=${LINUXAPK_RELEASE_VERSION:-${branch#v}}
[[ $version != "$branch" && -n $version ]] || version=$(git describe --tags --always)

if ! git diff --quiet HEAD --; then
  echo "release.sh: uncommitted changes; the Flatpak is built from HEAD" >&2
  exit 1
fi
[[ -f out/stage/share/linuxapk/manifest.tsv ]] || {
  echo "release.sh: run mise run build first" >&2
  exit 1
}

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

# Builder runs from its own installation: Flatpak hides that installation
# inside the sandbox, so the SDK it builds against lives in another one.
tools=${LINUXAPK_FLATPAK_TOOLS:-$out/tools}
sdk=${LINUXAPK_FLATPAK_SDK:-$out/sdk}
flathub=https://dl.flathub.org/repo/flathub.flatpakrepo
for d in "$tools" "$sdk"; do
  FLATPAK_USER_DIR=$d flatpak remote-add --user --if-not-exists flathub "$flathub"
done
FLATPAK_USER_DIR=$tools flatpak install --user -y --noninteractive --or-update \
  flathub org.flatpak.Builder

mkdir -p "$rel"
# A restored repo stays; only a fresh export starts empty. publish/ is rewritten.
if [[ ! -d $rel/repo ]]; then
  mkdir -p "$rel/repo"
fi
rm -rf "$rel/publish" "$rel/gpg"
mkdir -p "$rel/publish" "$rel/gpg"
chmod 700 "$rel/gpg"

gpg_args=()
if [[ -n ${FLATPAK_GPG_PRIVATE_KEY:-} || -n ${FLATPAK_GPG_KEY_ID:-} ]]; then
  [[ -n ${FLATPAK_GPG_PRIVATE_KEY:-} && -n ${FLATPAK_GPG_KEY_ID:-} ]] || {
    echo "release.sh: set both FLATPAK_GPG_PRIVATE_KEY and FLATPAK_GPG_KEY_ID, or neither" >&2
    exit 1
  }
  export GNUPGHOME=$rel/gpg
  printf '%s\n' "$FLATPAK_GPG_PRIVATE_KEY" | gpg --batch --import
  gpg --batch --export --armor "$FLATPAK_GPG_KEY_ID" >"$rel/gpg/public.asc"
  [[ -s $rel/gpg/public.asc ]] || {
    echo "release.sh: FLATPAK_GPG_KEY_ID exported no public key" >&2
    exit 1
  }
  gpg_args=(--gpg-homedir="$rel/gpg" --gpg-sign="$FLATPAK_GPG_KEY_ID")
fi

# flatpak-builder's sandbox has no gpg. A signed release finishes in the
# sandbox, then the host's flatpak build-export signs the only commit. An
# unsigned dry run lets the builder commit.
builder=(
  flatpak run --user --env=FLATPAK_USER_DIR="$sdk" --command=flatpak-builder
  org.flatpak.Builder
  --user --install-deps-from=flathub --force-clean --disable-rofiles-fuse
  --state-dir="$out/state"
)
# stable matches the .flatpakref. flatpak-builder's own export uses master.
if ((${#gpg_args[@]})); then
  FLATPAK_USER_DIR=$tools "${builder[@]}" "$out/build" "$manifest"
  flatpak build-export "${gpg_args[@]}" --update-appstream "$rel/repo" "$out/build" stable
else
  FLATPAK_USER_DIR=$tools "${builder[@]}" --repo="$rel/repo" \
    --default-branch=stable "$out/build" "$manifest"
fi

if [[ -s ${rel}/gpg/public.asc ]]; then
  gpg_b64=$(base64 -w0 "$rel/gpg/public.asc")
  gpg_lines=$(fold -w 72 <<<"$gpg_b64" | sed 's/^/GPGKey=/')
else
  gpg_lines=
fi

cat >"$rel/publish/$remote.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Linux.apk
Url=$pages_url/repo
Homepage=https://github.com/sargunv/$repo_name
Comment=Android apps on the Linux desktop
Description=The Linux.apk Flatpak repository
Icon=https://github.com/sargunv/$repo_name/raw/main/host/manager/data/$id.svg
EOF
if [[ -n $gpg_lines ]]; then
  printf '%s\n' "$gpg_lines" >>"$rel/publish/$remote.flatpakrepo"
else
  printf 'GPGKey=\nNoGpgVerify=true\n' >>"$rel/publish/$remote.flatpakrepo"
fi

cat >"$rel/publish/$id.flatpakref" <<EOF
[Flatpak Ref]
Name=$id
Title=Linux.apk
Branch=stable
Url=$pages_url/repo
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
IsRuntime=false
SuggestRemoteName=$remote
EOF
if [[ -n $gpg_lines ]]; then
  printf '%s\n' "$gpg_lines" >>"$rel/publish/$id.flatpakref"
fi

# The runtime is not in this repo; a bundle install fetches it from Flathub.
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  "$rel/repo" "$rel/$id.flatpak" "$id" stable

flatpak build-update-repo --generate-static-deltas --prune "${gpg_args[@]}" "$rel/repo"

# Static hosting: the OSTree repo plus the files a browser and flatpak open.
cp -a "$rel/repo" "$rel/publish/repo"
cat >"$rel/publish/index.html" <<EOF
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Linux.apk</title>
</head>
<body>
<h1>Linux.apk $version</h1>
<p>Add the repository once. Later releases arrive with <code>flatpak update</code>.</p>
<pre><code>flatpak remote-add --if-not-exists $remote $pages_url/$remote.flatpakrepo
flatpak install $remote $id</code></pre>
<p>Or install <a href="$id.flatpakref">$id.flatpakref</a> directly.</p>
<p>A single-file bundle, <code>$id.flatpak</code>, is attached to
<a href="https://github.com/sargunv/$repo_name/releases/tag/v$version">the GitHub release</a>.
It needs the Flathub remote for the $runtime//$runtime_version runtime:</p>
<pre><code>flatpak install ./$id.flatpak</code></pre>
</body>
</html>
EOF

echo "release.sh: $id ($remote) at $pages_url"
echo "release.sh: repo $rel/repo"
echo "release.sh: bundle $rel/$id.flatpak"
echo "release.sh: publish $rel/publish"
