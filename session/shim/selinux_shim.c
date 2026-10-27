// LD_PRELOAD shim that lets Android userspace run with SELinux hidden.
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

EXPORT int security_compute_create(const char* scon, const char* tcon, security_class_t tclass,
                                   char** newcon) {
    (void)scon; (void)tcon; (void)tclass;
    *newcon = strdup(kServiceContext);
    return *newcon ? 0 : -1;
}
EXPORT int security_compute_create_raw(const char* scon, const char* tcon,
                                       security_class_t tclass, char** newcon) {
    return security_compute_create(scon, tcon, tclass, newcon);
}

#define NOOP_SETTER(name) \
    EXPORT int name(const char* con) { (void)con; return 0; } \
    EXPORT int name##_raw(const char* con) { (void)con; return 0; }

NOOP_SETTER(setcon)
NOOP_SETTER(setexeccon)
NOOP_SETTER(setfscreatecon)
NOOP_SETTER(setsockcreatecon)
NOOP_SETTER(setkeycreatecon)

#define NOOP_FILE_SETTER(name, arg_t) \
    EXPORT int name(arg_t f, const char* con) { (void)f; (void)con; return 0; } \
    EXPORT int name##_raw(arg_t f, const char* con) { (void)f; (void)con; return 0; }

NOOP_FILE_SETTER(setfilecon, const char*)
NOOP_FILE_SETTER(lsetfilecon, const char*)
NOOP_FILE_SETTER(fsetfilecon, int)
