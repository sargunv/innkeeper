#!/usr/bin/env bash
# End-to-end test of `linuxapk` against a real session (its own instance).
#   tests/e2e/cli/run.sh [corpus-dir]
# Installs OpenCalc and Wikipedia, validates their desktop entries and icons
# (rendered in Android by the system app's IconDump), launches them through
# gtk-launch and gio open (checking the resumed activity with dumpsys),
# reinstalls (idempotent), repairs a stale entry on `list`, then uninstalls
# and checks that no desktop files or icons are left. Uses the
# staged build (out/stage). Desktop entries, icons and caches go to a scratch
# XDG tree under out/, never the user's. Stops and resets its named session.
#
# Environment: LINUXAPK_INSTANCE (default cli), LINUXAPK_CORPUS (default
# out/test/compat/corpus, filled by `mise run test:compat -- fetch`).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then
  export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-cli}
  export LINUXAPK_TEST_LAUNCHER_PORTAL=${LINUXAPK_TEST_LAUNCHER_PORTAL:-gtk}
  exec "$repo/tests/nested.sh" "$0" "$@"
fi
corpus=${1:-${LINUXAPK_CORPUS:-$repo/out/test/compat/corpus}}
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-cli}
session=$repo/tools/dev/session.sh
la=$repo/tools/dev/linuxapk.sh
[[ -x $la ]] || {
  echo "no $la (run \`mise run build\`)" >&2
  exit 1
}

# The dev driver gives the session a fresh /data, and nested.sh the desktop
# side a scratch XDG tree.
"$session" stop >/dev/null
"$session" reset >/dev/null
confirm_pid=""
cleanup() {
  [[ -z $confirm_pid ]] || kill "$confirm_pid" 2>/dev/null || true
  "$session" stop >/dev/null
  "$session" reset >/dev/null
}
trap cleanup EXIT
if [[ ${LINUXAPK_TEST_LAUNCHER_PORTAL:-} == gtk ]]; then
  /usr/bin/python3 "$here/confirm.py" &
  confirm_pid=$!
fi
flatpak_prefix=Manager.
apps=$XDG_DATA_HOME/applications
icons=$XDG_DATA_HOME/icons/hicolor

fail() {
  echo "FAIL $*" >&2
  exit 1
}
ok() { echo "ok   $*"; }

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
  out=$("$la" install "${apk[$pkg]}" 2>&1) || fail "install $pkg: $out"
  echo "$out"
  ! grep -q "couldn't render the icon" <<<"$out" || fail "$pkg: Android didn't render the icon"
  d=$apps/dev.linuxapk.$pkg.desktop
  [[ -f $d ]] || fail "no $d"
  if command -v desktop-file-validate >/dev/null; then
    out=$(desktop-file-validate "$d") || fail "desktop-file-validate $d: $out"
    [[ -z $out ]] || echo "$out"
  else
    podman run --rm --memory=512m --pids-limit=128 -v "$d:/x.desktop:ro,Z" registry.fedoraproject.org/fedora-minimal:latest \
      sh -c 'microdnf -q -y install desktop-file-utils >/dev/null && desktop-file-validate /x.desktop' ||
      fail "desktop-file-validate $d"
  fi
  ok "$pkg: desktop entry valid"
  grep -q "^Exec=.* run $pkg %U$" "$d" || fail "Exec line"
  grep -q "^StartupWMClass=dev.linuxapk.${flatpak_prefix}$pkg$" "$d" || fail "StartupWMClass"
  ! grep -Eq 'x-scheme-handler/https?;' "$d" || fail "declares http(s)"
  for s in 48 256 512; do
    file "$icons/${s}x$s/apps/dev.linuxapk.$pkg.png" | grep -q "PNG image data, $s x $s" ||
      fail "icon ${s}px"
  done
  ok "$pkg: icons 48-512 px, rendered by Android"
done
grep -q '^MimeType=x-scheme-handler/wikipedia;$' "$apps/dev.linuxapk.org.wikipedia.desktop" ||
  fail "wikipedia scheme handler"
ok "Wikipedia declares x-scheme-handler/wikipedia only"

# Idempotent: a reinstall rewrites the same entry.
before=$(sha256sum "$apps/dev.linuxapk.org.wikipedia.desktop")
"$la" install "${apk["org.wikipedia"]}" >/dev/null
[[ $(sha256sum "$apps/dev.linuxapk.org.wikipedia.desktop") == "$before" ]] || fail "reinstall changed the entry"
ok "reinstall is idempotent"

# An entry from an older Linux.apk (LINUXAPK_SESSION, a program that's gone)
# is pointed at this linuxapk by `list`, keeping its instance.
stale=$apps/dev.linuxapk.org.example.stale.desktop
cat >"$stale" <<EOF
[Desktop Entry]
Type=Application
Name=Stale
Exec=env LINUXAPK_SESSION=/gone/linuxapk-session LINUXAPK_INSTANCE=$LINUXAPK_INSTANCE /gone/linuxapk run org.example.stale %U
X-LinuxApk-Package=org.example.stale
EOF
"$la" list | grep -q $'^org.wikipedia\tWikipedia$' || fail "list"
ok "list"
repair_cli=linuxapk
want="Exec=env LINUXAPK_INSTANCE=$LINUXAPK_INSTANCE ${repair_cli} run org.example.stale %U"
[[ $(grep '^Exec=' "$stale") == "$want" ]] || fail "stale entry not repaired: $(grep '^Exec=' "$stale")"
rm "$stale"
ok "list repairs a stale entry"

if [[ ${LINUXAPK_TEST_UPDATE:-} == 1 ]]; then
  before_commit=$(flatpak info --user --show-commit dev.linuxapk.Manager)
  before_entry=$(sha256sum "$apps/dev.linuxapk.org.wikipedia.desktop")
  "$repo/packaging/flatpak/dev-init.sh"
  after_commit=$(flatpak info --user --show-commit dev.linuxapk.Manager)
  [[ $before_commit != "$after_commit" ]] || fail "deployment did not update"
  [[ $(sha256sum "$apps/dev.linuxapk.org.wikipedia.desktop") == "$before_entry" ]] || fail "update changed app entry"
  ok "Flatpak updated $before_commit -> $after_commit; existing entry retained"
fi

# Launch through the desktop, as the app menu would (LINUXAPK_INSTANCE unset:
# the entry carries it).
env -u LINUXAPK_INSTANCE gtk-launch dev.linuxapk.${flatpak_prefix}com.darkempire78.opencalculator
wait_top com.darkempire78.opencalculator
ok "gtk-launch: OpenCalc resumed ($(top))"
echo 'SKIP desktop URI association: DynamicLauncher does not rebuild the MIME cache'
"$la" run org.wikipedia 'wikipedia://en.wikipedia.org/wiki/Linux'
wait_top org.wikipedia PageActivity
ok "Wikipedia URI resumed ($(top))"

if [[ ${LINUXAPK_TEST_UPDATE:-} == 1 ]]; then
  "$session" stop >/dev/null
  # A cold CLI launch must show feedback before Android has booted.
  "$session" wait >/dev/null
  /usr/bin/python3 "$here/splash.py" "$repo/out/test/splash-$LINUXAPK_INSTANCE.log" \
    "$la" run com.darkempire78.opencalculator || fail "splash exceeded 200 ms or launch failed"
  wait_top com.darkempire78.opencalculator
fi

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
