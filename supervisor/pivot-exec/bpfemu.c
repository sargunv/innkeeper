// Userspace emulation of the bpf(2) syscall for a Linux.apk session.
// It also intercepts a few calls that only make sense with real eBPF:
// statfs(2) on /sys/fs/bpf, getsockopt(SO_ANDROID_DROP_REASON), raising
// RLIMIT_MEMLOCK, SELinux label reads of clatd and the eBPF pins,
// alarm-clock timerfds, and real-time scheduling requests.
//
// Android's networking stack (netbpfload, netd, system_server's
// connectivity code) needs eBPF maps, but a rootless session can't use real
// eBPF. pivot-exec installs a seccomp filter on Android init that turns every
// bpf() call into a user notification; the binder broker receives it and
// forwards it to this file's serve thread, which answers it (it runs in
// pivot-exec, outside Android's namespaces). seccomp catches the syscall
// itself, so it works for every process and linker namespace, static binaries
// and JNI code included.
//
// Model:
//  - Objects (maps, programs, BTF, links) live in the supervisor. A process
//    holds a memfd named "lxbpf-<kind>-<id>", injected with
//    SECCOMP_IOCTL_NOTIF_ADDFD, so fds survive fork/exec/SCM_RIGHTS like real
//    bpf fds. The supervisor identifies a target fd by its /proc/<pid>/fd link.
//  - Hash and array maps (and their per-CPU, LRU and LPM variants) keep their
//    data here, so every process sees the same contents.
//  - Ring buffer maps are sized memfds, so consumers can mmap them (and never
//    see data).
//  - Pins are regular files on the session's /sys/fs/bpf tmpfs that contain the
//    object name. BPF_OBJ_GET re-injects the object's memfd.
//  - Program load, attach, detach and links succeed and do nothing, so traffic
//    accounting and the per-UID firewall become no-ops.
//  - Permissions on pinned objects aren't enforced.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/bpf.h>
#include <linux/capability.h>
#include <linux/filter.h>
#include <linux/openat2.h>
#include <linux/seccomp.h>
#include <pthread.h>
#include <sched.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <sys/uio.h>
#include <sys/vfs.h>
#include <unistd.h>

#include "bpfemu.h"

#ifndef AUDIT_ARCH_X86_64
#define AUDIT_ARCH_X86_64 0xc000003e
#endif

// Android's private socket option (packages/modules/Connectivity
// bpf/progs/include/internal_net_api.h).
#define SO_ANDROID_DROP_REASON 0xAD01D01

#define LOG(...) fprintf(stderr, "bpfemu: " __VA_ARGS__)

enum kind { K_MAP, K_PROG, K_BTF, K_LINK, K_NKINDS };
static const char* kKindName[K_NKINDS] = {"map", "prog", "btf", "link"};

struct obj {
  enum kind kind;
  uint32_t id;
  int memfd;
  // maps
  uint32_t type, key_size, value_size, max_entries, flags;
  uint32_t vstride;  // bytes stored per value (per-CPU maps: all CPUs)
  char name[BPF_OBJ_NAME_LEN];
  uint8_t* keys;
  uint8_t* vals;
  uint8_t* used;  // hash-like maps only
  uint32_t count;
  size_t bytes;  // charged to map_bytes
  // programs
  uint32_t prog_type, insn_cnt;
};

static struct obj** objs[K_NKINDS];
static uint32_t nobjs[K_NKINDS];
static int ncpus = 1;

// Objects live as long as the session, so Android can't be allowed to grow
// them without bound. A boot creates about 250 objects and 25 MB of maps
// (two thirds of it per-CPU values, on 32 CPUs).
#define MAX_OBJS 4096
#define MAX_MAP_BYTES ((size_t)128 << 20)
#define MAX_MAP_BYTES_PER_CPU ((size_t)2 << 20)
static size_t map_bytes;
static int notif_fd = -1;

// ---- target access ----------------------------------------------------------

static int rd(pid_t pid, uint64_t addr, void* buf, size_t len) {
  if (!len) return 0;
  if (!addr) return -EFAULT;
  struct iovec l = {buf, len}, r = {(void*)(uintptr_t)addr, len};
  return process_vm_readv(pid, &l, 1, &r, 1, 0) == (ssize_t)len ? 0 : -EFAULT;
}

static int wr(pid_t pid, uint64_t addr, const void* buf, size_t len) {
  if (!len) return 0;
  if (!addr) return -EFAULT;
  struct iovec l = {(void*)buf, len}, r = {(void*)(uintptr_t)addr, len};
  return process_vm_writev(pid, &l, 1, &r, 1, 0) == (ssize_t)len ? 0 : -EFAULT;
}

static int rdstr(pid_t pid, uint64_t addr, char* buf, size_t len) {
  // Read up to len-1 bytes, stopping at NUL; tolerate page ends by reading
  // byte-wise on failure.
  memset(buf, 0, len);
  if (rd(pid, addr, buf, len - 1) != 0) {
    for (size_t i = 0; i < len - 1; i++) {
      if (rd(pid, addr + i, buf + i, 1) != 0) return i ? 0 : -EFAULT;
      if (!buf[i]) break;
    }
  }
  buf[len - 1] = 0;
  return 0;
}

// Which emulated object does fd `fd` of `pid` refer to?
static struct obj* lookup_fd(pid_t pid, int fd, int want_kind) {
  char path[64], link[128];
  snprintf(path, sizeof(path), "/proc/%d/fd/%d", pid, fd);
  ssize_t n = readlink(path, link, sizeof(link) - 1);
  if (n < 0) return NULL;
  link[n] = 0;
  char kname[16];
  unsigned id;
  if (sscanf(link, "/memfd:lxbpf-%15[a-z]-%u", kname, &id) != 2) return NULL;
  for (int k = 0; k < K_NKINDS; k++) {
    if (strcmp(kname, kKindName[k]) != 0) continue;
    if (want_kind >= 0 && k != want_kind) return NULL;
    if (id == 0 || id > nobjs[k]) return NULL;
    return objs[k][id - 1];
  }
  return NULL;
}

static struct obj* new_obj(enum kind k) {
  uint32_t total = 0;
  for (int i = 0; i < K_NKINDS; i++) total += nobjs[i];
  if (total >= MAX_OBJS) return NULL;
  struct obj* o = calloc(1, sizeof(*o));
  if (!o) return NULL;
  o->kind = k;
  o->id = nobjs[k] + 1;
  char name[32];
  snprintf(name, sizeof(name), "lxbpf-%s-%u", kKindName[k], o->id);
  o->memfd = memfd_create(name, MFD_CLOEXEC);
  if (o->memfd < 0) {
    free(o);
    return NULL;
  }
  struct obj** n = realloc(objs[k], sizeof(*n) * (nobjs[k] + 1));
  if (!n) {
    close(o->memfd);
    free(o);
    return NULL;
  }
  objs[k] = n;
  objs[k][nobjs[k]++] = o;
  return o;
}

// Unregister and free the newest object of kind k, whose creation failed
// (nobody else has seen it yet).
static void drop_last(enum kind k) {
  struct obj* o = objs[k][--nobjs[k]];
  map_bytes -= o->bytes;
  free(o->keys);
  free(o->vals);
  free(o->used);
  close(o->memfd);
  free(o);
}

// ---- maps
// ---------------------------------------------------------------------

static int is_array(uint32_t t) {
  switch (t) {
    case BPF_MAP_TYPE_ARRAY:
    case BPF_MAP_TYPE_PERCPU_ARRAY:
    case BPF_MAP_TYPE_PROG_ARRAY:
    case BPF_MAP_TYPE_PERF_EVENT_ARRAY:
    case BPF_MAP_TYPE_CGROUP_ARRAY:
    case BPF_MAP_TYPE_ARRAY_OF_MAPS:
    case BPF_MAP_TYPE_DEVMAP:
    case BPF_MAP_TYPE_CPUMAP:
    case BPF_MAP_TYPE_XSKMAP:
    case BPF_MAP_TYPE_SOCKMAP:
      return 1;
  }
  return 0;
}

static int is_percpu(uint32_t t) {
  return t == BPF_MAP_TYPE_PERCPU_HASH || t == BPF_MAP_TYPE_PERCPU_ARRAY ||
         t == BPF_MAP_TYPE_LRU_PERCPU_HASH ||
         t == BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE;
}

static int is_ring(uint32_t t) {
  return t == BPF_MAP_TYPE_RINGBUF || t == BPF_MAP_TYPE_USER_RINGBUF;
}

// Charge `n` bytes to the map budget.
static int charge(struct obj* m, size_t n) {
  size_t max = MAX_MAP_BYTES + MAX_MAP_BYTES_PER_CPU * (size_t)ncpus;
  if (n > max - map_bytes) return -ENOMEM;
  map_bytes += n;
  m->bytes += n;
  return 0;
}

static int map_alloc(struct obj* m) {
  // Element operations copy keys through a fixed buffer; LPM keys are
  // {u32 prefixlen; u8 data[1..256]} as in kernel/bpf/lpm_trie.c.
  if (m->key_size > 512) return -E2BIG;
  if (
    m->type == BPF_MAP_TYPE_LPM_TRIE && (m->key_size < 5 || m->key_size > 260)
  )
    return -EINVAL;
  if (is_ring(m->type)) {
    // consumer page + producer page + data mapped twice
    long pg = sysconf(_SC_PAGESIZE);
    size_t sz = 2 * (size_t)pg + 2 * (size_t)m->max_entries;
    int r = charge(m, sz);
    if (r) return r;
    return ftruncate(m->memfd, (off_t)sz) ? -errno : 0;
  }
  switch (m->type) {
    case BPF_MAP_TYPE_SK_STORAGE:
    case BPF_MAP_TYPE_INODE_STORAGE:
    case BPF_MAP_TYPE_TASK_STORAGE:
    case BPF_MAP_TYPE_CGRP_STORAGE:
      // Local storage maps have max_entries == 0 (keyed by object fds);
      // keep a small table so userspace lookups behave.
      if (m->max_entries == 0) m->max_entries = 256;
      break;
  }
  if (!m->max_entries || m->max_entries > (1u << 24)) return -EINVAL;
  uint64_t stride = m->value_size;
  if (is_percpu(m->type)) stride = ((stride + 7) & ~7ull) * (uint64_t)ncpus;
  if (stride > (1u << 24)) return -E2BIG;
  m->vstride = (uint32_t)stride;
  size_t n = m->max_entries, vs = m->vstride ? m->vstride : 1;
  size_t ks = m->key_size ? m->key_size : 1;
  // At most 2^24 entries of at most 2^24 + 512 + 1 bytes: no overflow.
  int r = charge(m, n * vs + (is_array(m->type) ? 0 : n * (ks + 1)));
  if (r) return r;
  m->vals = calloc(n, vs);
  if (!m->vals) return -ENOMEM;
  if (!is_array(m->type)) {
    m->keys = calloc(n, ks);
    m->used = calloc(n, 1);
    if (!m->keys || !m->used) return -ENOMEM;
  }
  return 0;
}

static int64_t find_key(struct obj* m, const uint8_t* key) {
  for (uint32_t i = 0; i < m->max_entries; i++)
    if (
      m->used[i] && !memcmp(m->keys + (size_t)i * m->key_size, key, m->key_size)
    )
      return i;
  return -1;
}

// Longest-prefix match for LPM tries: key = {u32 prefixlen; u8 data[]}.
static int64_t find_lpm(struct obj* m, const uint8_t* key) {
  uint32_t qlen;
  memcpy(&qlen, key, 4);
  int64_t best = -1;
  uint32_t best_len = 0;
  for (uint32_t i = 0; i < m->max_entries; i++) {
    if (!m->used[i]) continue;
    const uint8_t* k = m->keys + (size_t)i * m->key_size;
    uint32_t plen;
    memcpy(&plen, k, 4);
    if (plen > qlen || (best >= 0 && plen < best_len)) continue;
    uint32_t full = plen / 8, rem = plen % 8;
    if (memcmp(k + 4, key + 4, full)) continue;
    if (rem && ((k[4 + full] ^ key[4 + full]) & (0xff << (8 - rem)))) continue;
    best = i;
    best_len = plen;
  }
  return best;
}

static uint8_t* map_value(struct obj* m, const uint8_t* key, int for_lookup) {
  if (is_array(m->type)) {
    uint32_t idx;
    memcpy(&idx, key, 4);
    return idx < m->max_entries ? m->vals + (size_t)idx * m->vstride : NULL;
  }
  int64_t i = (for_lookup && m->type == BPF_MAP_TYPE_LPM_TRIE)
                ? find_lpm(m, key)
                : find_key(m, key);
  return i < 0 ? NULL : m->vals + (size_t)i * m->vstride;
}

static int map_update(
  struct obj* m, const uint8_t* key, const uint8_t* val, uint64_t flags
) {
  if (is_array(m->type)) {
    if (flags == BPF_NOEXIST) return -EEXIST;
    uint8_t* v = map_value(m, key, 0);
    if (!v) return -E2BIG;
    memcpy(v, val, m->vstride);
    return 0;
  }
  if (m->type == BPF_MAP_TYPE_LPM_TRIE) {
    // find_lpm trusts stored prefix lengths to fit the key.
    uint32_t plen;
    memcpy(&plen, key, 4);
    if (plen > (m->key_size - 4) * 8) return -EINVAL;
  }
  int64_t i = find_key(m, key);
  if (i >= 0) {
    if ((flags & 3) == BPF_NOEXIST) return -EEXIST;
    memcpy(m->vals + (size_t)i * m->vstride, val, m->vstride);
    return 0;
  }
  if ((flags & 3) == BPF_EXIST) return -ENOENT;
  if (m->count >= m->max_entries) {
    if (
      m->type != BPF_MAP_TYPE_LRU_HASH &&
      m->type != BPF_MAP_TYPE_LRU_PERCPU_HASH
    )
      return -E2BIG;
    for (uint32_t j = 0; j < m->max_entries; j++)  // evict something
      if (m->used[j]) {
        m->used[j] = 0;
        m->count--;
        break;
      }
  }
  for (uint32_t j = 0; j < m->max_entries; j++) {
    if (m->used[j]) continue;
    m->used[j] = 1;
    m->count++;
    memcpy(m->keys + (size_t)j * m->key_size, key, m->key_size);
    memcpy(m->vals + (size_t)j * m->vstride, val, m->vstride);
    return 0;
  }
  return -E2BIG;
}

static int map_delete(struct obj* m, const uint8_t* key) {
  if (is_array(m->type)) return -EINVAL;
  int64_t i = find_key(m, key);
  if (i < 0) return -ENOENT;
  m->used[i] = 0;
  m->count--;
  return 0;
}

static int map_next_key(struct obj* m, const uint8_t* key, uint8_t* next) {
  if (is_array(m->type)) {
    uint32_t idx = 0, n;
    if (key) memcpy(&idx, key, 4);
    n = (!key || idx >= m->max_entries) ? 0 : idx + 1;
    if (n >= m->max_entries) return -ENOENT;
    memcpy(next, &n, 4);
    return 0;
  }
  int64_t start = 0;
  if (key) {
    int64_t i = find_key(m, key);
    start = i < 0 ? 0 : i + 1;  // unknown key: restart from the first
  }
  for (uint32_t j = start; j < m->max_entries; j++) {
    if (!m->used[j]) continue;
    memcpy(next, m->keys + (size_t)j * m->key_size, m->key_size);
    return 0;
  }
  return -ENOENT;
}

// ---- syscall handlers
// -----------------------------------------------------------

// Complete the syscall by installing obj's memfd in the target; returns the
// target fd (the syscall's return value) or -errno.
static int inject(uint64_t req_id, struct obj* o) {
  struct seccomp_notif_addfd a = {
    .id = req_id,
    .flags = SECCOMP_ADDFD_FLAG_SEND,
    .srcfd = o->memfd,
    .newfd = 0,
    .newfd_flags = O_CLOEXEC,
  };
  int r = ioctl(notif_fd, SECCOMP_IOCTL_NOTIF_ADDFD, &a);
  return r < 0 ? -errno : r;
}

#define INJECTED 0x7fffffff  // handler already completed the syscall

static long do_map_create(pid_t pid, union bpf_attr* a, uint64_t req_id) {
  (void)pid;
  struct obj* m = new_obj(K_MAP);
  if (!m) return -ENOMEM;
  m->type = a->map_type;
  m->key_size = a->key_size;
  m->value_size = a->value_size;
  m->max_entries = a->max_entries;
  m->flags = a->map_flags;
  // Like kernel/bpf/devmap.c: device maps are read-only for programs.
  if (m->type == BPF_MAP_TYPE_DEVMAP || m->type == BPF_MAP_TYPE_DEVMAP_HASH)
    m->flags |= BPF_F_RDONLY_PROG;
  memcpy(m->name, a->map_name, sizeof(m->name));
  int r = map_alloc(m);
  if (!r) r = inject(req_id, m);
  if (r < 0) {
    drop_last(K_MAP);
    return r;
  }
  return INJECTED;
}

static long do_elem(pid_t pid, int cmd, union bpf_attr* a) {
  struct obj* m = lookup_fd(pid, a->map_fd, K_MAP);
  if (!m) return -EBADF;
  // Ring buffers have no elements (and no tables).
  if (!m->vals) return -EINVAL;
  uint8_t key[512], next[512];
  if (m->key_size > sizeof(key)) return -E2BIG;
  int have_key = a->key != 0;
  if (have_key && rd(pid, a->key, key, m->key_size)) return -EFAULT;
  if (!have_key && cmd != BPF_MAP_GET_NEXT_KEY) return -EINVAL;
  switch (cmd) {
    case BPF_MAP_LOOKUP_ELEM:
    case BPF_MAP_LOOKUP_AND_DELETE_ELEM: {
      uint8_t* v = map_value(m, key, 1);
      if (!v) return -ENOENT;
      if (wr(pid, a->value, v, m->vstride)) return -EFAULT;
      if (cmd == BPF_MAP_LOOKUP_AND_DELETE_ELEM && !is_array(m->type))
        map_delete(m, key);
      return 0;
    }
    case BPF_MAP_UPDATE_ELEM: {
      uint8_t* buf = malloc(m->vstride ? m->vstride : 1);
      if (!buf) return -ENOMEM;
      int r = rd(pid, a->value, buf, m->vstride);
      if (!r) r = map_update(m, key, buf, a->flags);
      free(buf);
      return r;
    }
    case BPF_MAP_DELETE_ELEM:
      return map_delete(m, key);
    case BPF_MAP_GET_NEXT_KEY: {
      int r = map_next_key(m, have_key ? key : NULL, next);
      if (r) return r;
      return wr(pid, a->next_key, next, m->key_size) ? -EFAULT : 0;
    }
  }
  return -EINVAL;
}

static long do_new_simple(enum kind k, union bpf_attr* a, uint64_t req_id) {
  struct obj* o = new_obj(k);
  if (!o) return -ENOMEM;
  if (k == K_PROG) {
    o->prog_type = a->prog_type;
    o->insn_cnt = a->insn_cnt;
    memcpy(o->name, a->prog_name, sizeof(o->name));
  }
  int r = inject(req_id, o);
  if (r < 0) {
    drop_last(k);
    return r;
  }
  return INJECTED;
}

static int openat2_(
  int dirfd, const char* path, uint64_t flags, uint64_t mode, uint64_t resolve
) {
  struct open_how how = {.flags = flags, .mode = mode, .resolve = resolve};
  int fd = (int)syscall(__NR_openat2, dirfd, path, &how, sizeof(how));
  return fd < 0 ? -errno : fd;
}

// The session's /sys/fs/bpf tmpfs, opened by pivot-exec before any Android
// code runs. Pins resolve only beneath it, never through a caller's root or
// mount namespace (where Android could mount FUSE over /sys/fs/bpf).
static int pin_dir = -1;

void bpfemu_set_pin_dir(int fd) { pin_dir = fd; }

// Open `rel` beneath pin_dir. This runs on the supervisor's only thread with
// the host user's power: no symlinks (they would resolve against the host's
// root), no '..' out and no other mounts (FUSE).
static int open_beneath(const char* rel, int flags, mode_t mode) {
  if (pin_dir < 0) return -ENOENT;
  return openat2_(
    pin_dir, *rel ? rel : ".", flags | O_CLOEXEC, mode,
    RESOLVE_BENEATH | RESOLVE_NO_XDEV | RESOLVE_NO_SYMLINKS |
      RESOLVE_NO_MAGICLINKS
  );
}

// Open pin `uaddr` (an absolute path under /sys/fs/bpf), never with a
// blocking open (O_NONBLOCK: a FIFO), and only if it is a regular file.
static int open_pin(pid_t pid, uint64_t uaddr, int flags, mode_t mode) {
  char p[256];
  if (rdstr(pid, uaddr, p, sizeof(p))) return -EFAULT;
  // Relative/path_fd pins and pins elsewhere: unsupported.
  if (strncmp(p, "/sys/fs/bpf/", 12) != 0) return -EINVAL;
  int fd =
    open_beneath(p + 12, flags | O_NOFOLLOW | O_NONBLOCK | O_NOCTTY, mode);
  if (fd == -EACCES && !(flags & O_CREAT)) {
    // Without a user namespace (Flatpak mode) there is no CAP_DAC_OVERRIDE,
    // and netbpfload makes pins such as 0060: read ours with the owner's
    // read bit set for the open.
    int pfd = open_beneath(p + 12, O_PATH | O_NOFOLLOW, 0);
    struct stat st;
    char self[32];
    if (pfd < 0) return fd;
    snprintf(self, sizeof(self), "/proc/self/fd/%d", pfd);
    if (
      fstat(pfd, &st) == 0 && S_ISREG(st.st_mode) &&
      chmod(self, (st.st_mode & 07777) | 0400) == 0
    ) {
      fd = open(self, O_RDONLY | O_CLOEXEC | O_NONBLOCK | O_NOCTTY);
      if (fd < 0) fd = -errno;
      chmod(self, st.st_mode & 07777);
    }
    close(pfd);
  }
  if (fd < 0) return fd;
  struct stat st;
  if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode)) {
    close(fd);
    return -EINVAL;
  }
  return fd;
}

static long do_obj_pin(pid_t pid, union bpf_attr* a) {
  struct obj* o = lookup_fd(pid, a->bpf_fd, -1);
  if (!o) return -EBADF;
  int fd = open_pin(pid, a->pathname, O_WRONLY | O_CREAT | O_EXCL, 0600);
  if (fd < 0) return fd;
  dprintf(fd, "lxbpf-%s-%u\n", kKindName[o->kind], o->id);
  close(fd);
  return 0;
}

static long do_obj_get(pid_t pid, union bpf_attr* a, uint64_t req_id) {
  char buf[64], kname[16];
  int r;
  int fd = open_pin(pid, a->pathname, O_RDONLY, 0);
  if (fd < 0) return fd;
  ssize_t n = read(fd, buf, sizeof(buf) - 1);
  close(fd);
  if (n <= 0) return -EINVAL;
  buf[n] = 0;
  unsigned id;
  if (sscanf(buf, "lxbpf-%15[a-z]-%u", kname, &id) != 2) return -EINVAL;
  for (int k = 0; k < K_NKINDS; k++) {
    if (strcmp(kname, kKindName[k]) || !id || id > nobjs[k]) continue;
    r = inject(req_id, objs[k][id - 1]);
    return r < 0 ? r : INJECTED;
  }
  return -ENOENT;
}

static long do_get_info(pid_t pid, union bpf_attr* a, uint64_t uattr) {
  struct obj* o = lookup_fd(pid, a->info.bpf_fd, -1);
  if (!o) return -EBADF;
  union {
    struct bpf_map_info map;
    struct bpf_prog_info prog;
    struct bpf_btf_info btf;
    struct bpf_link_info link;
  } info;
  memset(&info, 0, sizeof(info));
  size_t sz;
  switch (o->kind) {
    case K_MAP:
      info.map.type = o->type;
      info.map.id = o->id;
      info.map.key_size = o->key_size;
      info.map.value_size = o->value_size;
      info.map.max_entries = o->max_entries;
      info.map.map_flags = o->flags;
      memcpy(info.map.name, o->name, sizeof(info.map.name));
      sz = sizeof(info.map);
      break;
    case K_PROG:
      info.prog.type = o->prog_type;
      info.prog.id = o->id;
      // Loaders check that the "kernel" translated and JITed the program.
      info.prog.xlated_prog_len = o->insn_cnt * 8;
      info.prog.jited_prog_len = o->insn_cnt * 8;
      memcpy(info.prog.name, o->name, sizeof(info.prog.name));
      sz = sizeof(info.prog);
      break;
    case K_BTF:
      info.btf.id = o->id;
      sz = sizeof(info.btf);
      break;
    default:
      info.link.id = o->id;
      sz = sizeof(info.link);
      break;
  }
  // The kernel accepts larger user structs (zero tail) and copies back
  // min(user size, kernel size), then updates info_len.
  uint32_t n = a->info.info_len < sz ? a->info.info_len : sz;
  if (wr(pid, a->info.info, &info, n)) return -EFAULT;
  if (wr(pid, uattr + offsetof(union bpf_attr, info.info_len), &n, sizeof(n)))
    return -EFAULT;
  return 0;
}

static long do_next_id(
  pid_t pid, enum kind k, union bpf_attr* a, uint64_t uattr
) {
  uint32_t next = a->start_id + 1;
  if (next > nobjs[k]) return -ENOENT;
  return wr(pid, uattr + offsetof(union bpf_attr, next_id), &next, sizeof(next))
           ? -EFAULT
           : 0;
}

static long do_fd_by_id(enum kind k, union bpf_attr* a, uint64_t req_id) {
  uint32_t id = a->map_id;  // same offset as prog_id/btf_id/link_id
  if (!id || id > nobjs[k]) return -ENOENT;
  int r = inject(req_id, objs[k][id - 1]);
  return r < 0 ? r : INJECTED;
}

// Last program "attached" per attach type (the target is ignored), so
// BPF_PROG_QUERY can confirm attachments as netd expects.
#define MAX_ATTACH_TYPES 128
static uint32_t attached_prog[MAX_ATTACH_TYPES];

static long do_prog_attach(pid_t pid, int cmd, union bpf_attr* a) {
  if (a->attach_type >= MAX_ATTACH_TYPES) return 0;
  if (cmd == BPF_PROG_DETACH) {
    attached_prog[a->attach_type] = 0;
    return 0;
  }
  struct obj* p = lookup_fd(pid, a->attach_bpf_fd, K_PROG);
  if (!p) return -EBADF;
  attached_prog[a->attach_type] = p->id;
  return 0;
}

static long do_prog_query(pid_t pid, union bpf_attr* a, uint64_t uattr) {
  uint32_t t = a->query.attach_type;
  uint32_t id = t < MAX_ATTACH_TYPES ? attached_prog[t] : 0;
  uint32_t cnt = id ? 1 : 0;
  if (
    cnt && a->query.prog_cnt >= 1 && a->query.prog_ids &&
    wr(pid, a->query.prog_ids, &id, sizeof(id))
  )
    return -EFAULT;
  return wr(
           pid, uattr + offsetof(union bpf_attr, query.prog_cnt), &cnt,
           sizeof(cnt)
         )
           ? -EFAULT
           : 0;
}

// statfs(2): libbpf refuses to pin outside a filesystem with BPF_FS_MAGIC.
// For paths under /sys/fs/bpf (our tmpfs) answer with that magic; let the
// kernel handle everything else.
#define CONTINUE 0x7ffffffe
#ifndef BPF_FS_MAGIC
#define BPF_FS_MAGIC 0xcafe4a11
#endif
static long handle_statfs(struct seccomp_notif* req) {
  pid_t pid = req->pid;
  char p[256];
  if (rdstr(pid, req->data.args[0], p, sizeof(p))) return CONTINUE;
  if (strncmp(p, "/sys/fs/bpf", 11) != 0 || (p[11] && p[11] != '/'))
    return CONTINUE;
  if (ioctl(notif_fd, SECCOMP_IOCTL_NOTIF_ID_VALID, &req->id) != 0)
    return -ESRCH;
  int fd = open_beneath(p[11] ? p + 12 : "", O_PATH, 0);
  if (fd < 0) return fd;
  struct statfs st;
  int r = fstatfs(fd, &st);
  close(fd);
  if (r != 0) return -errno;
  st.f_type = BPF_FS_MAGIC;
  return wr(pid, req->data.args[1], &st, sizeof(st)) ? -EFAULT : 0;
}

// getsockopt(SOL_SOCKET, SO_ANDROID_DROP_REASON): normally answered by netd's
// cgroup getsockopt eBPF program (bpf/progs/netd.c); netd checks it works.
static long handle_getsockopt(struct seccomp_notif* req) {
  pid_t pid = req->pid;
  uint32_t len;
  if (rd(pid, req->data.args[4], &len, sizeof(len))) return -EFAULT;
  if (len < sizeof(uint64_t))
    return -EINVAL;     // like the program on 6.1+ kernels
  uint64_t reason = 0;  // DROP_REASON_NONE
  len = sizeof(reason);
  if (wr(pid, req->data.args[3], &reason, sizeof(reason))) return -EFAULT;
  return wr(pid, req->data.args[4], &len, sizeof(len)) ? -EFAULT : 0;
}

// RLIMIT_MEMLOCK: raising it needs CAP_SYS_RESOURCE in the initial
// namespace (the host caps it at 8 MiB), and netd treats failing to raise it
// to 1 GiB (for eBPF map accounting) as fatal. Emulated maps don't lock
// memory, so pretend the change worked; reads go to the kernel.
static long handle_memlock(struct seccomp_notif* req) {
  if (req->data.nr == __NR_prlimit64 && !req->data.args[2]) return CONTINUE;
  return 0;
}

// getxattr/lgetxattr("security.selinux"): with SELinux hidden, files carry
// host labels. ClatCoordinator (system_server) verifies the Android labels of
// clatd and the clat eBPF pins and aborts otherwise. Answer with the labels
// file_contexts/genfs_contexts would give for those paths; let the kernel
// answer everything else.
static const char* android_label(const char* p) {
  static const struct {
    const char* path;
    const char* ctx;
  } fixed[] = {
    {"/apex/com.android.tethering/bin/for-system", "u:object_r:system_file:s0"},
    {"/apex/com.android.tethering/bin/for-system/clatd",
     "u:object_r:clatd_exec:s0"},
    {"/sys/fs/bpf", "u:object_r:fs_bpf:s0"},
  };
  for (size_t i = 0; i < sizeof(fixed) / sizeof(fixed[0]); i++)
    if (!strcmp(p, fixed[i].path)) return fixed[i].ctx;
  // /sys/fs/bpf/<dir>[/...] -> fs_bpf_<dir> (Android's genfs_contexts).
  static char buf[128];
  if (!strncmp(p, "/sys/fs/bpf/", 12)) {
    const char* d = p + 12;
    size_t n = strcspn(d, "/");
    if (n && n < 64) {
      snprintf(buf, sizeof(buf), "u:object_r:fs_bpf_%.*s:s0", (int)n, d);
      return buf;
    }
  }
  return NULL;
}

static long handle_getxattr(struct seccomp_notif* req) {
  pid_t pid = req->pid;
  char name[32], p[256];
  if (rdstr(pid, req->data.args[1], name, sizeof(name))) return CONTINUE;
  if (strcmp(name, "security.selinux")) return CONTINUE;
  if (rdstr(pid, req->data.args[0], p, sizeof(p)) || p[0] != '/')
    return CONTINUE;
  const char* ctx = android_label(p);
  if (!ctx) return CONTINUE;
  size_t len = strlen(ctx) + 1, size = req->data.args[3];
  if (size == 0) return len;
  if (size < len) return -ERANGE;
  return wr(pid, req->data.args[2], ctx, len) ? -EFAULT : (long)len;
}

// timerfd_create(CLOCK_*_ALARM) needs CAP_WAKE_ALARM in the initial
// namespace. AlarmManagerService then falls back to a handler path that
// crashes system_server on listener alarms. Hand out a timer on the matching
// non-alarm clock: same semantics except waking a suspended machine.
static long handle_timerfd(struct seccomp_notif* req) {
  int clock =
    req->data.args[0] == CLOCK_REALTIME_ALARM ? CLOCK_REALTIME : CLOCK_BOOTTIME;
  int flags = (int)req->data.args[1];
  int fd = timerfd_create(clock, flags | TFD_CLOEXEC);
  if (fd < 0) return -errno;
  struct seccomp_notif_addfd a = {
    .id = req->id,
    .flags = SECCOMP_ADDFD_FLAG_SEND,
    .srcfd = fd,
    .newfd = 0,
    .newfd_flags = (flags & TFD_CLOEXEC) ? O_CLOEXEC : 0,
  };
  int r = ioctl(notif_fd, SECCOMP_IOCTL_NOTIF_ADDFD, &a);
  close(fd);
  return r < 0 ? -errno : INJECTED;
}

// sched_setscheduler(SCHED_FIFO/SCHED_RR) needs CAP_SYS_NICE in the initial
// namespace (RLIMIT_RTPRIO is 0 on the host). The Bluetooth stack aborts when
// it can't get real-time scheduling. Pretend it worked; threads keep their
// normal policy.
static long handle_sched(struct seccomp_notif* req) {
  (void)req;
  return 0;
}

// capget, Flatpak mode only: every capability, as capset pretended to keep
// (system_server checks it may block suspend before it reads input).
static long handle_capget(struct seccomp_notif* req) {
  struct __user_cap_header_struct hdr;
  if (rd(req->pid, req->data.args[0], &hdr, sizeof(hdr))) return -EFAULT;
  if (hdr.version != _LINUX_CAPABILITY_VERSION_3 || !req->data.args[1])
    return CONTINUE;
  struct __user_cap_data_struct d[2] = {
    {.effective = ~0u, .permitted = ~0u},
    {.effective = (1u << (CAP_LAST_CAP - 31)) - 1,
     .permitted = (1u << (CAP_LAST_CAP - 31)) - 1},
  };
  return wr(req->pid, req->data.args[1], d, sizeof(d)) ? -EFAULT : 0;
}

static long handle(struct seccomp_notif* req) {
  if (req->data.nr == __NR_sched_setscheduler) return handle_sched(req);
  if (req->data.nr == __NR_timerfd_create) return handle_timerfd(req);
  if (req->data.nr == __NR_getxattr || req->data.nr == __NR_lgetxattr)
    return handle_getxattr(req);
  if (req->data.nr == __NR_statfs) return handle_statfs(req);
  if (req->data.nr == __NR_prlimit64 || req->data.nr == __NR_setrlimit)
    return handle_memlock(req);
  if (req->data.nr == __NR_getsockopt) return handle_getsockopt(req);
  // setsockopt(SO_MARK), Flatpak mode only: the session shares the host's
  // network namespace, where netd's per-network routing marks mean nothing.
  if (req->data.nr == __NR_setsockopt) return 0;
  // capset, prctl(PR_SET_SECUREBITS, PR_CAPBSET_DROP, PR_CAP_AMBIENT): Flatpak
  // mode only. The sandbox has no capabilities to raise, keep or drop; under
  // desktop parity every process has the user's power anyway.
  if (req->data.nr == __NR_capset || req->data.nr == __NR_prctl) return 0;
  if (req->data.nr == __NR_capget) return handle_capget(req);
  pid_t pid = req->pid;
  int cmd = (int)req->data.args[0];
  uint64_t uattr = req->data.args[1];
  uint32_t size = (uint32_t)req->data.args[2];
  union bpf_attr a;
  memset(&a, 0, sizeof(a));
  if (size > sizeof(a)) size = sizeof(a);
  if (rd(pid, uattr, &a, size)) return -EFAULT;
  // Don't act on behalf of a task that already went away.
  if (ioctl(notif_fd, SECCOMP_IOCTL_NOTIF_ID_VALID, &req->id) != 0)
    return -ESRCH;

  switch (cmd) {
    case BPF_MAP_CREATE:
      return do_map_create(pid, &a, req->id);
    case BPF_MAP_LOOKUP_ELEM:
    case BPF_MAP_UPDATE_ELEM:
    case BPF_MAP_DELETE_ELEM:
    case BPF_MAP_GET_NEXT_KEY:
    case BPF_MAP_LOOKUP_AND_DELETE_ELEM:
      return do_elem(pid, cmd, &a);
    case BPF_PROG_LOAD:
      return do_new_simple(K_PROG, &a, req->id);
    case BPF_BTF_LOAD:
      return do_new_simple(K_BTF, &a, req->id);
    case BPF_LINK_CREATE:
    case BPF_RAW_TRACEPOINT_OPEN:
      return do_new_simple(K_LINK, &a, req->id);
    case BPF_OBJ_PIN:
      return do_obj_pin(pid, &a);
    case BPF_OBJ_GET:
      return do_obj_get(pid, &a, req->id);
    case BPF_OBJ_GET_INFO_BY_FD:
      return do_get_info(pid, &a, uattr);
    case BPF_PROG_GET_NEXT_ID:
      return do_next_id(pid, K_PROG, &a, uattr);
    case BPF_MAP_GET_NEXT_ID:
      return do_next_id(pid, K_MAP, &a, uattr);
    case BPF_BTF_GET_NEXT_ID:
      return do_next_id(pid, K_BTF, &a, uattr);
    case BPF_PROG_GET_FD_BY_ID:
      return do_fd_by_id(K_PROG, &a, req->id);
    case BPF_MAP_GET_FD_BY_ID:
      return do_fd_by_id(K_MAP, &a, req->id);
    case BPF_BTF_GET_FD_BY_ID:
      return do_fd_by_id(K_BTF, &a, req->id);
    case BPF_PROG_QUERY:
      return do_prog_query(pid, &a, uattr);
    case BPF_PROG_ATTACH:
    case BPF_PROG_DETACH:
      return do_prog_attach(pid, cmd, &a);
    case BPF_PROG_TEST_RUN:
    case BPF_MAP_FREEZE:
    case BPF_LINK_UPDATE:
    case BPF_LINK_DETACH:
    case BPF_PROG_BIND_MAP:
      return 0;
    case BPF_TOKEN_CREATE:  // libbpf feature probe
      return -EOPNOTSUPP;
  }
  LOG("unsupported bpf cmd %d from pid %d\n", cmd, pid);
  return -EINVAL;
}

// ---- setup / loop
// ---------------------------------------------------------------

#ifndef __NR_openat
#define __NR_openat 257
#endif
#ifndef __NR_faccessat2
#define __NR_faccessat2 439
#endif
#ifndef __NR_ioctl
#define __NR_ioctl 16
#endif

int bpfemu_install_filter(int single_uid, int flat) {
  // The syscall clauses (bpf, statfs, getxattr, getsockopt, prlimit, timerfd,
  // sched). All jumps are relative, and they target NOTIF/ALLOW, which sit
  // immediately after this block, so the whole block can be shifted down by a
  // fixed prefix without touching any offset inside it.
  struct sock_filter clauses[] = {
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_bpf, 23, 0),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_statfs, 22, 0),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getxattr, 21, 0),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_lgetxattr, 20, 0),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getsockopt, 0, 4),
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SOL_SOCKET, 0, 18),
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SO_ANDROID_DROP_REASON, 15, 16),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_prlimit64, 3, 0),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_setrlimit, 0, 4),
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[0])),
    BPF_STMT(BPF_JMP | BPF_JA, 1),
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, RLIMIT_MEMLOCK, 9, 10),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_timerfd_create, 0, 3),
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[0])),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, CLOCK_REALTIME_ALARM, 6, 0),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, CLOCK_BOOTTIME_ALARM, 5, 6),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_sched_setscheduler, 0, 5),
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])),
    BPF_STMT(BPF_ALU | BPF_AND | BPF_K, ~SCHED_RESET_ON_FORK),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SCHED_FIFO, 1, 0),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SCHED_RR, 0, 1),
  };
  const int nclauses = sizeof(clauses) / sizeof(clauses[0]);

  struct sock_filter f[128];
  int n = 0;
  // Prefix: arch guard and nr load. `notif`/`allow` indices are filled once the
  // program length is known.
  f[n++] = (struct sock_filter)BPF_STMT(
    BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)
  );
  int arch_jf = n;  // patched below (jump-false to ALLOW)
  f[n++] = (struct sock_filter)BPF_JUMP(
    BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 0, 0
  );
  f[n++] = (struct sock_filter)BPF_STMT(
    BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)
  );

  // openat -> NOTIF (inject the binder memfd); ioctl of type 'b' -> NOTIF
  // (serve the binder command). The broker serves both.
  int openat_i = n;
  f[n++] =
    (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_openat, 0, 0);
  int ioctl_i = n;
  f[n++] =
    (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_ioctl, 0, 0);
  f[n++] = (struct sock_filter)BPF_STMT(
    BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])
  );
  f[n++] = (struct sock_filter)BPF_STMT(BPF_ALU | BPF_AND | BPF_K, 0xff00);
  int typejeq_i = n;
  f[n++] =
    (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, ('b' << 8), 0, 0);

  // Single-uid mode (virtual ids): the credential syscalls -> NOTIF, and
  // getsockopt(SOL_SOCKET, SO_PEERCRED) -> NOTIF; everything else falls
  // through to the clauses with nr reloaded.
  static const int kVidNrs[] = {
    __NR_getuid,    __NR_geteuid,   __NR_getgid,    __NR_getegid,
    __NR_getresuid, __NR_getresgid, __NR_getgroups, __NR_setuid,
    __NR_setgid,    __NR_setreuid,  __NR_setregid,  __NR_setresuid,
    __NR_setresgid, __NR_setfsuid,  __NR_setfsgid,  __NR_setgroups,
    __NR_chown,     __NR_fchown,    __NR_lchown,    __NR_fchownat,
  };
  int vid_start = n;  // where non-binder syscalls continue
  int vid_jumps[32], nvid_jumps = 0, peercred_i = -1, lstat_i = -1,
                     access_i = -1;
  if (single_uid) {
    for (size_t i = 0; i < sizeof(kVidNrs) / sizeof(kVidNrs[0]); i++) {
      vid_jumps[nvid_jumps++] = n;
      f[n++] = (struct sock_filter)BPF_JUMP(
        BPF_JMP | BPF_JEQ | BPF_K, kVidNrs[i], 0, 0
      );
    }
    // faccessat/faccessat2 asking for W_OK: ART refuses writable dex files,
    // which every APK is to the one kernel uid (see
    // supervisor/broker/src/creds/syscalls.rs).
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, __NR_faccessat, 1, 0
    );
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, __NR_faccessat2, 0, 3
    );
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])
    );
    access_i = n;
    f[n++] =
      (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JSET | BPF_K, W_OK, 0, 0);
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)
    );
    // lstat(): newfstatat(AT_FDCWD, path, st, AT_SYMLINK_NOFOLLOW) exactly,
    // for the virtual owners (see supervisor/broker/src/creds/syscalls.rs).
    // Other stat calls run natively.
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, __NR_newfstatat, 0, 5
    );
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[0])
    );
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, (uint32_t)AT_FDCWD, 0, 2
    );
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[3])
    );
    lstat_i = n;
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, AT_SYMLINK_NOFOLLOW, 0, 0
    );
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)
    );
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, __NR_getsockopt, 0, 5
    );
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])
    );
    f[n++] =
      (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SOL_SOCKET, 0, 2);
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])
    );
    peercred_i = n;
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, SO_PEERCRED, 0, 0
    );
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)
    );
  }

  // Flatpak mode (no namespaces): readlink of /proc magic links, which show
  // the composed root's host paths (the broker rewrites them), fstat (the
  // user's files shown as root's), mkdir and chmod (the owner keeps rwx),
  // capget, capset and the capability prctls (there are no capabilities to
  // give or drop; bpfemu accepts them and reports a full set), and
  // setsockopt(SO_MARK), which needs CAP_NET_ADMIN (bpfemu accepts and
  // ignores it, as it does netd's other per-network policy).
  int flat_jumps[16], nflat_jumps = 0, mark_i = -1, prctl_i[3] = {-1, -1, -1};
  if (flat) {
    flat_jumps[nflat_jumps++] = n;
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, __NR_readlinkat, 0, 0
    );
    flat_jumps[nflat_jumps++] = n;
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, __NR_readlink, 0, 0
    );
    flat_jumps[nflat_jumps++] = n;
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, __NR_getcwd, 0, 0
    );
    flat_jumps[nflat_jumps++] = n;
    f[n++] =
      (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_fstat, 0, 0);
    static const int kFlatNrs[] = {
      __NR_capget, __NR_capset, __NR_mkdir,    __NR_mkdirat,
      __NR_chmod,  __NR_fchmod, __NR_fchmodat, 452 /* fchmodat2 */,
    };
    for (size_t i = 0; i < sizeof(kFlatNrs) / sizeof(kFlatNrs[0]); i++) {
      flat_jumps[nflat_jumps++] = n;
      f[n++] = (struct sock_filter)BPF_JUMP(
        BPF_JMP | BPF_JEQ | BPF_K, kFlatNrs[i], 0, 0
      );
    }
    f[n++] =
      (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_prctl, 0, 5);
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[0])
    );
    prctl_i[0] = n;
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, PR_SET_SECUREBITS, 0, 0
    );
    prctl_i[1] = n;
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, PR_CAPBSET_DROP, 0, 0
    );
    prctl_i[2] = n;
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, PR_CAP_AMBIENT, 0, 0
    );
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)
    );
    f[n++] = (struct sock_filter)BPF_JUMP(
      BPF_JMP | BPF_JEQ | BPF_K, __NR_setsockopt, 0, 5
    );
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])
    );
    f[n++] =
      (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SOL_SOCKET, 0, 2);
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])
    );
    mark_i = n;
    f[n++] =
      (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SO_MARK, 0, 0);
    f[n++] = (struct sock_filter)BPF_STMT(
      BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)
    );
  }

  // The bpf clauses follow.
  for (int i = 0; i < nclauses; i++) f[n++] = clauses[i];
  int notif = n;
  f[n++] =
    (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF);
  int allow = n;
  f[n++] = (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW);

  // Patch relative jumps now that NOTIF/ALLOW positions are known.
  f[arch_jf].jf = allow - arch_jf - 1;
  f[openat_i].jt = notif - openat_i - 1;    // openat -> NOTIF
  f[openat_i].jf = 0;                       // fall to ioctl check
  f[ioctl_i].jt = 0;                        // fall to the type load
  f[ioctl_i].jf = vid_start - ioctl_i - 1;  // not ioctl -> the rest
  f[typejeq_i].jt = notif - typejeq_i - 1;  // type 'b' -> NOTIF
  f[typejeq_i].jf = allow - typejeq_i - 1;  // other ioctl -> ALLOW
  for (int i = 0; i < nvid_jumps; i++)
    f[vid_jumps[i]].jt = notif - vid_jumps[i] - 1;
  if (peercred_i >= 0) f[peercred_i].jt = notif - peercred_i - 1;
  if (lstat_i >= 0) f[lstat_i].jt = notif - lstat_i - 1;
  if (access_i >= 0) f[access_i].jt = notif - access_i - 1;
  for (int i = 0; i < nflat_jumps; i++)
    f[flat_jumps[i]].jt = notif - flat_jumps[i] - 1;
  if (mark_i >= 0) f[mark_i].jt = notif - mark_i - 1;
  for (int i = 0; i < 3; i++)
    if (prctl_i[i] >= 0) f[prctl_i[i]].jt = notif - prctl_i[i] - 1;

  struct sock_fprog prog = {.len = n, .filter = f};
  // Waits stay interruptible (no WAIT_KILLABLE_RECV): ptrace (debuggerd) and
  // job control must be able to stop threads parked in a binder read. The
  // broker makes an interrupted, restarted BINDER_WRITE_READ idempotent
  // instead.
  return syscall(
    SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_NEW_LISTENER,
    &prog
  );
}

// The broker is the listener's only receiver (binder ioctls and openat are
// hot; bpf and friends are rare), and it forwards the notifications that are
// ours over this SOCK_SEQPACKET socket.
static int forwarded = -1;

static void* serve(void* arg) {
  (void)arg;
  long n = sysconf(_SC_NPROCESSORS_CONF);
  ncpus = n > 0 ? (int)n : 1;
  struct seccomp_notif_sizes sizes;
  if (syscall(SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, &sizes) != 0) {
    LOG("SECCOMP_GET_NOTIF_SIZES: %s\n", strerror(errno));
    return NULL;
  }
  struct seccomp_notif* req = calloc(1, sizes.seccomp_notif);
  struct seccomp_notif_resp* resp = calloc(1, sizes.seccomp_notif_resp);
  for (;;) {
    memset(req, 0, sizes.seccomp_notif);
    ssize_t got = recv(forwarded, req, sizes.seccomp_notif, 0);
    if (got < 0 && errno == EINTR) continue;
    if (got <= 0) {
      LOG("forwarded notification: %s\n", got ? strerror(errno) : "EOF");
      return NULL;
    }
    long r = handle(req);
    if (r == INJECTED) continue;
    memset(resp, 0, sizes.seccomp_notif_resp);
    resp->id = req->id;
    if (r == CONTINUE)
      resp->flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE;
    else if (r < 0)
      resp->error = (int)r;
    else
      resp->val = r;
    if (ioctl(notif_fd, SECCOMP_IOCTL_NOTIF_SEND, resp) != 0 && errno != ENOENT)
      LOG("NOTIF_SEND: %s\n", strerror(errno));
  }
}

int bpfemu_start(int listener, int forwarded_sock) {
  notif_fd = listener;
  forwarded = forwarded_sock;
  pthread_t t;
  return pthread_create(&t, NULL, serve, NULL);
}
