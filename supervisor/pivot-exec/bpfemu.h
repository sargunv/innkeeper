// Userspace bpf(2) emulation for the session; see bpfemu.c.
#pragma once

// Install the seccomp filter on the calling thread (inherited by all
// descendants). Returns the notification listener fd, or -1. The filter traps
// bpf and its companions (see bpfemu.c), openat and binder ioctls (ioctl cmd
// type byte == 'b'); the kernel allows only one notification listener per
// process, so bpf and binder share this one filter. The runtime also traps
// credential syscalls, readlink/getcwd, file metadata and capabilities for
// Android's virtual ids and composed root. Only the bootless developer
// harness (harness != 0) uses real credentials and namespace paths.
int bpfemu_install_filter(int harness);

// The directory pins live in (the session's /sys/fs/bpf tmpfs), opened
// before any Android code runs. Without it, pins fail with ENOENT.
void bpfemu_set_pin_dir(int fd);

// Start the serve thread. The broker receives every notification on
// `listener` and forwards the non-binder ones (one seccomp_notif per
// SOCK_SEQPACKET message) to `forwarded_sock`; the thread answers them via
// `listener`. Returns 0, or an error number if the thread can't start.
int bpfemu_start(int listener, int forwarded_sock);
