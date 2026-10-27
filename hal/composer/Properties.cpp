// SPDX-License-Identifier: AGPL-3.0-or-later
#include "Properties.h"

#if defined(__ANDROID__)
#include <sys/system_properties.h>
#endif

namespace linuxapk::hwc {

std::string getProperty(const char* name, const std::string& def) {
#if defined(__ANDROID__)
  char value[PROP_VALUE_MAX] = {};
  if (__system_property_get(name, value) > 0) return value;
#endif
  return def;
}

}  // namespace linuxapk::hwc
