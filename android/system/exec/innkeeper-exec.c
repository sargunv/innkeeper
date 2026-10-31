// innkeeper-exec: run a command as a descendant of Android init.
//
//   innkeeper-exec --serve            (init service)
//   innkeeper-exec CMD [ARGS...]      (client; used by `innkeeper-session
//   exec`)
//
// Binder is served through the seccomp filter pivot-exec installs on init,
// which only init's descendants inherit. `innkeeper-session
// exec` joins the session's namespaces, so its processes have no filter and
// no binder (am, pm, cmd, dumpsys, service ...). The client instead hands its
// argv, environment, working directory and stdio fds (SCM_RIGHTS) to this
// server, which init started; the server forks the command with those fds,
// relays its exit status, and kills the command's process group if the client
// goes away. This is what adbd does for `adb shell`.
//
// Built static with the NDK (build:system). The socket lives on the
// session's private /dev, mode 0600 root, so only root inside the session can
// use it, and it grants nothing joining the namespaces doesn't already give.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

extern char** environ;

static const char kSocket[] = "/dev/socket/innkeeper-exec";

#define MAX_MSG (1 << 20)

static int send_all(int fd, const void* buf, size_t len) {
  const char* p = buf;
  while (len) {
    ssize_t n = send(fd, p, len, MSG_NOSIGNAL);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) return -1;
    p += n;
    len -= n;
  }
  return 0;
}

static int recv_all(int fd, void* buf, size_t len) {
  char* p = buf;
  while (len) {
    ssize_t n = recv(fd, p, len, 0);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) return -1;
    p += n;
    len -= n;
  }
  return 0;
}

// Message: u32 total length, then NUL-separated strings:
//   cwd \0 argc(decimal) \0 argv... \0 env... \0
// sent together with the client's fds 0, 1, 2.

static int client(int argc, char** argv) {
  const char* path = kSocket;
  int s = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  struct sockaddr_un sa = {.sun_family = AF_UNIX};
  snprintf(sa.sun_path, sizeof(sa.sun_path), "%s", path);
  if (s < 0 || connect(s, (struct sockaddr*)&sa, sizeof(sa)) != 0) {
    fprintf(stderr, "innkeeper-exec: connect %s: %s\n", path, strerror(errno));
    return 127;
  }
  char cwd[4096];
  if (!getcwd(cwd, sizeof(cwd))) strcpy(cwd, "/");
  size_t cap = 4096, len = 0;
  char* msg = malloc(cap);
#define PUT(str)                                        \
  do {                                                  \
    size_t l = strlen(str) + 1;                         \
    while (len + l > cap) msg = realloc(msg, cap *= 2); \
    memcpy(msg + len, str, l);                          \
    len += l;                                           \
  } while (0)
  char num[16];
  snprintf(num, sizeof(num), "%d", argc);
  PUT(cwd);
  PUT(num);
  for (int i = 0; i < argc; i++) PUT(argv[i]);
  for (char** e = environ; *e; e++) PUT(*e);
#undef PUT
  if (len > MAX_MSG) {
    fprintf(stderr, "innkeeper-exec: command too large\n");
    return 127;
  }
  uint32_t hdr = (uint32_t)len;
  int fds[3] = {0, 1, 2};
  for (int i = 0; i < 3; i++)
    if (fcntl(i, F_GETFD) < 0) fds[i] = open("/dev/null", O_RDWR | O_CLOEXEC);
  char cbuf[CMSG_SPACE(sizeof(fds))];
  memset(cbuf, 0, sizeof(cbuf));
  struct iovec iov = {.iov_base = &hdr, .iov_len = sizeof(hdr)};
  struct msghdr mh = {
    .msg_iov = &iov,
    .msg_iovlen = 1,
    .msg_control = cbuf,
    .msg_controllen = sizeof(cbuf)
  };
  struct cmsghdr* cm = CMSG_FIRSTHDR(&mh);
  cm->cmsg_level = SOL_SOCKET;
  cm->cmsg_type = SCM_RIGHTS;
  cm->cmsg_len = CMSG_LEN(sizeof(fds));
  memcpy(CMSG_DATA(cm), fds, sizeof(fds));
  if (sendmsg(s, &mh, MSG_NOSIGNAL) != sizeof(hdr) || send_all(s, msg, len)) {
    fprintf(stderr, "innkeeper-exec: send: %s\n", strerror(errno));
    return 127;
  }
  int32_t status;
  if (recv_all(s, &status, sizeof(status))) return 255;
  return status;
}

// Per-connection handler (a forked child of the server).
static void serve_one(int c) {
  uint32_t hdr;
  int fds[3] = {-1, -1, -1};
  char cbuf[CMSG_SPACE(sizeof(fds))];
  struct iovec iov = {.iov_base = &hdr, .iov_len = sizeof(hdr)};
  struct msghdr mh = {
    .msg_iov = &iov,
    .msg_iovlen = 1,
    .msg_control = cbuf,
    .msg_controllen = sizeof(cbuf)
  };
  if (recvmsg(c, &mh, MSG_CMSG_CLOEXEC) != sizeof(hdr)) _exit(1);
  struct cmsghdr* cm = CMSG_FIRSTHDR(&mh);
  if (
    !cm || cm->cmsg_type != SCM_RIGHTS || cm->cmsg_len != CMSG_LEN(sizeof(fds))
  )
    _exit(1);
  memcpy(fds, CMSG_DATA(cm), sizeof(fds));
  if (hdr == 0 || hdr > MAX_MSG) _exit(1);
  char* msg = malloc(hdr + 1);
  if (!msg || recv_all(c, msg, hdr)) _exit(1);
  msg[hdr] = 0;
  // Split into strings.
  size_t n = 0;
  for (size_t i = 0; i < hdr; i++)
    if (!msg[i]) n++;
  char** strs = calloc(n + 1, sizeof(char*));
  size_t k = 0;
  for (size_t i = 0; i < hdr && k < n; i += strlen(msg + i) + 1)
    strs[k++] = msg + i;
  if (k < 2) _exit(1);
  int argc = atoi(strs[1]);
  if (argc < 1 || (size_t)argc > k - 2) _exit(1);
  char** argv = calloc(argc + 1, sizeof(char*));
  for (int i = 0; i < argc; i++) argv[i] = strs[2 + i];
  char** envp = strs + 2 + argc;  // NULL-terminated by calloc

  pid_t pid = fork();
  if (pid < 0) _exit(1);
  if (pid == 0) {
    setpgid(0, 0);
    for (int i = 0; i < 3; i++) dup2(fds[i], i);
    signal(SIGPIPE, SIG_DFL);
    signal(SIGCHLD, SIG_DFL);
    if (chdir(strs[0]) != 0 && chdir("/") != 0) _exit(127);
    execvpe(argv[0], argv, envp);
    fprintf(stderr, "innkeeper-exec: %s: %s\n", argv[0], strerror(errno));
    _exit(errno == ENOENT ? 127 : 126);
  }
  for (int i = 0; i < 3; i++) close(fds[i]);
  // Wait for exit or disconnect together. Polling waitpid every 200 ms made
  // short commands (including boot readiness probes) wait out the whole tick.
  int pidfd = syscall(SYS_pidfd_open, pid, 0);
  struct pollfd watched[] = {
    {.fd = pidfd, .events = POLLIN},
    {.fd = c, .events = POLLRDHUP},
  };
  int ready;
  do {
    ready = pidfd < 0 ? -1 : poll(watched, 2, -1);
  } while (ready < 0 && errno == EINTR);
  int disconnected = ready < 0 || (watched[1].revents &
                                   (POLLHUP | POLLRDHUP | POLLERR | POLLNVAL));
  if (disconnected) {
    kill(-pid, SIGKILL);
    kill(pid, SIGKILL);
  }
  if (pidfd >= 0) close(pidfd);
  int status = 0;
  pid_t reaped;
  do {
    reaped = waitpid(pid, &status, 0);
  } while (reaped < 0 && errno == EINTR);
  if (disconnected || reaped < 0) _exit(0);
  int32_t rc = WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
  send_all(c, &rc, sizeof(rc));
  _exit(0);
}

static int server(void) {
  const char* path = kSocket;
  signal(SIGCHLD, SIG_IGN);  // reap connection handlers automatically
  signal(SIGPIPE, SIG_IGN);
  int s = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  struct sockaddr_un sa = {.sun_family = AF_UNIX};
  snprintf(sa.sun_path, sizeof(sa.sun_path), "%s", path);
  unlink(path);
  mode_t old = umask(0177);
  if (
    s < 0 || bind(s, (struct sockaddr*)&sa, sizeof(sa)) != 0 ||
    listen(s, 16) != 0
  ) {
    fprintf(stderr, "innkeeper-exec: listen %s: %s\n", path, strerror(errno));
    return 1;
  }
  umask(old);
  for (;;) {
    int c = accept4(s, NULL, NULL, SOCK_CLOEXEC);
    if (c < 0) {
      if (errno == EINTR || errno == ECONNABORTED) continue;
      return 1;
    }
    pid_t p = fork();
    if (p == 0) {
      close(s);
      signal(SIGCHLD, SIG_DFL);
      serve_one(c);
    }
    close(c);
  }
}

int main(int argc, char** argv) {
  if (argc == 2 && strcmp(argv[1], "--serve") == 0) return server();
  if (argc < 2) {
    fprintf(stderr, "usage: innkeeper-exec CMD [ARGS...] | --serve\n");
    return 2;
  }
  return client(argc - 1, argv + 1);
}
