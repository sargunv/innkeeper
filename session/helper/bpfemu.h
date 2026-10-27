// Userspace bpf(2) emulation for the session; see bpfemu.c.
#pragma once

// Install the seccomp filter on the calling thread (inherited by all
// descendants). Returns the notification listener fd, or -1.
int bpfemu_install_filter(void);

// Serve notifications on the listener fd (passed as the pointer value).
// Never returns unless the listener breaks.
void* bpfemu_serve(void* listener_fd);
