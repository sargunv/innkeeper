#!/usr/bin/env bash
# End-to-end test of `linuxapk` against a real session (its own instance).
#   tests/e2e/cli/run.sh [corpus-dir]
# Installs OpenCalc and Wikipedia, validates their desktop entries and icons,
# launches them through gtk-launch and xdg-open (checking the resumed activity
# with dumpsys), reinstalls (idempotent), then uninstalls and checks that no
# desktop files or icons are left. Uses the staged build (out/stage), and
# leaves the session running; stop it with
# `LINUXAPK_INSTANCE=cli mise run session -- stop`.
#
# Environment: LINUXAPK_INSTANCE (default cli), LINUXAPK_CORPUS (default
# out/test/compat/corpus, filled by `tests/compat/run.sh fetch`).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
corpus=${1:-${LINUXAPK_CORPUS:-$repo/out/test/compat/corpus}}
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-cli}
session=$repo/out/stage/libexec/linuxapk/linuxapk-session
la=$repo/out/stage/bin/linuxapk
[[ -x $la ]] || {
  echo "no $la (run \`mise run build\`)" >&2
  exit 1
}
apps=${XDG_DATA_HOME:-$HOME/.local/share}/applications
icons=${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor

fail() {
  echo "FAIL $*" >&2
  exit 1
}
ok() { echo "ok   $*"; }

if ! "$session" status >/dev/null 2>&1; then
  avail=$(awk '/^MemAvailable:/ {print int($2 / 1048576)}' /proc/meminfo)
  ((avail >= 12)) || fail "only ${avail} GB available; not starting a session"
fi

top() {
  "$session" exec dumpsys activity activities 2>/dev/null |
    sed -n 's/.*topResumedActivity=ActivityRecord{[^ ]* u0 \([^ ]*\) .*/\1/p' | head -1
}
wait_top() { # package [activity-substring]
  for _ in $(seq 30); do
    t=$(top)
    [[ $t == "$1/"*"${2:-}"* ]] && return 0
    sleep 1
  done
  fail "top activity is '$t', wanted $1 ${2:-}"
}

declare -A apk=(
  ["com.darkempire78.opencalculator"]=$corpus/com.darkempire78.opencalculator_53.apk
  ["org.wikipedia"]=$corpus/org.wikipedia_50612.apk
)
for pkg in "${!apk[@]}"; do
  [[ -f ${apk[$pkg]} ]] || fail "missing corpus APK ${apk[$pkg]}"
  "$la" install "${apk[$pkg]}"
  d=$apps/dev.linuxapk.$pkg.desktop
  [[ -f $d ]] || fail "no $d"
  if command -v desktop-file-validate >/dev/null; then
    out=$(desktop-file-validate "$d") || fail "desktop-file-validate $d: $out"
    [[ -z $out ]] || echo "$out"
  else
    podman run --rm -v "$d:/x.desktop:ro,Z" registry.fedoraproject.org/fedora-minimal:latest \
      sh -c 'microdnf -q -y install desktop-file-utils >/dev/null && desktop-file-validate /x.desktop' ||
      fail "desktop-file-validate $d"
  fi
  ok "$pkg: desktop entry valid"
  grep -q "^Exec=.* run $pkg %U$" "$d" || fail "Exec line"
  grep -q "^StartupWMClass=dev.linuxapk.$pkg$" "$d" || fail "StartupWMClass"
  ! grep -Eq 'x-scheme-handler/https?;' "$d" || fail "declares http(s)"
  for s in 48 256 512; do
    file "$icons/${s}x$s/apps/dev.linuxapk.$pkg.png" | grep -q "PNG image data, $s x $s" ||
      fail "icon ${s}px"
  done
  ok "$pkg: icons 48-512 px"
done
grep -q '^MimeType=x-scheme-handler/wikipedia;$' "$apps/dev.linuxapk.org.wikipedia.desktop" ||
  fail "wikipedia scheme handler"
ok "Wikipedia declares x-scheme-handler/wikipedia only"

# Idempotent: a reinstall rewrites the same entry.
before=$(sha256sum "$apps/dev.linuxapk.org.wikipedia.desktop")
"$la" install "${apk["org.wikipedia"]}" >/dev/null
[[ $(sha256sum "$apps/dev.linuxapk.org.wikipedia.desktop") == "$before" ]] || fail "reinstall changed the entry"
ok "reinstall is idempotent"

"$la" list | grep -q $'^org.wikipedia\tWikipedia$' || fail "list"
ok "list"

# Launch through the desktop, as the app menu would (no lane env in scope:
# the entry carries it).
env -u LINUXAPK_INSTANCE gtk-launch dev.linuxapk.com.darkempire78.opencalculator
wait_top com.darkempire78.opencalculator
ok "gtk-launch: OpenCalc resumed ($(top))"
env -u LINUXAPK_INSTANCE xdg-open 'wikipedia://en.wikipedia.org/wiki/Linux'
wait_top org.wikipedia PageActivity
ok "xdg-open wikipedia://: Wikipedia resumed ($(top))"

# Untrusted input is refused.
! "$la" run org.wikipedia 'file:///etc/passwd' 2>/dev/null || fail "file: URI accepted"
! "$la" run '../../x' 2>/dev/null || fail "bad package accepted"
ok "bad URIs and package names refused"

if [[ ${KEEP:-} != 1 ]]; then
  for pkg in "${!apk[@]}"; do
    "$la" uninstall "$pkg"
    [[ ! -e $apps/dev.linuxapk.$pkg.desktop ]] || fail "entry left for $pkg"
    [[ -z $(find "$icons" -name "dev.linuxapk.$pkg.png") ]] || fail "icons left for $pkg"
    ! "$session" exec cmd package path "$pkg" >/dev/null 2>&1 || fail "$pkg still installed"
  done
  ok "uninstalled cleanly"
fi
echo "all ok"
