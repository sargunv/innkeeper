// pivot-exec NEWROOT PROG [ARGS...]
//
// The session runs inside Flatpak (see below). The namespace path
// is retained only for the bootless bionic test harness and kernel binder
// oracle, which assemble NEWROOT in tests/bionic/lib.sh.
//
// In those developer harnesses, pivot to NEWROOT, clone nested namespaces
// with locked mounts, and install the seccomp filter before executing PROG.
// The parent serves notifications through the Rust broker.
//
// Built static with the NDK so it keeps working after the host root is gone.
//
// Flatpak runtime (NEWROOT "/"): the sandbox forbids mounts
// and namespaces, and the launcher already built Android's root out of
// symlinks in the sandbox's own "/". Steps 1 and 2 are skipped: init is a
// plain child, and this process becomes its subreaper instead of pid 1. The
// broker fakes the /proc entries step 4 would bind.
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
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

extern char** environ;

// The static bootstrap runs before Android's linker exists. After pivot it
// execs the dynamic Rust broker, which owns the entire syscall boundary.
static const char* broker;
// Flatpak mode (see the header).
static int flat;

static void die(const char* what);

// The broker stays in the outer namespaces to see every Android pid.
static void spawn_broker(int pins, int listener, pid_t init) {
  pid_t p = fork();
  if (p < 0) die("fork broker");
  if (p != 0) return;
  // Flatpak mode: init's pid, which the broker shows as 1.
  if (flat) {
    char pid[16];
    snprintf(pid, sizeof(pid), "%d", init);
    setenv("LINUXAPK_INIT_PID", pid, 1);
  }
  // Inherited fds must survive exec; clear CLOEXEC.
  if (pins >= 0) fcntl(pins, F_SETFD, 0);
  fcntl(listener, F_SETFD, 0);
  char pinbuf[16], fdbuf[16];
  snprintf(pinbuf, sizeof(pinbuf), "%d", pins);
  snprintf(fdbuf, sizeof(fdbuf), "%d", listener);
  char* argv[] = {(char*)broker, fdbuf, pinbuf, NULL};
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

  if (!flat) {
    // Fresh procfs for the nested pid namespace, then the fake entries.
    if (
      mount("proc", "/proc", "proc", MS_NOSUID | MS_NODEV | MS_NOEXEC, NULL) !=
      0
    )
      die("mount /proc");
    bind_if_present("/linuxapk/proc/cmdline", "/proc/cmdline");
    bind_if_present("/linuxapk/proc/bootconfig", "/proc/bootconfig");
    bind_if_present(
      "/linuxapk/proc/sys/vm/mmap_rnd_bits", "/proc/sys/vm/mmap_rnd_bits"
    );
  } else {
    // The shim's cue that this process is init (it isn't pid 1 here): init
    // keeps the shim, its services don't inherit it.
    setenv("LINUXAPK_SHIM_INIT", "1", 1);
  }

  // Rust installs the filter, hands its listener to our parent, then execs
  // Android. No syscall policy or emulator runs in this static bootstrap.
  fcntl(sync_sock[0], F_SETFD, 0);
  char syncbuf[16];
  snprintf(syncbuf, sizeof(syncbuf), "%d", sync_sock[0]);
  int n = 0;
  while (child_argv[n]) n++;
  char** args = calloc(n + 4, sizeof(char*));
  if (!args) die("calloc broker argv");
  args[0] = (char*)broker;
  args[1] = "--init";
  args[2] = syncbuf;
  memcpy(args + 3, child_argv, (n + 1) * sizeof(char*));
  // Init's shim consumes LD_PRELOAD (and its init marker) in its constructor.
  // Do not load it into this intermediate Rust process: restore the preload
  // only in the environment Rust hands to the final Android exec.
  const char* preload = getenv("LD_PRELOAD");
  if (preload) {
    if (setenv("LINUXAPK_INIT_PRELOAD", preload, 1) != 0) die("save preload");
    unsetenv("LD_PRELOAD");
  }
  execve(broker, args, environ);
  die("execve");
  return 1;
}

int main(int argc, char** argv) {
  if (argc < 3) {
    fprintf(stderr, "usage: pivot-exec NEWROOT PROG [ARGS...]\n");
    return 2;
  }
  child_argv = argv + 2;
  broker = getenv("LINUXAPK_BROKER");
  if (broker == NULL || broker[0] == '\0') {
    fprintf(stderr, "pivot-exec: LINUXAPK_BROKER (the broker) is not set\n");
    return 2;
  }
  const char* harness = getenv("LINUXAPK_BIONIC_HARNESS");
  flat = !(harness && strcmp(harness, "1") == 0);

  if (flat) {
    if (strcmp(argv[1], "/") != 0) {
      fprintf(stderr, "pivot-exec: Flatpak runtime needs NEWROOT /\n");
      return 2;
    }
    if (chdir("/") != 0) die("chdir /");
    // Orphaned Android processes come back to us, not the sandbox's pid 1.
    if (prctl(PR_SET_CHILD_SUBREAPER, 1) != 0) die("subreaper");
  } else {
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
  }

  // The session's bpffs tmpfs (the launcher's), for the bpf emulator's pins:
  // opened now, before Android can mount anything over it.
  int pins = open("/sys/fs/bpf", O_PATH | O_DIRECTORY | O_CLOEXEC);
  if (pins < 0)
    fprintf(stderr, "pivot-exec: /sys/fs/bpf: %s\n", strerror(errno));

  if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sync_sock) != 0)
    die("socketpair");
  static char stack[256 * 1024] __attribute__((aligned(16)));
  int flags = CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWCGROUP |
              CLONE_NEWNET | CLONE_NEWIPC | CLONE_NEWUTS | SIGCHLD;
  if (flat) flags = SIGCHLD;
  pid_t pid = clone(child_main, stack + sizeof(stack), flags, NULL);
  if (pid < 0) die("clone");
  close(sync_sock[0]);
  if (!flat) {
    write_identity_map(pid, "uid_map");
    write_identity_map(pid, "gid_map");
  }
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

  spawn_broker(pins, listener, pid);
  close(listener);
  if (pins >= 0) close(pins);

  // We are pid 1 of the outer pid namespace (Flatpak mode: init's
  // subreaper): reap until the child is gone.
  int status;
  for (;;) {
    pid_t w = waitpid(-1, &status, 0);
    if (w == pid) break;
    if (w < 0 && errno != EINTR) die("waitpid");
  }
  if (WIFEXITED(status)) return WEXITSTATUS(status);
  return 128 + WTERMSIG(status);
}
