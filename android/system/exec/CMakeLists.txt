# SPDX-License-Identifier: AGPL-3.0-or-later
# The exec relay: an init service, and the client `linuxapk-session exec` runs.
if(ANDROID)
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
else()
  find_package(Python3 REQUIRED COMPONENTS Interpreter)
  add_executable(exec-relay-test tests/relay.c)
  target_compile_options(exec-relay-test PRIVATE -Wall -Werror)
  add_test(
    NAME exec-relay
    COMMAND
      ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/tests/relay.py
      $<TARGET_FILE:exec-relay-test>)
  set_tests_properties(exec-relay PROPERTIES TIMEOUT 10)
endif()
