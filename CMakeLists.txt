# SPDX-License-Identifier: AGPL-3.0-or-later
# Every Android-side C/C++ artifact, built with the NDK pin (preset "android"),
# and the host unit tests of the exec relay, graphics and health HALs and
# the bridge client (preset "host-tests"). Each module installs as COMPONENT
# <module>, at paths relative to the stage prefix (see
# tools/build/artifacts.toml); `mise run build:<module>` installs it.
cmake_minimum_required(VERSION 3.22)
project(innkeeper C CXX)

# The stage's Android tree: one dir per partition, plus apex/<name>.
set(INNKEEPER_ANDROID share/innkeeper/android)

if(ANDROID)
  add_subdirectory(supervisor/pivot-exec)
  add_subdirectory(supervisor/shim)
  # Test-only binaries; never installed.
  add_subdirectory(tests/bionic)
else()
  enable_testing()
endif()
add_subdirectory(android/system/exec)
add_subdirectory(guest/bridge-client)
add_subdirectory(android/vendor/audio)
add_subdirectory(android/vendor/graphics)
add_subdirectory(android/vendor/health)
