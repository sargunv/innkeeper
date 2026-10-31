// The libselinux file-label ABI over an owned test xattr. This permits both
// absent and present labels without root or a host libselinux installation.
#include <stdlib.h>
#include <sys/xattr.h>

static int filecon(const char* path, char** context, int nofollow) {
  ssize_t (*get)(const char*, const char*, void*, size_t) =
    nofollow ? lgetxattr : getxattr;
  ssize_t n = get(path, "user.innkeeper.test.selinux", NULL, 0);
  if (n < 0) return -1;
  char* label = malloc(n + 1);
  if (!label) return -1;
  n = get(path, "user.innkeeper.test.selinux", label, n);
  if (n < 0) {
    free(label);
    return -1;
  }
  label[n] = 0;
  *context = label;
  return n;
}

int getfilecon_raw(const char* path, char** context) {
  return filecon(path, context, 0);
}

int getfilecon(const char* path, char** context) {
  return getfilecon_raw(path, context);
}

int lgetfilecon_raw(const char* path, char** context) {
  return filecon(path, context, 1);
}

int lgetfilecon(const char* path, char** context) {
  return lgetfilecon_raw(path, context);
}
