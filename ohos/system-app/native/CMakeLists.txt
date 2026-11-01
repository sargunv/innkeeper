# SPDX-License-Identifier: AGPL-3.0-or-later
# The host tests of the system app's bridge connection (preset "host-tests").
# ohos/system-app/build.sh builds the module itself.
add_executable(system_app_connection_test connection.c tests/connection_test.c)
target_link_libraries(
  system_app_connection_test
  PRIVATE innkeeper_bridge_client)
target_compile_options(system_app_connection_test PRIVATE -Wall -Wextra -Werror)
add_test(NAME system_app_connection COMMAND system_app_connection_test)
