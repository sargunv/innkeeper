// The libselinux file-label ABI over an owned test xattr. This permits both
// absent and present labels without root or a host libselinux installation.
#include <stdlib.h>
#include <sys/xattr.h>

int getfilecon_raw(const char* path, char** context) {
  ssize_t n = getxattr(path, "user.linuxapk.test.selinux", NULL, 0);
  if (n < 0) return -1;
  char* label = malloc(n + 1);
  if (!label) return -1;
  n = getxattr(path, "user.linuxapk.test.selinux", label, n);
  if (n < 0) {
    free(label);
    return -1;
  }
  label[n] = 0;
  *context = label;
  return n;
}

int getfilecon(const char* path, char** context) {
  return getfilecon_raw(path, context);
}
