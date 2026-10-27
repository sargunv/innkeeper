// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <cstdint>
#include <string>

namespace linuxapk::hwc {

// System property; if unset (or on the host), the env var with dots replaced by
// underscores and upper-cased (vendor.linuxapk.x -> VENDOR_LINUXAPK_X).
std::string getProperty(const char* name, const std::string& def = "");
int64_t getIntProperty(const char* name, int64_t def);

}  // namespace linuxapk::hwc
