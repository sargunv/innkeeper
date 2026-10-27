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
#include <grp.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include "bpfemu.h"

#ifndef AUDIT_ARCH_X86_64
#define AUDIT_ARCH_X86_64 0xc000003e
#endif

extern char** environ;

// Path to the Rust ubinder broker helper (ubinder/src/bin/broker.rs), or NULL.
// When set (LINUXAPK_UBINDER), the child installs a second seccomp filter and
// the parent forks+execs the helper to serve the binder listener in userspace.
// The helper is a normal dynamic bionic binary, so pivot-exec itself stays a
// pure-C static binary (it is exec'd before pivot_root, where no linker
// exists). Read in main() before clone(); the child sees a copy.
static const char* ubinder_helper;
static int ubinder_enabled;

static void die(const char* what);

// Second seccomp filter: trap openat (to inject the per-process binder memfd)
// and binder ioctls (ioctl cmd type byte == 'b'). Disjoint from bpfemu's
// filter, so the two listeners coexist. Returns the notification listener fd.
static int ubinder_install_filter(void) {
  struct sock_filter f[] = {
    /* 0 */ BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)
    ),
    /* 1 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 0, 7),
    /* 2 */
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
    /* 3 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_openat, 4, 0),
    /* 4 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_ioctl, 0, 4),
    /* 5 */
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])),
    /* 6 */ BPF_STMT(BPF_ALU | BPF_AND | BPF_K, 0xff00),
    /* 7 */ BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, ('b' << 8), 0, 1),
    /* 8 */ BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF),
    /* 9 */ BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
  };
  struct sock_fprog prog = {.len = sizeof(f) / sizeof(f[0]), .filter = f};
  return syscall(
    SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_NEW_LISTENER,
    &prog
  );
}

// Fork+exec the Rust broker helper to serve the binder listener. Runs in the
// parent's (outer) namespaces, like the bpf thread, so it sees Android pids and
// can process_vm_* them. The helper is dynamic bionic; the parent is in the
// pivoted Android root, so /system/bin/linker64 and libc.so resolve.
static void spawn_ubinder_helper(int listener) {
  pid_t p = fork();
  if (p < 0) die("fork ubinder helper");
  if (p != 0) return;
  // Child: the listener was received CLOEXEC; clear it so it survives exec.
  fcntl(listener, F_SETFD, 0);
  char fdbuf[16];
  snprintf(fdbuf, sizeof(fdbuf), "%d", listener);
  char* argv[] = {(char*)ubinder_helper, fdbuf, NULL};
  execve(ubinder_helper, argv, environ);
  die("exec ubinder helper");
}

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
  for (char* line = strtok_r(buf, "\n", &save); line;
       line = strtok_r(NULL, "\n", &save)) {
    unsigned long in, outer, count;
    if (sscanf(line, "%lu %lu %lu", &in, &outer, &count) != 3) continue;
    len +=
      snprintf(out + len, sizeof(out) - len, "%lu %lu %lu\n", in, in, count);
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
  if (
    mount("proc", "/proc", "proc", MS_NOSUID | MS_NODEV | MS_NOEXEC, NULL) != 0
  )
    die("mount /proc");
  bind_if_present("/linuxapk/proc/cmdline", "/proc/cmdline");
  bind_if_present("/linuxapk/proc/bootconfig", "/proc/bootconfig");
  bind_if_present(
    "/linuxapk/proc/sys/vm/mmap_rnd_bits", "/proc/sys/vm/mmap_rnd_bits"
  );

  // Route every bpf() call of Android (inherited by all descendants) to the
  // emulator in our parent (see bpfemu.c); with LINUXAPK_UBINDER, add a second
  // filter that routes openat/binder-ioctl to the Rust broker (ubinder_serve).
  // Both listeners are passed to the parent in one SCM_RIGHTS array.
  int listeners[2];
  int nlisteners = 0;
  listeners[nlisteners] = bpfemu_install_filter();
  if (listeners[nlisteners] < 0) die("install bpf seccomp filter");
  nlisteners++;
  if (ubinder_enabled) {
    listeners[nlisteners] = ubinder_install_filter();
    if (listeners[nlisteners] < 0) die("install binder seccomp filter");
    nlisteners++;
  }
  char cbuf[CMSG_SPACE(2 * sizeof(int))];
  memset(cbuf, 0, sizeof(cbuf));
  char label[2] = {(char)('0' + nlisteners), 0};
  struct iovec iov = {.iov_base = label, .iov_len = 1};
  struct msghdr msg = {
    .msg_iov = &iov,
    .msg_iovlen = 1,
    .msg_control = cbuf,
    .msg_controllen = CMSG_SPACE(nlisteners * sizeof(int))
  };
  struct cmsghdr* cm = CMSG_FIRSTHDR(&msg);
  cm->cmsg_level = SOL_SOCKET;
  cm->cmsg_type = SCM_RIGHTS;
  cm->cmsg_len = CMSG_LEN(nlisteners * sizeof(int));
  memcpy(CMSG_DATA(cm), listeners, nlisteners * sizeof(int));
  if (sendmsg(sync_sock[0], &msg, 0) != 1) die("send seccomp listener");
  for (int i = 0; i < nlisteners; i++) close(listeners[i]);
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
  ubinder_helper = getenv("LINUXAPK_UBINDER");
  ubinder_enabled = ubinder_helper != NULL && ubinder_helper[0] != '\0';

  if (chdir(argv[1]) != 0) die("chdir newroot");
  // pivot_root(".", "."): the old root ends up stacked on top of the new
  // one; detaching "." then removes it.
  if (syscall(SYS_pivot_root, ".", ".") != 0) die("pivot_root");
  if (umount2(".", MNT_DETACH) != 0) die("umount old root");
  if (chdir("/") != 0) die("chdir /");

  // Drop the host user's supplementary groups (wheel etc.): they are
  // unmapped here but would still grant access to host files.
  if (setgroups(0, NULL) != 0) die("setgroups");

  if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sync_sock) != 0)
    die("socketpair");
  static char stack[256 * 1024] __attribute__((aligned(16)));
  int flags = CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWCGROUP |
              CLONE_NEWNET | CLONE_NEWIPC | CLONE_NEWUTS | SIGCHLD;
  pid_t pid = clone(child_main, stack + sizeof(stack), flags, NULL);
  if (pid < 0) die("clone");
  close(sync_sock[0]);
  write_identity_map(pid, "uid_map");
  write_identity_map(pid, "gid_map");
  if (write(sync_sock[1], "x", 1) != 1) die("release child");

  // Receive the child's seccomp listeners: [0] = bpf, [1] = binder (optional).
  char c, cbuf[CMSG_SPACE(2 * sizeof(int))];
  struct iovec iov = {.iov_base = &c, .iov_len = 1};
  struct msghdr msg = {
    .msg_iov = &iov,
    .msg_iovlen = 1,
    .msg_control = cbuf,
    .msg_controllen = sizeof(cbuf)
  };
  if (recvmsg(sync_sock[1], &msg, MSG_CMSG_CLOEXEC) != 1)
    die("receive seccomp listener");
  struct cmsghdr* cm = CMSG_FIRSTHDR(&msg);
  if (!cm || cm->cmsg_type != SCM_RIGHTS) die("no seccomp listener");
  int nlisteners = (cm->cmsg_len - CMSG_LEN(0)) / sizeof(int);
  int listeners[2] = {-1, -1};
  memcpy(listeners, CMSG_DATA(cm), nlisteners * sizeof(int));
  close(sync_sock[1]);
  pthread_t t;
  if (
    pthread_create(&t, NULL, bpfemu_serve, (void*)(intptr_t)listeners[0]) != 0
  )
    die("bpfemu thread");
  if (nlisteners > 1) spawn_ubinder_helper(listeners[1]);

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
