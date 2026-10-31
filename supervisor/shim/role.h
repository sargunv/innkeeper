// SPDX-License-Identifier: AGPL-3.0-or-later
// INNKEEPER_SHIM_ROLE names the one process whose workarounds the shim
// applies: a service's rc stanza sets it
// (android/overlay/system_ext/etc/init/innkeeper-services.rc), and pivot-exec
// sets "init" for init.
#pragma once
#include <stdlib.h>
#include <string.h>

static int shim_role(const char* role) {
  const char* e = getenv("INNKEEPER_SHIM_ROLE");
  return e && !strcmp(e, role);
}
