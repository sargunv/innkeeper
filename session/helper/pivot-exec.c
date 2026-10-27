// pivot-exec NEWROOT PROG [ARGS...]
//
// Make NEWROOT the root of the current mount namespace, detach the old root,
// and exec PROG (a path inside NEWROOT) with the current environment.
// Built static with the NDK so it keeps working after the host root is gone.
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/syscall.h>
#include <unistd.h>

extern char** environ;

static int die(const char* what) {
    fprintf(stderr, "pivot-exec: %s: %s\n", what, strerror(errno));
    return 1;
}

int main(int argc, char** argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: pivot-exec NEWROOT PROG [ARGS...]\n");
        return 2;
    }
    if (chdir(argv[1]) != 0) return die("chdir newroot");
    // pivot_root(".", "."): the old root ends up stacked on top of the new
    // one; detaching "." then removes it.
    if (syscall(SYS_pivot_root, ".", ".") != 0) return die("pivot_root");
    if (umount2(".", MNT_DETACH) != 0) return die("umount old root");
    if (chdir("/") != 0) return die("chdir /");
    execve(argv[2], argv + 2, environ);
    return die("execve");
}
