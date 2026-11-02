// SPDX-License-Identifier: AGPL-3.0-or-later
// A test tool for the e2e check of #391. It never ships in the layer. The test
// copies it into the profile and runs it over exec with OH's shim preloaded.
//
// On a device, the user's storage views are mounts, and realpath returns a path
// in them as it is. FileManager names its files by their realpath, so other
// apps can open them only if realpath keeps /storage/Users/currentUser. The
// tool prints each argument's realpath and exits with 0 if each one equals its
// argument.
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char** argv) {
  int status = 0;
  for (int i = 1; i < argc; i++) {
    char out[PATH_MAX];
    if (!realpath(argv[i], out)) {
      perror(argv[i]);
      status = 1;
      continue;
    }
    printf("%s -> %s\n", argv[i], out);
    if (strcmp(out, argv[i]) != 0) status = 1;
  }
  return status;
}
