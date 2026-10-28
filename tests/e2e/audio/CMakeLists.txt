# SPDX-License-Identifier: AGPL-3.0-or-later
# AAudio tone and latency probe that run.sh pushes into the session.
add_executable(linuxapk-tone tone.c)
target_link_libraries(linuxapk-tone PRIVATE aaudio m)
target_compile_options(linuxapk-tone PRIVATE -Wall -Wextra -Werror)
