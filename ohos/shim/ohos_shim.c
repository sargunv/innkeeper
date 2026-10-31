// LD_PRELOADed into OpenHarmony's init and what it starts (ohos/sandbox.sh),
// the musl counterpart of supervisor/shim for what the Flatpak sandbox can't
// give OH:
//
//  - init runs only as pid 1 ("Process id error"), and in the sandbox it is
//    the broker's child: getpid() is 1 in init itself, and real in the
//    services it forks.
//  - Flatpak's seccomp filter refuses mount, umount2, unshare, setns, chroot
//    and pivot_root before ours sees them; init's mounts succeed as no-ops, as
//    the Android shim does for its init (the composed root already has what
//    they'd mount). A mount namespace's setns still moves the caller to its
//    root, as the kernel does: init's mksandbox chdirs into the sandbox, and
//    services would otherwise inherit that cwd, which breaks the relative
//    paths apps load from (system/lib64/extensionability).
//  - Services get their cfg's "importance" through setpriority before exec,
//    and exit if it fails: without CAP_SYS_NICE, raising priority is
//    best-effort, as in the Android shim.
//  - appspawn and the other spawners give each app a mount namespace: bind
//    mounts into /mnt/sandbox/<user>/<bundle>, then pivot_root there. Apps
//    find their own files only at those paths (/data/storage/el1/bundle is
//    the app's code). Here the namespace calls succeed as no-ops, each bind
//    into the sandbox becomes a symlink at its mount point, and pivot_root
//    keeps the sandbox's /data/storage open on kViewFd: the session's
//    /data/storage links to /proc/self/fd/<kViewFd>, so each app process sees
//    its own. The rest of the app's root is the session's.
//  - Filesystems without a fixed inode table (btrfs) report 0 total and free
//    inodes from statfs, and BMS refuses any install that needs more than
//    are free ("Insufficient inodes"), SceneBoard's among them. There,
//    statfs reports a free inode per free block.
//
// Built freestanding (no libc headers or link): it binds to the guest's musl
// at load time, so one file works with whichever libc the image ships.
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

// pivot-exec gives init INNKEEPER_SHIM_ROLE=init, which the services init
// starts don't inherit. The spawners are known by name; the apps they fork
// inherit the flag.
__attribute__((constructor)) static void ohos_shim_init(void) {
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

// The rest only reach the kernel outside init (a fork that execs nothing) and
// the spawners.

// An app's view of /data/storage, which the session's /data/storage names.
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
  // A bind of another path into an app's sandbox: a symlink in place of the
  // mount point the spawner made (an empty directory or file). Binds of the
  // root, or of a path onto itself, need nothing.
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

int chroot(const char* root) {
  if (!spawner) return (int)ret(sys6(SYS_chroot, (long)root, 0, 0, 0, 0, 0));
  enter(root);
  return 0;
}

// The x86_64 kernel's struct statfs, which is musl's.
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

int setpriority(int which, unsigned who, int prio) {
  long r = sys6(SYS_setpriority, which, who, prio, 0, 0, 0);
  return init_pid ? 0 : (int)ret(r);
}
