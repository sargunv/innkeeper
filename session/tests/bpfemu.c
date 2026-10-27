// The bpf() emulator (helper/bpfemu.c) against hostile callers. Built and
// run by bpfemu.sh inside a private user and mount namespace whose
// /sys/fs/bpf is a tmpfs, like the session's. A child installs the session's
// filter and makes bpf() calls; the parent serves them with bpfemu_serve, as
// pivot-exec does, and must survive each one.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/bpf.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include "../helper/bpfemu.h"

static int fails;
#define CHECK(cond, ...)           \
  do {                             \
    if (cond) {                    \
      printf("ok   " __VA_ARGS__); \
    } else {                       \
      printf("FAIL " __VA_ARGS__); \
      fails++;                     \
    }                              \
    printf("\n");                  \
    fflush(stdout);                \
  } while (0)

// bpf(): the result, or -errno.
static long bpf(int cmd, union bpf_attr* a) {
  long r = syscall(__NR_bpf, cmd, a, sizeof(*a));
  return r < 0 ? -errno : r;
}

static long map_create(
  uint32_t type, uint32_t key, uint32_t val, uint32_t max
) {
  union bpf_attr a = {0};
  a.map_type = type;
  a.key_size = key;
  a.value_size = val;
  a.max_entries = max;
  return bpf(BPF_MAP_CREATE, &a);
}

static long update(int fd, const void* key, const void* val) {
  union bpf_attr a = {0};
  a.map_fd = fd;
  a.key = (uintptr_t)key;
  a.value = (uintptr_t)val;
  return bpf(BPF_MAP_UPDATE_ELEM, &a);
}

static long lookup(int fd, const void* key, void* val) {
  union bpf_attr a = {0};
  a.map_fd = fd;
  a.key = (uintptr_t)key;
  a.value = (uintptr_t)val;
  return bpf(BPF_MAP_LOOKUP_ELEM, &a);
}

static long pin(int fd, const char* path) {
  union bpf_attr a = {0};
  a.bpf_fd = fd;
  a.pathname = (uintptr_t)path;
  return bpf(BPF_OBJ_PIN, &a);
}

static long get(const char* path) {
  union bpf_attr a = {0};
  a.pathname = (uintptr_t)path;
  return bpf(BPF_OBJ_GET, &a);
}

static void tests(const char* tmp) {
  // A working map, pinned and fetched back.
  long m = map_create(BPF_MAP_TYPE_HASH, 4, 8, 16);
  uint32_t k = 7;
  uint64_t v = 42, got = 0;
  CHECK(m >= 0 && update(m, &k, &v) == 0, "hash map create and update");
  CHECK(pin(m, "/sys/fs/bpf/m") == 0, "pin under /sys/fs/bpf");
  long g = get("/sys/fs/bpf/m");
  CHECK(g >= 0 && lookup(g, &k, &got) == 0 && got == 42, "get pin and lookup");

  // S3: element operations on ring buffers (no tables).
  long ur = map_create(BPF_MAP_TYPE_USER_RINGBUF, 0, 0, 4096);
  long rb = map_create(BPF_MAP_TYPE_RINGBUF, 0, 0, 4096);
  CHECK(ur >= 0 && rb >= 0, "ring buffer create");
  CHECK(update(ur, &k, &v) == -EINVAL, "user ring buffer update: EINVAL");
  CHECK(lookup(rb, &k, &got) == -EINVAL, "ring buffer lookup: EINVAL");

  // S3: LPM prefixes must fit the key.
  CHECK(
    map_create(BPF_MAP_TYPE_LPM_TRIE, 4, 8, 16) == -EINVAL,
    "LPM key without data: EINVAL"
  );
  long lpm = map_create(BPF_MAP_TYPE_LPM_TRIE, 8, 8, 16);
  struct {
    uint32_t plen;
    uint8_t data[4];
  } key = {33, {10, 0, 0, 1}};
  CHECK(
    lpm >= 0 && update(lpm, &key, &v) == -EINVAL,
    "LPM prefix 33 in 4 bytes: EINVAL"
  );
  key.plen = 32;
  CHECK(
    update(lpm, &key, &v) == 0 && lookup(lpm, &key, &got) == 0, "LPM prefix 32"
  );

  // S6: pins are regular files under /sys/fs/bpf, opened without blocking or
  // following links out of it.
  mkfifo("/sys/fs/bpf/fifo", 0600);
  CHECK(get("/sys/fs/bpf/fifo") == -EINVAL, "get of a FIFO: EINVAL, no hang");
  char esc[512];
  snprintf(esc, sizeof(esc), "%s/escaped", tmp);
  symlink(tmp, "/sys/fs/bpf/link");
  CHECK(
    pin(m, "/sys/fs/bpf/link/escaped") == -ELOOP && access(esc, F_OK) != 0,
    "pin through a symlink: ELOOP, nothing created outside"
  );
  symlink(esc, "/sys/fs/bpf/flink");
  CHECK(get("/sys/fs/bpf/flink") == -ELOOP, "get of a symlink: ELOOP");
  CHECK(get("/sys/fs/bpf/../../../etc/passwd") == -EXDEV, "get with ..: EXDEV");
  char outside[512];
  snprintf(outside, sizeof(outside), "%s/m", tmp);
  CHECK(pin(m, outside) == -EINVAL, "pin outside /sys/fs/bpf: EINVAL");
}

int main(int argc, char** argv) {
  if (argc < 2) return fprintf(stderr, "usage: %s TMPDIR\n", argv[0]), 2;
  // Room for more objects than the cap (each holds a memfd).
  struct rlimit rl;
  getrlimit(RLIMIT_NOFILE, &rl);
  rl.rlim_cur = rl.rlim_max;
  setrlimit(RLIMIT_NOFILE, &rl);
  int sp[2];
  if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sp))
    return perror("socketpair"), 1;
  pid_t c = fork();
  if (c == 0) {
    alarm(20);
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)) return 1;
    int lfd = bpfemu_install_filter(0, 0);
    if (lfd < 0) return perror("filter"), 1;
    char ctl[CMSG_SPACE(sizeof(int))] = {0};
    char b = 0;
    struct iovec io = {&b, 1};
    struct msghdr mh = {
      .msg_iov = &io,
      .msg_iovlen = 1,
      .msg_control = ctl,
      .msg_controllen = sizeof(ctl)
    };
    struct cmsghdr* cm = CMSG_FIRSTHDR(&mh);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &lfd, sizeof(int));
    if (sendmsg(sp[1], &mh, 0) != 1) return 1;
    close(lfd);
    tests(argv[1]);
    return fails ? 1 : 0;
  }
  char ctl[CMSG_SPACE(sizeof(int))];
  char b;
  struct iovec io = {&b, 1};
  struct msghdr mh = {
    .msg_iov = &io,
    .msg_iovlen = 1,
    .msg_control = ctl,
    .msg_controllen = sizeof(ctl)
  };
  if (recvmsg(sp[0], &mh, 0) != 1 || !CMSG_FIRSTHDR(&mh))
    return fprintf(stderr, "no listener\n"), 1;
  int lfd;
  memcpy(&lfd, CMSG_DATA(CMSG_FIRSTHDR(&mh)), sizeof(int));
  pthread_t t;
  pthread_create(&t, NULL, bpfemu_serve, (void*)(intptr_t)lfd);
  // A hung or crashed supervisor fails the test (SIGALRM, SIGSEGV).
  alarm(30);
  int st;
  waitpid(c, &st, 0);
  if (!WIFEXITED(st))
    return fprintf(
             stderr, "FAIL child killed by signal %d (hang?)\n", WTERMSIG(st)
           ),
           1;
  return WEXITSTATUS(st);
}
