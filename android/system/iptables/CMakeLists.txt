# SPDX-License-Identifier: AGPL-3.0-or-later
# No-op iptables and ip6tables front ends for netd, since Android shares the
# host's network (docs/security.md).
if(ANDROID)
  install(
    PROGRAMS innkeeper-iptables iptables iptables-restore ip6tables
    ip6tables-restore
    DESTINATION ${INNKEEPER_ANDROID}/system/bin)
endif()
