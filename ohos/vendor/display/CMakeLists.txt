# SPDX-License-Identifier: AGPL-3.0-or-later
# Host unit tests of the composer's layer choice. ohos/build.sh builds the
# display VDIs themselves for OH's musl.
add_executable(ohos_display_layers_test tests/layers_test.cpp)
target_compile_options(
  ohos_display_layers_test
  PRIVATE -Wall -Wextra -Werror -UNDEBUG)
set_target_properties(ohos_display_layers_test PROPERTIES CXX_STANDARD 17)
add_test(NAME ohos_display_layers COMMAND ohos_display_layers_test)
