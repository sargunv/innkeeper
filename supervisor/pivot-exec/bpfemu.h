// Userspace bpf(2) emulation for the session; see bpfemu.c.
#pragma once

// Install the seccomp filter on the calling thread (inherited by all
// descendants). Returns the notification listener fd, or -1. The filter traps
// bpf and its companions (see bpfemu.c), openat and binder ioctls (ioctl cmd
// type byte == 'b'); the kernel allows only one notification listener per
// process, so bpf and binder share this one filter. If single_uid is nonzero,
// it also traps the credential syscalls (set*id/get*id,
// setgroups/getgroups, the chown family, getsockopt(SO_PEERCRED)), which the
// broker emulates with virtual ids (supervisor/broker/src/creds/table.rs,
// supervisor/README.md).
int bpfemu_install_filter(int single_uid);

// The directory pins live in (the session's /sys/fs/bpf tmpfs), opened
// before any Android code runs. Without it, pins fail with ENOENT.
void bpfemu_set_pin_dir(int fd);

// Start the serve thread. The broker receives every notification on
// `listener` and forwards the non-binder ones (one seccomp_notif per
// SOCK_SEQPACKET message) to `forwarded_sock`; the thread answers them via
// `listener`. Returns 0, or an error number if the thread can't start.
int bpfemu_start(int listener, int forwarded_sock);
