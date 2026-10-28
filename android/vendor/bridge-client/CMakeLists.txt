# SPDX-License-Identifier: AGPL-3.0-or-later
# liblinuxapk_bridge_client.a: the client side of the bridge protocol, linked
# into the composer and libtinyalsav2. It installs nothing. Natively (preset
# "host-tests") it also builds its framing test.
add_library(linuxapk_bridge_client STATIC bridge_client.c)
set_target_properties(
  linuxapk_bridge_client
  PROPERTIES
    C_STANDARD 17 C_VISIBILITY_PRESET hidden POSITION_INDEPENDENT_CODE ON)
target_include_directories(linuxapk_bridge_client PUBLIC include)
target_compile_options(linuxapk_bridge_client PRIVATE -Wall -Wextra -Werror)

if(NOT ANDROID)
  add_executable(bridge_client_test tests/framing_test.c)
  target_link_libraries(bridge_client_test PRIVATE linuxapk_bridge_client)
  add_test(NAME bridge_client COMMAND bridge_client_test)
endif()
