// Userspace bpf(2) emulation for the session; see bpfemu.c.
#pragma once

// Install the seccomp filter on the calling thread (inherited by all
// descendants). Returns the notification listener fd, or -1. If with_binder is
// nonzero, the filter also traps openat and binder ioctls (ioctl cmd type byte
// == 'b') so the serve loop can forward them to the ubinder broker; the kernel
// allows only one notification listener per process, so bpf and binder must
// share this one filter. If single_uid is nonzero (it needs with_binder), it
// also traps the credential syscalls (set*id/get*id, setgroups/getgroups,
// the chown family, getsockopt(SO_PEERCRED)), which the broker helper
// emulates with virtual ids (ubinder/src/vid.rs, docs/sandbox.md).
int bpfemu_install_filter(int with_binder, int single_uid);

// ubinder mode: the broker helper receives every notification on the listener
// and forwards the non-binder ones (one seccomp_notif per SOCK_SEQPACKET
// message) to this socket; bpfemu_serve then reads from it instead of the
// listener, and still answers via the listener. -1 (default) disables this.
void bpfemu_set_binder_forward(int sock);

// Serve notifications on the listener fd (passed as the pointer value).
// Never returns unless the listener breaks.
void* bpfemu_serve(void* listener_fd);
