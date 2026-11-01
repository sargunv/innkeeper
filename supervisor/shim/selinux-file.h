// init reads executable labels and vold_prepare_subdirs reads directory labels
// even with SELinux disabled. On hosts without SELinux, such as Ubuntu, files
// have no security.selinux xattr. These getters supply a label for our no-op
// security_compute_create and pass real labels and file errors through.
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
    static const char label[] = "u:object_r:innkeeper_exec:s0";        \
    *context = strdup(label);                                          \
    return *context ? sizeof(label) : -1;                              \
  }

FILE_CONTEXT_GETTER(getfilecon)
FILE_CONTEXT_GETTER(getfilecon_raw)
FILE_CONTEXT_GETTER(lgetfilecon)
FILE_CONTEXT_GETTER(lgetfilecon_raw)
#undef FILE_CONTEXT_GETTER
