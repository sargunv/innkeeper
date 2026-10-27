// pivot-exec NEWROOT PROG [ARGS...]
//
// Final step of the session launcher (runs as pid 1 of the session's outer
// user/mount/pid namespaces, after ns-init.sh assembled NEWROOT):
//
//  1. Make NEWROOT the root and detach the old (host) root.
//  2. Clone a child into nested user, mount, pid, cgroup, network, IPC and
//     UTS namespaces. The parent writes the child's uid/gid maps (identity
//     over the outer namespace's ranges), the bubblewrap technique: because
//     the child's mount namespace is owned by a less privileged user
//     namespace, every mount it inherits is MNT_LOCKED, so Android's root
//     cannot unmount the masks over host sysfs/proc, and read-only/nosuid/
//     nodev/noexec flags on inherited mounts can't be cleared.
//  3. The child installs a seccomp filter that routes bpf(2) to an emulator
//     thread in the parent (bpfemu.c), since real eBPF needs privileges.
//  4. The child (pid 1 of the nested pid namespace) mounts a fresh /proc,
//     binds the fake files staged under /linuxapk/proc over it, and execs
//     PROG. The parent stays as the outer pid 1 and exits with the child's
//     status, so the whole session ends when Android init does.
//
// Built static with the NDK so it keeps working after the host root is gone.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <grp.h>
#include <pthread.h>
#include <sys/socket.h>
#include <unistd.h>

#include "bpfemu.h"

extern char** environ;

static char** child_argv;
static int sync_sock[2];  // [0] child end, [1] parent end

static void die(const char* what) {
    fprintf(stderr, "pivot-exec: %s: %s\n", what, strerror(errno));
    _exit(1);
}

// Write "<in> <in> <count>" for each line of our own /proc/self/<map>: an
// identity map over everything the outer namespace maps.
static void write_identity_map(pid_t pid, const char* map) {
    char path[64], buf[1024], out[1024];
    snprintf(path, sizeof(path), "/proc/self/%s", map);
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) die("open own id map");
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) die("read own id map");
    buf[n] = 0;
    size_t len = 0;
    char* save = NULL;
    for (char* line = strtok_r(buf, "\n", &save); line; line = strtok_r(NULL, "\n", &save)) {
        unsigned long in, outer, count;
        if (sscanf(line, "%lu %lu %lu", &in, &outer, &count) != 3) continue;
        len += snprintf(out + len, sizeof(out) - len, "%lu %lu %lu\n", in, in, count);
    }
    snprintf(path, sizeof(path), "/proc/%d/%s", pid, map);
    fd = open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0) die("open child id map");
    if (write(fd, out, len) != (ssize_t)len) die("write child id map");
    close(fd);
}

static void bind_if_present(const char* src, const char* dst) {
    struct stat st;
    if (stat(src, &st) != 0 || stat(dst, &st) != 0) return;
    if (mount(src, dst, NULL, MS_BIND, NULL) != 0) die(dst);
}

static int child_main(void* arg) {
    (void)arg;
    char c;
    close(sync_sock[1]);
    if (read(sync_sock[0], &c, 1) != 1) die("wait for id maps");

    // Fresh procfs for the nested pid namespace, then the fake entries.
    if (mount("proc", "/proc", "proc", MS_NOSUID | MS_NODEV | MS_NOEXEC, NULL) != 0)
        die("mount /proc");
    bind_if_present("/linuxapk/proc/cmdline", "/proc/cmdline");
    bind_if_present("/linuxapk/proc/bootconfig", "/proc/bootconfig");
    bind_if_present("/linuxapk/proc/sys/vm/mmap_rnd_bits", "/proc/sys/vm/mmap_rnd_bits");

    // Route every bpf() call of Android (inherited by all descendants) to the
    // emulator in our parent (see bpfemu.c), then hand it the listener.
    int listener = bpfemu_install_filter();
    if (listener < 0) die("install bpf seccomp filter");
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof(cbuf));
    struct iovec iov = {.iov_base = "L", .iov_len = 1};
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = cbuf,
                         .msg_controllen = sizeof(cbuf)};
    struct cmsghdr* cm = CMSG_FIRSTHDR(&msg);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &listener, sizeof(int));
    if (sendmsg(sync_sock[0], &msg, 0) != 1) die("send seccomp listener");
    close(listener);
    close(sync_sock[0]);

    execve(child_argv[0], child_argv, environ);
    die("execve");
    return 1;
}

int main(int argc, char** argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: pivot-exec NEWROOT PROG [ARGS...]\n");
        return 2;
    }
    child_argv = argv + 2;

    if (chdir(argv[1]) != 0) die("chdir newroot");
    // pivot_root(".", "."): the old root ends up stacked on top of the new
    // one; detaching "." then removes it.
    if (syscall(SYS_pivot_root, ".", ".") != 0) die("pivot_root");
    if (umount2(".", MNT_DETACH) != 0) die("umount old root");
    if (chdir("/") != 0) die("chdir /");

    // Drop the host user's supplementary groups (wheel etc.): they are
    // unmapped here but would still grant access to host files.
    if (setgroups(0, NULL) != 0) die("setgroups");

    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sync_sock) != 0) die("socketpair");
    static char stack[256 * 1024] __attribute__((aligned(16)));
    int flags = CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWCGROUP | CLONE_NEWNET |
                CLONE_NEWIPC | CLONE_NEWUTS | SIGCHLD;
    pid_t pid = clone(child_main, stack + sizeof(stack), flags, NULL);
    if (pid < 0) die("clone");
    close(sync_sock[0]);
    write_identity_map(pid, "uid_map");
    write_identity_map(pid, "gid_map");
    if (write(sync_sock[1], "x", 1) != 1) die("release child");

    // Receive the child's seccomp listener and serve bpf() on a thread.
    char c, cbuf[CMSG_SPACE(sizeof(int))];
    struct iovec iov = {.iov_base = &c, .iov_len = 1};
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = cbuf,
                         .msg_controllen = sizeof(cbuf)};
    if (recvmsg(sync_sock[1], &msg, MSG_CMSG_CLOEXEC) != 1) die("receive seccomp listener");
    struct cmsghdr* cm = CMSG_FIRSTHDR(&msg);
    if (!cm || cm->cmsg_type != SCM_RIGHTS) die("no seccomp listener");
    int listener;
    memcpy(&listener, CMSG_DATA(cm), sizeof(int));
    close(sync_sock[1]);
    pthread_t t;
    if (pthread_create(&t, NULL, bpfemu_serve, (void*)(intptr_t)listener) != 0) die("bpfemu thread");

    // We are pid 1 of the outer pid namespace: reap until the child is gone.
    int status;
    for (;;) {
        pid_t w = waitpid(-1, &status, 0);
        if (w == pid) break;
        if (w < 0 && errno != EINTR) die("waitpid");
    }
    if (WIFEXITED(status)) return WEXITSTATUS(status);
    return 128 + WTERMSIG(status);
}
