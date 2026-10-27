// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <cstdint>
#include <string>

namespace linuxapk::hwc {

// System property, or `def` if unset (always on the host).
std::string getProperty(const char* name, const std::string& def = "");

}  // namespace linuxapk::hwc
