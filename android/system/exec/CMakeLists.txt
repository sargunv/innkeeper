# SPDX-License-Identifier: AGPL-3.0-or-later
# The exec relay, which runs as an init service and as the client that
# `innkeeper-session exec` starts.
if(ANDROID)
  add_executable(innkeeper-exec innkeeper-exec.c)
  target_compile_options(innkeeper-exec PRIVATE -Wall -Werror)
  target_link_options(innkeeper-exec PRIVATE -static)
  install(
    TARGETS innkeeper-exec
    RUNTIME DESTINATION ${INNKEEPER_ANDROID}/system/bin)
  install(
    FILES innkeeper-exec.rc
    DESTINATION ${INNKEEPER_ANDROID}/system/etc/init)
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
