# SPDX-License-Identifier: AGPL-3.0-or-later
# The exec relay: an init service, and the client `linuxapk-session exec` runs.
add_executable(linuxapk-exec linuxapk-exec.c)
target_compile_options(linuxapk-exec PRIVATE -Wall -Werror)
target_link_options(linuxapk-exec PRIVATE -static)
install(
  TARGETS linuxapk-exec
  RUNTIME DESTINATION ${LINUXAPK_ANDROID}/system/bin COMPONENT system)
install(
  FILES linuxapk-exec.rc
  DESTINATION ${LINUXAPK_ANDROID}/system/etc/init
  COMPONENT system)
