// Experimental LD_PRELOAD for netd only (see session/rootfs-overlay/system/
// etc/init/netd.rc). netd aborts in libnetd_updatable_init() (tethering APEX)
// because the network eBPF programs can't be loaded in a user namespace.
// Skip that init and make socket tagging a no-op, to see how far netd and
// system_server get without eBPF. Not a real fix.
#define EXPORT __attribute__((visibility("default")))
#include <stdint.h>
#include <sys/types.h>

EXPORT int libnetd_updatable_init(const char* cg2_path) { (void)cg2_path; return 0; }
EXPORT int libnetd_updatable_tagSocket(int sockFd, uint32_t tag, uid_t chargeUid, uid_t realUid) {
    (void)sockFd; (void)tag; (void)chargeUid; (void)realUid; return 0;
}
EXPORT int libnetd_updatable_untagSocket(int sockFd) { (void)sockFd; return 0; }
