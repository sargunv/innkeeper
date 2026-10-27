// SPDX-License-Identifier: AGPL-3.0-or-later
#include "Properties.h"

#include <cctype>
#include <cstdlib>

#if defined(__ANDROID__)
#include <sys/system_properties.h>
#endif

namespace linuxapk::hwc {

std::string getProperty(const char* name, const std::string& def) {
#if defined(__ANDROID__)
    char value[PROP_VALUE_MAX] = {};
    if (__system_property_get(name, value) > 0) return value;
    return def;
#else
    std::string env;
    for (const char* p = name; *p; p++) env += *p == '.' ? '_' : char(toupper(*p));
    const char* v = getenv(env.c_str());
    return v && *v ? v : def;
#endif
}

int64_t getIntProperty(const char* name, int64_t def) {
    std::string v = getProperty(name);
    if (v.empty()) return def;
    char* end = nullptr;
    long long r = strtoll(v.c_str(), &end, 0);
    return end && *end == '\0' ? r : def;
}

}  // namespace linuxapk::hwc
