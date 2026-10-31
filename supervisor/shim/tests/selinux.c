#define _GNU_SOURCE
#include <assert.h>
#include <stdio.h>
#include <sys/xattr.h>
#include <unistd.h>

#define EXPORT
#include "../selinux-file.h"

int main(int argc, char** argv) {
  assert(argc == 2);
  char path[4096];
  assert(snprintf(path, sizeof(path), "%s/liblabels.so", argv[1]) > 0);
  void* lib = dlopen(path, RTLD_NOW | RTLD_GLOBAL);
  assert(lib);
  assert(snprintf(path, sizeof(path), "%s/file", argv[1]) > 0);
  FILE* file = fopen(path, "w");
  assert(file);
  assert(fclose(file) == 0);
  int (*getters[])(const char*, char**) = {
    getfilecon, getfilecon_raw, lgetfilecon, lgetfilecon_raw
  };
  const char* names[] = {
    "getfilecon", "getfilecon_raw", "lgetfilecon", "lgetfilecon_raw"
  };
  for (unsigned i = 0; i < 4; i++) {
    int (*native)(const char*, char**) = dlsym(lib, names[i]);
    assert(native);
    char* context = NULL;
    assert(native(path, &context) == -1 && errno == ENODATA);
    assert(getters[i](path, &context) > 0);
    assert(strcmp("u:object_r:innkeeper_exec:s0", context) == 0);
    free(context);
    const char label[] = "u:object_r:test_exec:s0";
    assert(
      setxattr(path, "user.innkeeper.test.selinux", label, sizeof(label), 0) ==
      0
    );
    assert(getters[i](path, &context) > 0);
    assert(strcmp(label, context) == 0);
    free(context);
    assert(removexattr(path, "user.innkeeper.test.selinux") == 0);
    context = NULL;
    errno = 0;
    assert(getters[i]("/innkeeper-nonexistent-label-test", &context) == -1);
    assert(errno == ENOENT);
    printf(
      "ok   %s: unlabeled fallback; real label and ENOENT preserved\n", names[i]
    );
  }
  // The lget variants must inspect the link, not inherit its target's label.
  const char label[] = "u:object_r:test_target:s0";
  assert(
    setxattr(path, "user.innkeeper.test.selinux", label, sizeof(label), 0) == 0
  );
  char link[4096];
  assert(snprintf(link, sizeof(link), "%s/link", argv[1]) > 0);
  assert(symlink("file", link) == 0);
  for (unsigned i = 0; i < 4; i++) {
    char* context = NULL;
    assert(getters[i](link, &context) > 0);
    assert(
      strcmp(i < 2 ? label : "u:object_r:innkeeper_exec:s0", context) == 0
    );
    free(context);
  }
  assert(unlink(link) == 0);
  assert(unlink(path) == 0);
  dlclose(lib);
}
