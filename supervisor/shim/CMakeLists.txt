# SPDX-License-Identifier: AGPL-3.0-or-later
# LD_PRELOADed into every Android process (ns-init.sh).
add_library(linuxapk_shim SHARED linuxapk_shim.c)
set_target_properties(linuxapk_shim PROPERTIES C_VISIBILITY_PRESET hidden)
target_compile_options(linuxapk_shim PRIVATE -Wall -Werror)
install(
  TARGETS linuxapk_shim
  LIBRARY DESTINATION ${LINUXAPK_ANDROID}/system/lib64 COMPONENT supervisor)
