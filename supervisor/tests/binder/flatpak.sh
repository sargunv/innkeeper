#!/usr/bin/env bash
# The userspace binder tests inside the real Flatpak runtime. There is no
# kernel-binder oracle in this sandbox: mounting binderfs is forbidden.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
testbin=${LINUXAPK_BINDER_TESTS:-${XDG_DATA_HOME:-$HOME/.local/share}/linuxapk/test-bin/binder}
(cd "$testbin" && sha256sum --quiet -c "$repo/supervisor/tests/binder/test-bin.sha256")
tests=("$@")
((${#tests[@]})) || tests=(
  binderDriverInterfaceTest
  binderLibTest:-BinderLibTest.BinderNetlink*:BinderLibTest.CheckServiceAccessNotOk:BinderLibTest.GetCallingUidWithSetuid:BinderLibTest.InheritRt:BinderLibTest.SchedPolicySet
  binderSafeInterfaceTest
  binderThroughputTest
)
mkdir -p "$repo/out/test/binder"
read -ra args <<<"${BINDER_TEST_ARGS:-}"
rc=0
for spec in "${tests[@]}"; do
  name=${spec%%:*}
  filter=()
  [[ $name == "$spec" ]] || filter=("--gtest_filter=${spec#*:}")
  TIMEOUT=${BINDER_TEST_TIMEOUT:-300} "$repo/tests/bionic/flatpak.sh" -t "$testbin" -- \
    "/data/nativetest64/$name" "${filter[@]}" "${args[@]}" \
    >"$repo/out/test/binder/flatpak-$name.log" 2>&1 || rc=1
  cat "$repo/out/test/binder/flatpak-$name.log"
done
((rc == 0)) || exit "$rc"
echo 'kernel binder oracle unavailable in Flatpak: binderfs mounts are forbidden' >&2
exit 77
