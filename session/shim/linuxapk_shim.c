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

EXPORT int fsetxattr(
  int fd, const char* name, const void* value, size_t size, int flags
) {
  if (is_selinux_xattr(name)) return 0;
  return syscall(SYS_fsetxattr, fd, name, value, size, flags);
}
EXPORT int setxattr(
  const char* path, const char* name, const void* value, size_t size, int flags
) {
  if (is_selinux_xattr(name)) return 0;
  return syscall(SYS_setxattr, path, name, value, size, flags);
}
EXPORT int lsetxattr(
  const char* path, const char* name, const void* value, size_t size, int flags
) {
  if (is_selinux_xattr(name)) return 0;
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
