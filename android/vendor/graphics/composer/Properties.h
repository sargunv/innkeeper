// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <cstdint>
#include <string>

namespace innkeeper::hwc {

// Returns a system property, or `def` if it is unset. Every property is unset
// on the host.
std::string getProperty(const char* name, const std::string& def = "");

}  // namespace innkeeper::hwc
