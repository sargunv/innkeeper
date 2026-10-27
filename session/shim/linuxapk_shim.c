// LD_PRELOAD shim that lets an unmodified Android userspace run in a rootless
// user namespace. Mostly SELinux; see also rlimits/priorities at the end.
//
// The session hides /sys/fs/selinux, so Android's libselinux reports SELinux
// as disabled and skips access checks, restorecon and app domain transitions.
// A few call sites in init still assume an Android policy is loaded:
//
//   * Service::Start computes a domain with security_compute_create(), which
//     needs selinuxfs and fails, so no service without a seclabel can start.
//   * The forked child calls setexeccon(seclabel); the host kernel's SELinux
//     rejects Android contexts with EINVAL, and init treats that as fatal.
//   * Sockets and files get setsockcreatecon()/setfscreatecon() contexts.
//
// This shim makes those calls succeed as no-ops. The getters (getcon,
// getfilecon, getpeercon...) are left alone: they return host labels, which
// only end up in log lines and in checks that libselinux already skips.
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>

#define EXPORT __attribute__((visibility("default")))

typedef unsigned short security_class_t;

static const char kServiceContext[] = "u:r:linuxapk:s0";

EXPORT int security_compute_create(
  const char* scon, const char* tcon, security_class_t tclass, char** newcon
) {
  (void)scon;
  (void)tcon;
  (void)tclass;
  *newcon = strdup(kServiceContext);
  return *newcon ? 0 : -1;
}
EXPORT int security_compute_create_raw(
  const char* scon, const char* tcon, security_class_t tclass, char** newcon
) {
  return security_compute_create(scon, tcon, tclass, newcon);
}

#define NOOP_SETTER(name)                  \
  EXPORT int name(const char* con) {       \
    (void)con;                             \
    return 0;                              \
  }                                        \
  EXPORT int name##_raw(const char* con) { \
    (void)con;                             \
    return 0;                              \
  }

NOOP_SETTER(setcon)
NOOP_SETTER(setexeccon)
NOOP_SETTER(setfscreatecon)
NOOP_SETTER(setsockcreatecon)
NOOP_SETTER(setkeycreatecon)

#define NOOP_FILE_SETTER(name, arg_t)               \
  EXPORT int name(arg_t f, const char* con) {       \
    (void)f;                                        \
    (void)con;                                      \
    return 0;                                       \
  }                                                 \
  EXPORT int name##_raw(arg_t f, const char* con) { \
    (void)f;                                        \
    (void)con;                                      \
    return 0;                                       \
  }

NOOP_FILE_SETTER(setfilecon, const char*)
NOOP_FILE_SETTER(lsetfilecon, const char*)
NOOP_FILE_SETTER(fsetfilecon, int)

// selinux_status_*: the status page lives in selinuxfs, which is hidden.
// servicemanager CHECKs that selinux_status_open() succeeds. Report a
// status that never changes (no policy reloads, permissive).
EXPORT int selinux_status_open(int fallback) {
  (void)fallback;
  return 0;
}
EXPORT void selinux_status_close(void) {}
EXPORT int selinux_status_updated(void) { return 0; }
EXPORT int selinux_status_getenforce(void) { return 0; }
EXPORT int selinux_status_policyload(void) { return 0; }
EXPORT int selinux_status_deny_unknown(void) { return 0; }

// Labelling files by writing the security.selinux xattr directly (bionic's
// property area setup, restorecon fallbacks): the host kernel rejects Android
// contexts, and bionic's __system_property_area_init() fails, which is fatal
// for init. Pretend success for that one xattr; pass everything else through.
#include <sys/syscall.h>
#include <unistd.h>

static int is_selinux_xattr(const char* name) {
  return name && strcmp(name, "security.selinux") == 0;
}

// Single-uid mode (docs/sandbox.md): the kernel rejects POSIX ACL entries
// naming Android's uids and gids, which aren't mapped. vold gives
// /data/media/<user> a default ACL (group media_rw/everybody) and fails user
// storage preparation without it, and system_server then reboots to recovery
// to wipe /data. Every Android process is the same kernel uid there, so the
// ACL has nothing to grant: skip it.
//
// The mode comes from LINUXAPK_SINGLE_UID=1, which init passes on to every
// service. (Not the ro.boot property: libc's own startup fstat()s the
// property areas through this shim before properties can be read.)
static int single_uid_state = -1;
static int single_uid(void) {
  if (single_uid_state < 0) {
    const char* e = getenv("LINUXAPK_SINGLE_UID");
    single_uid_state = e && !strcmp(e, "1");
  }
  return single_uid_state;
}
static int is_skipped_xattr(const char* name) {
  if (is_selinux_xattr(name)) return 1;
  return name && !strncmp(name, "system.posix_acl_", 17) && single_uid();
}

EXPORT int fsetxattr(
  int fd, const char* name, const void* value, size_t size, int flags
) {
  if (is_skipped_xattr(name)) return 0;
  return syscall(SYS_fsetxattr, fd, name, value, size, flags);
}
EXPORT int setxattr(
  const char* path, const char* name, const void* value, size_t size, int flags
) {
  if (is_skipped_xattr(name)) return 0;
  return syscall(SYS_setxattr, path, name, value, size, flags);
}
EXPORT int lsetxattr(
  const char* path, const char* name, const void* value, size_t size, int flags
) {
  if (is_skipped_xattr(name)) return 0;
  return syscall(SYS_lsetxattr, path, name, value, size, flags);
}

// Resource limits and priorities. In a user namespace, raising a hard
// rlimit, lowering nice below the host's RLIMIT_NICE, or picking RT policies
// needs capabilities in the initial namespace. init treats a failed
// setrlimit/setpriority in a service's "rlimit"/"priority" option as fatal
// for that service (zygote has "priority -20"). Clamp limits to what we may
// set and treat priority changes we aren't allowed to make as best-effort.
#include <stdlib.h>
#include <sys/resource.h>

EXPORT int setpriority(int which, id_t who, int prio) {
  long rc = syscall(SYS_setpriority, which, who, prio);
  if (rc != 0 && (errno == EACCES || errno == EPERM)) return 0;
  return rc;
}

EXPORT int prlimit64(
  pid_t pid, int resource, const struct rlimit64* new_limit,
  struct rlimit64* old_limit
) {
  long rc = syscall(SYS_prlimit64, pid, resource, new_limit, old_limit);
  if (rc != 0 && errno == EPERM && new_limit) {
    struct rlimit64 cur, clamped = *new_limit;
    if (syscall(SYS_prlimit64, pid, resource, NULL, &cur) != 0) return -1;
    if (clamped.rlim_max > cur.rlim_max) clamped.rlim_max = cur.rlim_max;
    if (clamped.rlim_cur > clamped.rlim_max)
      clamped.rlim_cur = clamped.rlim_max;
    rc = syscall(SYS_prlimit64, pid, resource, &clamped, old_limit);
  }
  return rc;
}
EXPORT int setrlimit64(int resource, const struct rlimit64* limit) {
  return prlimit64(0, resource, limit, NULL);
}
EXPORT int setrlimit(int resource, const struct rlimit* limit) {
  struct rlimit64 l = {limit->rlim_cur, limit->rlim_max};
  return prlimit64(0, resource, &l, NULL);
}
EXPORT int prlimit(
  pid_t pid, int resource, const struct rlimit* new_limit,
  struct rlimit* old_limit
) {
  struct rlimit64 n, o;
  if (new_limit) {
    n.rlim_cur = new_limit->rlim_cur;
    n.rlim_max = new_limit->rlim_max;
  }
  int rc =
    prlimit64(pid, resource, new_limit ? &n : NULL, old_limit ? &o : NULL);
  if (rc == 0 && old_limit) {
    old_limit->rlim_cur = o.rlim_cur;
    old_limit->rlim_max = o.rlim_max;
  }
  return rc;
}

// Scope: the shim is LD_PRELOADed into init only. It has to be loaded by
// path, and no single path is visible from every linker namespace (vendor
// and APEX namespaces reject /system/lib64, and a bare name is only found in
// some of them), so letting init's services inherit LD_PRELOAD makes some of
// them fail to link. init's forked children still run the shim between fork
// and exec, which is where init needs it. Services that need it themselves
// get "setenv LD_PRELOAD /system/lib64/liblinuxapk_shim.so" in their rc.
__attribute__((constructor)) static void linuxapk_shim_init(void) {
  if (syscall(SYS_getpid) == 1) unsetenv("LD_PRELOAD");
}

// /proc/config.gz: system_server (Debug.isVmapStack, via libvintf's
// LoadKernelConfigs) CHECKs that it can read the kernel config, but distro
// kernels often don't build CONFIG_IKCONFIG_PROC, and procfs can't be given a
// file it lacks. The session launcher stages the host's /boot/config-$(uname
// -r) as /linuxapk/config.gz; serve that when /proc/config.gz is missing.
// (zygote, and so system_server and apps, preload this shim.)
#include <dlfcn.h>
typedef void* gzFile;
EXPORT gzFile gzopen(const char* path, const char* mode) {
  static gzFile (*real)(const char*, const char*);
  if (!real)
    real = (gzFile (*)(const char*, const char*))dlsym(RTLD_NEXT, "gzopen");
  if (!real) return NULL;
  gzFile f = real(path, mode);
  if (!f && path && strcmp(path, "/proc/config.gz") == 0)
    f = real("/linuxapk/config.gz", mode);
  return f;
}

// Quota project ids: vold (and installd) tag app directories with
// FS_IOC_FSSETXATTR and FS_PROJINHERIT_FL. Only the initial user namespace may
// change a project id, so the first fails with EINVAL here, and filesystems
// without project quotas (btrfs) reject the flag. vold forgives that only on
// ext4 ("legacy userdata"); elsewhere it gives up on the app's external dirs,
// getExternalFilesDir() returns null, and apps that use it crash (Godot does
// at startup). Treat the refusal as success, as vold does on ext4, and set the
// other flags without FS_PROJINHERIT_FL.
#include <linux/fs.h>
#include <stdarg.h>
#include <sys/ioctl.h>
EXPORT int ioctl(int fd, int request, ...) {
  va_list ap;
  va_start(ap, request);
  void* arg = va_arg(ap, void*);
  va_end(ap);
  long rc = syscall(SYS_ioctl, fd, request, arg);
  if (rc == -1 && (errno == EINVAL || errno == EPERM || errno == EOPNOTSUPP)) {
    if ((unsigned)request == FS_IOC_FSSETXATTR) return 0;
    if (
      (unsigned)request == FS_IOC_SETFLAGS && arg &&
      (*(unsigned*)arg & FS_PROJINHERIT_FL)
    ) {
      unsigned flags = *(unsigned*)arg & ~FS_PROJINHERIT_FL;
      return syscall(SYS_ioctl, fd, request, &flags);
    }
  }
  return rc;
}

// Single-uid mode (docs/sandbox.md): the kernel sees every file as owned by
// uid 0, and the seccomp supervisor records the owner a chown asked for in
// the "user.linuxapk.owner" xattr ("uid:gid"). installd verifies the owners of
// the app directories it prepares (fs_prepare_dir_strict, the cache dirs, the
// fts walks of fixupAppData and clearAppCache) and treats a mismatch as a
// failure, which makes PackageManager wipe and recreate the app's data on
// every boot. Report the recorded owner. Only processes whose rc sets
// LINUXAPK_VSTAT=1 (installd) pay for the extra getxattrat per stat. Not
// zygote: its children include sandboxed processes (WebView renderers) whose
// own seccomp policy kills them for a syscall it doesn't allow. Bionic's own
// callers (fts) reach these wrappers through the PLT too.
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/stat.h>

static int vstat_state = -1;
static int vstat_on(void) {
  if (vstat_state < 0) {
    const char* e = getenv("LINUXAPK_VSTAT");
    vstat_state = e && !strcmp(e, "1") && single_uid();
  }
  return vstat_state;
}

static void vstat_apply(long n, const char* buf, struct stat* st) {
  if (n <= 0 || n >= 32) return;
  char tmp[32];
  memcpy(tmp, buf, n);
  tmp[n] = 0;
  unsigned uid, gid;
  if (sscanf(tmp, "%u:%u", &uid, &gid) == 2) {
    st->st_uid = uid;
    st->st_gid = gid;
  }
}

// getxattrat(2) (Linux 6.13): the session's seccomp filter traps every
// getxattr/lgetxattr (for security.selinux), which would cost a supervisor
// round trip per stat; getxattrat isn't trapped. Older kernels fall back.
#ifndef __NR_getxattrat
#define __NR_getxattrat 464
#endif
struct linuxapk_xattr_args {
  uint64_t value;
  uint32_t size;
  uint32_t flags;
};
static int have_getxattrat = 1;

static long vstat_getxattr(
  int dirfd, const char* path, int flags, char* buf, size_t len
) {
  static const char kName[] = "user.linuxapk.owner";
  if (have_getxattrat) {
    struct linuxapk_xattr_args a = {(uintptr_t)buf, (uint32_t)len, 0};
    long n = syscall(
      __NR_getxattrat, dirfd, path,
      flags & (AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH), kName, &a, sizeof(a)
    );
    if (n >= 0 || errno != ENOSYS) return n;
    have_getxattrat = 0;
  }
  if ((flags & AT_EMPTY_PATH) && !*path)
    return syscall(SYS_fgetxattr, dirfd, kName, buf, len);
  char p[4096];
  const char* q = path;
  if (path[0] != '/' && dirfd != AT_FDCWD) {
    if (
      snprintf(p, sizeof(p), "/proc/self/fd/%d/%s", dirfd, path) >=
      (int)sizeof(p)
    )
      return -1;
    q = p;
  }
  return syscall(
    (flags & AT_SYMLINK_NOFOLLOW) ? SYS_lgetxattr : SYS_getxattr, q, kName, buf,
    len
  );
}

static void vstat_fixup(
  int dirfd, const char* path, int flags, struct stat* st
) {
  if (!S_ISDIR(st->st_mode) && !S_ISREG(st->st_mode)) return;
  char buf[32];
  vstat_apply(vstat_getxattr(dirfd, path, flags, buf, sizeof(buf)), buf, st);
}

EXPORT int fstatat(int dirfd, const char* path, struct stat* st, int flags) {
  int saved = errno;
  long r = syscall(SYS_newfstatat, dirfd, path, st, flags);
  if (r == 0 && vstat_on()) {
    vstat_fixup(dirfd, path, flags, st);
    errno = saved;
  }
  return r;
}
EXPORT int fstatat64(int dirfd, const char* path, struct stat64* st, int flags)
  __attribute__((alias("fstatat")));
EXPORT int stat(const char* path, struct stat* st) {
  return fstatat(AT_FDCWD, path, st, 0);
}
EXPORT int stat64(const char* path, struct stat64* st)
  __attribute__((alias("stat")));
EXPORT int lstat(const char* path, struct stat* st) {
  return fstatat(AT_FDCWD, path, st, AT_SYMLINK_NOFOLLOW);
}
EXPORT int lstat64(const char* path, struct stat64* st)
  __attribute__((alias("lstat")));
EXPORT int fstat(int fd, struct stat* st) {
  int saved = errno;
  long r = syscall(SYS_fstat, fd, st);
  if (r == 0 && vstat_on()) {
    vstat_fixup(fd, "", AT_EMPTY_PATH, st);
    errno = saved;
  }
  return r;
}
EXPORT int fstat64(int fd, struct stat64* st) __attribute__((alias("fstat")));
