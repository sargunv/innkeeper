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
//  3. The child installs the seccomp filter (bpfemu.c). The parent forks the
//     binder broker (LINUXAPK_UBINDER), which receives every notification,
//     serves binder and forwards the rest, such as bpf(2), to an emulator
//     thread (real eBPF needs privileges).
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
#include <sched.h>
#include <signal.h>
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

extern char** environ;

// Path to the Rust binder broker (ubinder/src/bin/broker.rs), from
// LINUXAPK_UBINDER. The kernel allows only one notification listener per
// process, so bpfemu installs one seccomp filter for bpf, openat and binder
// ioctls, and the broker receives every notification, serves the binder ones
// and forwards the rest to the bpf thread. The broker is a normal dynamic
// bionic binary, so pivot-exec stays a pure-C static binary (it is exec'd
// before pivot_root, where no linker exists).
static const char* broker;
// Single-uid mode (LINUXAPK_SINGLE_UID=1): the session maps only uid/gid 0,
// and the broker emulates Android's uids (docs/sandbox.md).
static int single_uid;

static void die(const char* what);

// Fork+exec the broker, which receives notifications on `listener` and
// forwards non-binder ones to `sock`. Runs in the parent's (outer) namespaces
// like the bpf thread, so it sees Android pids and can process_vm_* them.
// Dynamic bionic; the parent is in the pivoted Android root, so
// /system/bin/linker64 and libc.so resolve.
static void spawn_broker(int sock, int listener) {
  pid_t p = fork();
  if (p < 0) die("fork broker");
  if (p != 0) return;
  // Inherited fds must survive exec; clear CLOEXEC.
  fcntl(sock, F_SETFD, 0);
  fcntl(listener, F_SETFD, 0);
  char sockbuf[16], fdbuf[16];
  snprintf(sockbuf, sizeof(sockbuf), "%d", sock);
  snprintf(fdbuf, sizeof(fdbuf), "%d", listener);
  char* argv[] = {(char*)broker, sockbuf, fdbuf, NULL};
  execve(broker, argv, environ);
  die("exec broker");
}

// Is setgroups(2) disabled in this user namespace (/proc/self/setgroups)?
static int setgroups_denied(void) {
  char buf[16] = {0};
  int fd = open("/proc/self/setgroups", O_RDONLY | O_CLOEXEC);
  if (fd < 0) return 0;
  ssize_t n = read(fd, buf, sizeof(buf) - 1);
  close(fd);
  return n > 0 && strncmp(buf, "deny", 4) == 0;
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

  // Route Android's bpf(), openat and binder ioctls to the supervisor via one
  // seccomp notification listener, then hand it the fd.
  int listener = bpfemu_install_filter(single_uid);
  if (listener < 0) die("install seccomp filter");
  char cbuf[CMSG_SPACE(sizeof(int))];
  memset(cbuf, 0, sizeof(cbuf));
  struct iovec iov = {.iov_base = "L", .iov_len = 1};
  struct msghdr msg = {
    .msg_iov = &iov,
    .msg_iovlen = 1,
    .msg_control = cbuf,
    .msg_controllen = sizeof(cbuf)
  };
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
  broker = getenv("LINUXAPK_UBINDER");
  if (broker == NULL || broker[0] == '\0') {
    fprintf(stderr, "pivot-exec: LINUXAPK_UBINDER (the broker) is not set\n");
    return 2;
  }
  const char* su = getenv("LINUXAPK_SINGLE_UID");
  single_uid = su != NULL && strcmp(su, "1") == 0;

  if (chdir(argv[1]) != 0) die("chdir newroot");
  // pivot_root(".", "."): the old root ends up stacked on top of the new
  // one; detaching "." then removes it.
  if (syscall(SYS_pivot_root, ".", ".") != 0) die("pivot_root");
  if (umount2(".", MNT_DETACH) != 0) die("umount old root");
  if (chdir("/") != 0) die("chdir /");

  // Drop the host user's supplementary groups (wheel etc.): they are
  // unmapped here but would still grant access to host files.
  // (Test harnesses run under `unshare --map-root-user`, where setgroups is
  // denied; their supplementary groups are already unmapped.)
  if (setgroups(0, NULL) != 0 && !setgroups_denied()) die("setgroups");

  // The session's bpffs tmpfs (ns-init.sh), for the bpf emulator's pins:
  // opened now, before Android can mount anything over it.
  int pins = open("/sys/fs/bpf", O_PATH | O_DIRECTORY | O_CLOEXEC);
  if (pins < 0)
    fprintf(stderr, "pivot-exec: /sys/fs/bpf: %s\n", strerror(errno));
  bpfemu_set_pin_dir(pins);

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

  // Receive the child's single seccomp listener.
  char c, cbuf[CMSG_SPACE(sizeof(int))];
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
  int listener;
  memcpy(&listener, CMSG_DATA(cm), sizeof(int));
  close(sync_sock[1]);

  // The broker receives all notifications and forwards the bpf ones to the
  // serve thread over a SOCK_SEQPACKET pair.
  int sp[2];
  if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sp) != 0)
    die("broker socketpair");
  spawn_broker(sp[1], listener);
  close(sp[1]);
  if (bpfemu_start(listener, sp[0]) != 0) die("bpfemu thread");

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
