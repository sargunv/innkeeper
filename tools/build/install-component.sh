#!/usr/bin/env bash
# Install one module of the NDK superbuild (out/build/android) into its own,
# fresh install tree: out/build/<module>/install.
#   tools/build/install-component.sh MODULE
set -euo pipefail
module=${1:?usage: install-component.sh MODULE}
dst=out/build/$module/install
rm -rf "$dst"
cmake --install out/build/android --component "$module" --strip --prefix "$dst" >/dev/null
