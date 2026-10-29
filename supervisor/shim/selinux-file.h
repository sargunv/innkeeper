// init reads the service executable's label even with SELinux disabled.
// Ubuntu's files have no security.selinux xattr. Supply a label for our
// no-op security_compute_create, preserving real labels and file errors.
#include <dlfcn.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>

#define FILE_CONTEXT_GETTER(name)                                      \
  EXPORT int name(const char* path, char** context) {                  \
    static int (*real)(const char*, char**);                           \
    if (!real) real = dlsym(RTLD_NEXT, #name);                         \
    if (!real) {                                                       \
      errno = ENOSYS;                                                  \
      return -1;                                                       \
    }                                                                  \
    int n = real(path, context);                                       \
    if (n >= 0 || (errno != ENODATA && errno != EOPNOTSUPP)) return n; \
    static const char label[] = "u:object_r:linuxapk_exec:s0";         \
    *context = strdup(label);                                          \
    return *context ? sizeof(label) : -1;                              \
  }

FILE_CONTEXT_GETTER(getfilecon)
FILE_CONTEXT_GETTER(getfilecon_raw)
#undef FILE_CONTEXT_GETTER
