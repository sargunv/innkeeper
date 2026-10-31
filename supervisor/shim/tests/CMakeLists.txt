# SPDX-License-Identifier: AGPL-3.0-or-later
# Host unit tests of the shim's headers (preset "host-tests"). They check with
# assert(), so NDEBUG stays off in every build type.
foreach(name fuse netlink oom selinux)
  add_executable(shim-${name}-test ${name}.c)
  target_compile_options(shim-${name}-test PRIVATE -Wall -Werror -UNDEBUG)
endforeach()
# A stand-in libselinux whose file labels live in a user xattr.
add_library(shim-test-labels SHARED selinux-labels.c)
set_target_properties(shim-test-labels PROPERTIES OUTPUT_NAME labels)
target_compile_options(shim-test-labels PRIVATE -Wall -Werror)
target_link_libraries(shim-selinux-test PRIVATE ${CMAKE_DL_LIBS})
add_dependencies(shim-selinux-test shim-test-labels)

add_test(NAME shim-fuse COMMAND shim-fuse-test)
add_test(NAME shim-netlink COMMAND shim-netlink-test)
add_test(NAME shim-oom COMMAND shim-oom-test)
add_test(
  NAME shim-selinux
  COMMAND shim-selinux-test $<TARGET_FILE_DIR:shim-test-labels>)
