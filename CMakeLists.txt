# SPDX-License-Identifier: AGPL-3.0-or-later
# Every Android-side C/C++ artifact, built with the NDK pin (preset "android"),
# and the host unit tests of the exec relay, graphics and health HALs, the
# bridge client and the shim's headers (preset "host-tests"). Each module
# installs as COMPONENT <module>, at paths relative to the stage prefix (see
# tools/build/artifacts.toml); `mise run build:<module>` installs it.
cmake_minimum_required(VERSION 3.22)
project(innkeeper C CXX)

# The stage's Android tree: one dir per partition, plus apex/<name>.
set(INNKEEPER_ANDROID share/innkeeper/android)

# The frozen AOSP interfaces and headers the HALs build against
# (android/vendor/tools/sync-aosp.sh).
set(AOSP ${CMAKE_SOURCE_DIR}/android/vendor/third_party/aosp)
set(VENDOR_TOOLS ${CMAKE_SOURCE_DIR}/android/vendor/tools)
# NDK-backend C++ for the frozen AIDL packages in ARGN, generated into
# <out>/{include,src} at configure time with the build-tools pin's aidl.
function(innkeeper_gen_aidl out)
  file(GLOB_RECURSE inputs CONFIGURE_DEPENDS ${AOSP}/aidl/*.aidl)
  set_property(
    DIRECTORY
    APPEND
    PROPERTY CMAKE_CONFIGURE_DEPENDS ${VENDOR_TOOLS}/gen-aidl.sh ${inputs})
  execute_process(
    COMMAND ${VENDOR_TOOLS}/gen-aidl.sh ${out} ${ARGN} OUTPUT_QUIET
    COMMAND_ERROR_IS_FATAL ANY)
endfunction()

if(ANDROID)
  add_subdirectory(supervisor/shim)
  # Test-only binaries; never installed.
  add_subdirectory(tests/bionic)
else()
  enable_testing()
  add_subdirectory(supervisor/shim/tests)
endif()
add_subdirectory(android/system/exec)
add_subdirectory(guest/bridge-client)
add_subdirectory(android/vendor/audio)
add_subdirectory(android/vendor/graphics)
add_subdirectory(android/vendor/health)
