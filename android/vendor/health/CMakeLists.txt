# SPDX-License-Identifier: AGPL-3.0-or-later
# The Health HAL (IHealth AIDL V5), part of the root superbuild. With the NDK:
# the vendor service. Natively (preset "host-tests"): the BATTERY event
# decoder and its HealthInfo mapping.
set(HEALTH ${CMAKE_CURRENT_SOURCE_DIR})
set(GRAPHICS ${CMAKE_SOURCE_DIR}/android/vendor/graphics)

add_library(innkeeper_health_battery STATIC src/battery.c)
target_include_directories(innkeeper_health_battery PUBLIC src)
set_target_properties(
  innkeeper_health_battery
  PROPERTIES C_STANDARD 17 POSITION_INDEPENDENT_CODE ON)
target_compile_options(innkeeper_health_battery PRIVATE -Wall -Wextra -Werror)

if(ANDROID)
  # The IHealth V5 backend, linked against the graphics HALs' libbinder_ndk
  # stub.
  set(HEALTH_AIDL_GEN ${CMAKE_CURRENT_BINARY_DIR}/aidl)
  innkeeper_gen_aidl(${HEALTH_AIDL_GEN} android.hardware.health)
  file(GLOB_RECURSE HEALTH_AIDL_SRCS ${HEALTH_AIDL_GEN}/src/*.cpp)

  add_executable(health src/main.cpp ${HEALTH_AIDL_SRCS})
  set_target_properties(
    health
    PROPERTIES
      OUTPUT_NAME android.hardware.health-service.innkeeper CXX_STANDARD 20)
  target_include_directories(
    health
    PRIVATE
      ${HEALTH_AIDL_GEN}/include ${AOSP}/include ${GRAPHICS}/common/include)
  target_link_libraries(
    health
    PRIVATE innkeeper_health_battery innkeeper_bridge_client binder_ndk_stub log)

  set(VENDOR ${INNKEEPER_ANDROID}/vendor)
  install(TARGETS health RUNTIME DESTINATION ${VENDOR}/bin/hw)
  install(
    FILES etc/init/android.hardware.health-service.innkeeper.rc
    DESTINATION ${VENDOR}/etc/init)
  install(
    FILES etc/vintf/android.hardware.health-service.innkeeper.xml
    DESTINATION ${VENDOR}/etc/vintf/manifest)
else()
  add_executable(health_battery_test tests/battery_test.c)
  target_link_libraries(health_battery_test PRIVATE innkeeper_health_battery)
  add_test(NAME health_battery COMMAND health_battery_test)
endif()
