#!/usr/bin/env bash
# AppImage of Linux.apk (packaging/appimage/README.md): the staged prefix
# (out/stage, after `mise run build`) as usr/, with AppRun starting the
# manager, or the CLI. It builds nothing from source and ships no Android
# image; the session fetches that at first use.
#
#   packaging/appimage/build.sh [<out-dir>]   default out/appimage
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
top=$(cd "$here/../.." && pwd)
out=${1:-$top/out/appimage}
id=dev.linuxapk.Manager
s=$top/out/stage
[[ -f $s/share/linuxapk/manifest.tsv ]] || {
  echo "build.sh: no out/stage (run \`mise run build\`)" >&2
  exit 1
}
d=$out/AppDir
rm -rf "$d"
mkdir -p "$d/usr"
cp -a "$s/." "$d/usr/"
# The newest glibc symbol version any host program needs (weak ones aside),
# for AppRun's message.
glibc=$(for f in "$s"/bin/* "$s"/libexec/linuxapk/linuxapk-*; do
  objdump -T "$f" | awk '$2 != "w" && /\*UND\*/' | grep -o 'GLIBC_[0-9.]*'
done | sed 's/GLIBC_//' | sort -uV | tail -1)
sed "s/@GLIBC@/$glibc/" "$here/AppRun" >"$d/AppRun"
chmod 755 "$d/AppRun"
# appimagetool takes the entry and icon at the top, for Gear Lever and other
# integrators.
cp "$s/share/applications/$id.desktop" "$s/share/icons/hicolor/scalable/apps/$id.svg" "$d/"
ln -s "$id.svg" "$d/.DirIcon"
# The runtime is pinned in mise.toml; appimagetool would fetch the latest.
# (`mise which`: the file itself, never its shim.)
runtime=$(mise which appimage-runtime-x86_64) || {
  echo "build.sh: no appimagetool runtime (run \`mise install\`)" >&2
  exit 1
}
ARCH=x86_64 appimagetool --no-appstream --runtime-file "$runtime" \
  "$d" "$out/Linux.apk-x86_64.AppImage"
