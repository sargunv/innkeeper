# SPDX-License-Identifier: AGPL-3.0-or-later
# The no-op firewall front ends netd runs (README.md in android/system).
if(ANDROID)
  install(
    PROGRAMS innkeeper-iptables iptables iptables-restore ip6tables
    ip6tables-restore
    DESTINATION ${INNKEEPER_ANDROID}/system/bin)
endif()
