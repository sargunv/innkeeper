// LD_PRELOADed into OpenHarmony's init (ohos/sandbox.sh), the musl
// counterpart of supervisor/shim for what the Flatpak sandbox can't give it:
//
//  - init runs only as pid 1 ("Process id error"), and in the sandbox it is
//    the broker's child: getpid() is 1 in init itself, and real in the
//    services it forks.
//  - Flatpak's seccomp filter refuses mount, umount2, unshare and setns
//    before ours sees them; init's mounts succeed as no-ops, as the Android
//    shim does for its init (the composed root already has what they'd mount).
//  - Services get their cfg's "importance" through setpriority before exec,
//    and exit if it fails: without CAP_SYS_NICE, raising priority is
//    best-effort, as in the Android shim.
//
// Built freestanding (no libc headers or link): it binds to the guest's musl
// at load time, so one file works with whichever libc the image ships.
#define SYS_getpid 39
#define SYS_mount 165
#define SYS_umount2 166
#define SYS_unshare 272
#define SYS_setns 308
#define SYS_setpriority 141

extern char* getenv(const char*);
extern int unsetenv(const char*);

static long init_pid;

static long sys0(long nr) {
  long r;
  __asm__ volatile("syscall" : "=a"(r) : "a"(nr) : "rcx", "r11", "memory");
  return r;
}

// pivot-exec marks init with LINUXAPK_SHIM_INIT. Neither it nor the preload
// reaches the services init starts.
__attribute__((constructor)) static void ohos_shim_init(void) {
  if (getenv("LINUXAPK_SHIM_INIT")) {
    init_pid = sys0(SYS_getpid);
    unsetenv("LINUXAPK_SHIM_INIT");
    unsetenv("LD_PRELOAD");
  }
}

static int is_init(void) { return init_pid && sys0(SYS_getpid) == init_pid; }

int getpid(void) { return is_init() ? 1 : (int)sys0(SYS_getpid); }

// The rest only reach the kernel outside init (a fork that execs nothing).
extern long syscall(long, ...);

int mount(
  const char* src, const char* dst, const char* type, unsigned long flags,
  const void* data
) {
  if (init_pid) return 0;
  return (int)syscall(SYS_mount, src, dst, type, flags, data);
}

int umount2(const char* dst, int flags) {
  if (init_pid) return 0;
  return (int)syscall(SYS_umount2, dst, flags);
}

int umount(const char* dst) { return umount2(dst, 0); }

int unshare(int flags) {
  if (init_pid) return 0;
  return (int)syscall(SYS_unshare, flags);
}

int setns(int fd, int type) {
  if (init_pid) return 0;
  return (int)syscall(SYS_setns, fd, type);
}

int setpriority(int which, unsigned who, int prio) {
  long r = syscall(SYS_setpriority, which, who, prio);
  return init_pid ? 0 : (int)r;
}
