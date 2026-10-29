# SPDX-License-Identifier: AGPL-3.0-or-later
# Static: it runs on the host, in the session's outer namespaces, before the
# pivot into the Android root.
add_executable(pivot-exec pivot-exec.c)
target_compile_options(pivot-exec PRIVATE -Wall -Werror)
target_link_options(pivot-exec PRIVATE -static)
install(
  TARGETS pivot-exec
  RUNTIME DESTINATION libexec/linuxapk COMPONENT supervisor)
