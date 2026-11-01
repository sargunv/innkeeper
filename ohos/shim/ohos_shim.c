// LD_PRELOADed into OpenHarmony's init and the processes it starts
// (host/launcher/src/ohos.rs). It is the musl counterpart of supervisor/shim
// and covers what the Flatpak sandbox can't give OH:
//
//  - init refuses to run unless it is pid 1 ("Process id error"), and in the
//    sandbox it is the broker's child. getpid() returns 1 in init itself and
//    the real pid in the services it forks.
//  - Flatpak's seccomp filter refuses mount, umount2, unshare, setns, chroot
//    and pivot_root before our filter sees them. init's mounts succeed as
//    no-ops, as the Android shim does for its init, because the composed root
//    already has what they would mount. A setns into a mount namespace still
//    moves the caller to the root, as the kernel does. init's mksandbox chdirs
//    into the sandbox, and services would otherwise inherit that cwd, which
//    breaks the relative paths that apps load from
//    (system/lib64/extensionability).
//  - Services set their cfg's "importance" through setpriority before exec,
//    and exit if it fails. Without CAP_SYS_NICE, raising priority is
//    best-effort, as in the Android shim.
//  - appspawn and the other spawners give each app a mount namespace. They
//    bind-mount paths into /mnt/sandbox/<user>/<bundle> and then pivot_root
//    there, and apps find their files only at those paths
//    (/data/storage/el1/bundle is the app's code). Here the namespace calls
//    succeed as no-ops, and each bind into the sandbox becomes a symlink at
//    its mount point. pivot_root keeps the sandbox's /data/storage open on
//    kViewFd. The session's /data/storage links to /proc/self/fd/<kViewFd>,
//    so each app process sees its own. The rest of the app's root is the
//    session's.
//  - Filesystems without a fixed inode table, such as btrfs, report 0 total
//    and free inodes from statfs. BMS refuses any install that needs more
//    inodes than are free ("Insufficient inodes"), including SceneBoard's. On
//    those filesystems statfs reports a free inode per free block.
//  - /data is a symlink to the profile's directory on the host, so realpath
//    would return host paths. installd refuses to rename a code path unless
//    it starts with /data/app/el1/bundle/, which fails every install.
//    realpath returns /data in place of the host directory.
//  - The image's llvmpipe offers native fence fds when it can export a sync
//    file from a dma-buf, which udmabuf allows. Each export finishes every GL
//    context in the process from the exporting thread, and EGL exports one
//    on each swap. A swap on one render_service thread then flushes a
//    context that another thread is drawing with, and llvmpipe's binner
//    crashes (#309). The sync file export fails with ENOTTY, as on a kernel
//    before 6.0, so llvmpipe offers no native fences. EGL's software swap
//    still waits for its own context's rendering.
//
// The shim is built freestanding, with no libc headers or link. It binds to
// the guest's musl at load time, so one file works with whichever libc the
// image ships.
#define SYS_read 0
#define SYS_close 3
#define SYS_getpid 39
#define SYS_dup3 292
#define SYS_openat 257
#define SYS_chdir 80
#define SYS_rmdir 84
#define SYS_unlink 87
#define SYS_symlink 88
#define SYS_chroot 161
#define SYS_pivot_root 155
#define SYS_mount 165
#define SYS_umount2 166
#define SYS_unshare 272
#define SYS_setns 308
#define SYS_setpriority 141
#define SYS_statfs 137
#define SYS_ioctl 16
#define ENOTTY 25
// _IOWR('b', 2, struct dma_buf_export_sync_file)
#define DMA_BUF_IOCTL_EXPORT_SYNC_FILE 0xc0086202u
#define AT_FDCWD (-100)
#define O_DIRECTORY 0200000
#define O_CLOEXEC 02000000
#define O_PATH 010000000
#define MS_BIND 4096
#define CLONE_NEWNS 0x00020000

extern char* getenv(const char*);
extern int unsetenv(const char*);
extern int* __errno_location(void);

static long init_pid;
static int spawner;

extern void* dlsym(void*, const char*);
#define RTLD_NEXT ((void*)-1)
static char* (*next_realpath)(const char*, char*);
static char data_dir[4096];
static unsigned long data_len;

static long sys6(long nr, long a, long b, long c, long d, long e, long f) {
  long r;
  register long r10 __asm__("r10") = d;
  register long r8 __asm__("r8") = e;
  register long r9 __asm__("r9") = f;
  __asm__ volatile("syscall"
                   : "=a"(r)
                   : "a"(nr), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                   : "rcx", "r11", "memory");
  return r;
}
#define sys0(nr) sys6(nr, 0, 0, 0, 0, 0, 0)

// A libc-style result: -1 and errno for a failed syscall.
static long ret(long r) {
  if (r < 0 && r > -4096) {
    *__errno_location() = (int)-r;
    return -1;
  }
  return r;
}

static int starts(const char* s, const char* prefix) {
  while (*prefix) {
    if (*s++ != *prefix++) return 0;
  }
  return 1;
}

static int same(const char* a, const char* b) {
  return starts(a, b) && starts(b, a);
}

// The broker gives init INNKEEPER_SHIM_ROLE=init, and the shim unsets it so
// that the services init starts don't inherit it. The shim recognizes the
// spawners by name, and the apps they fork inherit the spawner flag.
__attribute__((constructor)) static void ohos_shim_init(void) {
  next_realpath = (char* (*)(const char*, char*))dlsym(RTLD_NEXT, "realpath");
  if (next_realpath("/data", data_dir)) {
    while (data_dir[data_len]) data_len++;
  }
  const char* role = getenv("INNKEEPER_SHIM_ROLE");
  if (role && same(role, "init")) {
    init_pid = sys0(SYS_getpid);
    unsetenv("INNKEEPER_SHIM_ROLE");
    return;
  }
  char comm[32] = {0};
  long fd =
    sys6(SYS_openat, AT_FDCWD, (long)"/proc/self/comm", O_CLOEXEC, 0, 0, 0);
  if (fd < 0) return;
  sys6(SYS_read, fd, (long)comm, sizeof(comm) - 1, 0, 0, 0);
  sys6(SYS_close, fd, 0, 0, 0, 0, 0);
  spawner = starts(comm, "appspawn") || starts(comm, "nativespawn") ||
            starts(comm, "hybridspawn") || starts(comm, "cjappspawn") ||
            starts(comm, "nwebspawn");
}

static int is_init(void) { return init_pid && sys0(SYS_getpid) == init_pid; }

int getpid(void) { return is_init() ? 1 : (int)sys0(SYS_getpid); }

// The wrappers below call the kernel only in processes other than init and
// the spawners. A fork of init that has not exec'd counts as init.

// The fd of an app's view of /data/storage, which the session's /data/storage
// links to.
enum { kViewFd = 990 };

static void enter(const char* root) {
  char path[4096];
  unsigned n = 0;
  for (const char* p = root; *p && n < sizeof(path) - 16; p++) path[n++] = *p;
  for (const char* p = "/data/storage"; *p; p++) path[n++] = *p;
  path[n] = 0;
  long fd =
    sys6(SYS_openat, AT_FDCWD, (long)path, O_PATH | O_DIRECTORY, 0, 0, 0);
  if (fd >= 0) {
    sys6(SYS_dup3, fd, kViewFd, 0, 0, 0, 0);
    sys6(SYS_close, fd, 0, 0, 0, 0, 0);
  }
  sys6(SYS_chdir, (long)"/", 0, 0, 0, 0, 0);
}

long syscall(long nr, long a, long b, long c, long d, long e, long f) {
  if (spawner && nr == SYS_pivot_root) {
    enter((const char*)a);
    return 0;
  }
  return ret(sys6(nr, a, b, c, d, e, f));
}

int mount(
  const char* src, const char* dst, const char* type, unsigned long flags,
  const void* data
) {
  if (init_pid) return 0;
  if (!spawner) {
    return (int)ret(sys6(
      SYS_mount, (long)src, (long)dst, (long)type, (long)flags, (long)data, 0
    ));
  }
  // A bind of another path into an app's sandbox becomes a symlink that
  // replaces the mount point the spawner made, an empty directory or file.
  // Binds of the root, or of a path onto itself, need nothing.
  if (
    (flags & MS_BIND) && src && dst && starts(dst, "/mnt/sandbox/") &&
    !same(src, "/") && !same(src, dst)
  ) {
    if (sys6(SYS_rmdir, (long)dst, 0, 0, 0, 0, 0) != 0) {
      sys6(SYS_unlink, (long)dst, 0, 0, 0, 0, 0);
    }
    sys6(SYS_symlink, (long)src, (long)dst, 0, 0, 0, 0);
  }
  return 0;
}

int umount2(const char* dst, int flags) {
  if (init_pid || spawner) return 0;
  return (int)ret(sys6(SYS_umount2, (long)dst, flags, 0, 0, 0, 0));
}

int umount(const char* dst) { return umount2(dst, 0); }

int unshare(int flags) {
  if (init_pid || spawner) return 0;
  return (int)ret(sys6(SYS_unshare, flags, 0, 0, 0, 0, 0));
}

int setns(int fd, int type) {
  if (init_pid || spawner) {
    if (!type || (type & CLONE_NEWNS))
      sys6(SYS_chdir, (long)"/", 0, 0, 0, 0, 0);
    return 0;
  }
  return (int)ret(sys6(SYS_setns, fd, type, 0, 0, 0, 0));
}

int ioctl(int fd, int req, ...) {
  __builtin_va_list ap;
  __builtin_va_start(ap, req);
  long arg = __builtin_va_arg(ap, long);
  __builtin_va_end(ap);
  if ((unsigned)req == DMA_BUF_IOCTL_EXPORT_SYNC_FILE) return (int)ret(-ENOTTY);
  return (int)ret(sys6(SYS_ioctl, fd, req, arg, 0, 0, 0));
}

int chroot(const char* root) {
  if (!spawner) return (int)ret(sys6(SYS_chroot, (long)root, 0, 0, 0, 0, 0));
  enter(root);
  return 0;
}

// The x86_64 kernel's struct statfs, which musl uses as is.
struct statfs {
  unsigned long f_type, f_bsize;
  unsigned long f_blocks, f_bfree, f_bavail, f_files, f_ffree;
  int f_fsid[2];
  unsigned long f_namelen, f_frsize, f_flags, f_spare[4];
};

int statfs(const char* path, struct statfs* st) {
  long r = ret(sys6(SYS_statfs, (long)path, (long)st, 0, 0, 0, 0));
  if (r == 0 && st->f_files == 0) st->f_files = st->f_ffree = st->f_bavail;
  return (int)r;
}

// The host directory is longer than "/data", so the result shrinks in place.
char* realpath(const char* path, char* out) {
  char* r = next_realpath(path, out);
  if (
    r && data_len >= 5 && starts(r, data_dir) &&
    (r[data_len] == '/' || r[data_len] == 0)
  ) {
    const char* rest = r + data_len;
    char* d = r;
    for (const char* p = "/data"; *p; p++) *d++ = *p;
    while ((*d++ = *rest++)) {
    }
  }
  return r;
}

int setpriority(int which, unsigned who, int prio) {
  long r = sys6(SYS_setpriority, which, who, prio, 0, 0, 0);
  return init_pid ? 0 : (int)ret(r);
}
