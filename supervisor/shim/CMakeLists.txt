# SPDX-License-Identifier: AGPL-3.0-or-later
# LD_PRELOADed into every Android process (host/launcher/src/init.rs).
add_library(innkeeper_shim SHARED innkeeper_shim.c)
set_target_properties(innkeeper_shim PROPERTIES C_VISIBILITY_PRESET hidden)
target_compile_options(innkeeper_shim PRIVATE -Wall -Werror)
install(
  TARGETS innkeeper_shim
  LIBRARY DESTINATION ${INNKEEPER_ANDROID}/system/lib64 COMPONENT supervisor)
