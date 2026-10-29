#!/usr/bin/env bash
# Register a stage snapshot once (or refresh it to test a Flatpak update).
# The dev runner mounts out/stage over /app, so ordinary rebuilds need no export.
set -euo pipefail
top=$(cd "$(dirname "$0")/../.." && pwd)
export FLATPAK_USER_DIR=$top/out/flatpak/user
out=$top/out/flatpak/dev
id=dev.linuxapk.Manager
mkdir -p "$out"/{home,config,data,cache,state}
export HOME=$out/home XDG_CONFIG_HOME=$out/config XDG_DATA_HOME=$out/data
export XDG_CACHE_HOME=$out/cache XDG_STATE_HOME=$out/state
[[ -f $top/out/stage/share/linuxapk/manifest.tsv ]] || {
  echo 'run mise run build first' >&2
  exit 1
}
rm -rf "$out/app"
mkdir -p "$out/app/files"
cp -a --reflink=auto "$top/out/stage/." "$out/app/files/"
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user -y --noninteractive flathub org.gnome.Platform//51
cat >"$out/app/metadata" <<EOF
[Application]
name=$id
runtime=org.gnome.Platform/x86_64/51
sdk=org.gnome.Sdk/x86_64/51
command=linuxapk
EOF
# Use the distributable app's permissions plus the exact read-only stage.
mapfile -t grants < <(sed -n '/^finish-args:/,/^modules:/s/^  - \(--[^ ]*\)$/\1/p' "$top/packaging/flatpak/$id.yml")
# Menu entries start the registered deployment. Resolve their executable to the
# current stage too, without repacking after every edit.
for program in linuxapk linuxapk-manager; do
  cat >"$out/app/files/bin/$program" <<WRAPPER
#!/bin/sh
exec "$top/out/stage/bin/$program" "\$@"
WRAPPER
  chmod +x "$out/app/files/bin/$program"
done
flatpak build-finish "${grants[@]}" --filesystem="$top/out/stage:ro" "$out/app"
flatpak build-export --timestamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$out/repo" "$out/app"
flatpak remote-add --user --if-not-exists --no-gpg-verify linuxapk-dev "$out/repo"
flatpak install --user -y --noninteractive --reinstall linuxapk-dev "$id"
