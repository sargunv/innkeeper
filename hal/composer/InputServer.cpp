// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <algorithm>
#include <cctype>

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#include "InputServer.h"

#include "linuxapk/log.h"

namespace linuxapk::hwc {

InputServer::~InputServer() { stop(); }

bool InputServer::start(const std::string& path) {
  // init-created socket: "socket linuxapk-input stream 0660 system input".
  // The fallback bind (tests without init) is owner-only.
  std::string base = path.substr(path.find_last_of('/') + 1);
  // init publishes ANDROID_SOCKET_<name> with non-alphanumerics mapped to '_'.
  std::string env = "ANDROID_SOCKET_" + base;
  for (char& c : env) {
    if (!isalnum(static_cast<unsigned char>(c))) c = '_';
  }
  if (const char* v = getenv(env.c_str())) {
    mListenFd = atoi(v);
    fcntl(mListenFd, F_SETFD, FD_CLOEXEC);
  } else {
    mListenFd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    if (path.size() >= sizeof(addr.sun_path)) return false;
    memcpy(addr.sun_path, path.c_str(), path.size() + 1);
    unlink(path.c_str());
    if (
      bind(mListenFd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) != 0
    ) {
      LOGE("input: bind %s: %s", path.c_str(), strerror(errno));
      close(mListenFd);
      mListenFd = -1;
      return false;
    }
    chmod(path.c_str(), 0600);
  }
  if (listen(mListenFd, 4) != 0) {
    LOGE("input: listen: %s", strerror(errno));
    return false;
  }
  mWakeFd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
  mThread = std::thread([this] { loop(); });
  LOGI("input: event stream on %s", path.c_str());
  return true;
}

void InputServer::stop() {
  if (mThread.joinable()) {
    uint64_t one = 1;
    (void)!write(mWakeFd, &one, sizeof one);
    mThread.join();
  }
  std::lock_guard lock(mMutex);
  for (auto& c : mClients) close(c.fd);
  mClients.clear();
  if (mListenFd >= 0) close(mListenFd);
  if (mWakeFd >= 0) close(mWakeFd);
  mListenFd = mWakeFd = -1;
}

void InputServer::setCommandHandler(std::function<void(const std::string&)> f) {
  std::lock_guard lock(mMutex);
  mHandler = std::move(f);
}

std::string InputServer::helloLocked() const {
  char buf[128];
  snprintf(buf, sizeof buf, "hello %d %u %u", kVersion, mDisplayW, mDisplayH);
  std::string s = buf;
  for (auto& [id, w] : mWindows) {
    snprintf(buf, sizeof buf, "\nwindow %d %d %.3f %d", w.w, w.h, w.scale, id);
    s += buf;
  }
  return s;
}

void InputServer::setDisplaySize(uint32_t w, uint32_t h) {
  std::lock_guard lock(mMutex);
  mDisplayW = w;
  mDisplayH = h;
}

void InputServer::setWindow(
  int32_t display, int32_t w, int32_t h, double scale
) {
  std::lock_guard lock(mMutex);
  auto it = mWindows.find(display);
  if (
    it != mWindows.end() && it->second.w == w && it->second.h == h &&
    it->second.scale == scale
  ) {
    return;
  }
  mWindows[display] = {w, h, scale};
  char buf[96];
  snprintf(buf, sizeof buf, "window %d %d %.3f %d", w, h, scale, display);
  sendLocked(buf);
}

void InputServer::removeWindow(int32_t display) {
  std::lock_guard lock(mMutex);
  mWindows.erase(display);
}

void InputServer::send(const char* fmt, ...) {
  char buf[512];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(buf, sizeof buf, fmt, ap);
  va_end(ap);
  std::lock_guard lock(mMutex);
  sendLocked(buf);
}

void InputServer::sendLocked(const std::string& line) {
  std::string msg = line + "\n";
  for (size_t i = 0; i < mClients.size();) {
    ssize_t n = ::send(
      mClients[i].fd, msg.data(), msg.size(), MSG_DONTWAIT | MSG_NOSIGNAL
    );
    if (n != ssize_t(msg.size())) {
      LOGW(
        "input: dropping client (%s)", n < 0 ? strerror(errno) : "short write"
      );
      close(mClients[i].fd);
      mClients.erase(mClients.begin() + long(i));
    } else {
      i++;
    }
  }
}

void InputServer::loop() {
  constexpr size_t kMaxLine = 4096;
  while (true) {
    std::vector<pollfd> fds = {{mWakeFd, POLLIN, 0}, {mListenFd, POLLIN, 0}};
    {
      std::lock_guard lock(mMutex);
      for (auto& c : mClients) fds.push_back({c.fd, POLLIN, 0});
    }
    if (poll(fds.data(), fds.size(), -1) < 0) {
      if (errno == EINTR) continue;
      return;
    }
    if (fds[0].revents) return;
    if (fds[1].revents & POLLIN) {
      int c = accept4(mListenFd, nullptr, nullptr, SOCK_CLOEXEC);
      if (c >= 0) {
        std::lock_guard lock(mMutex);
        std::string hello = helloLocked() + "\n";
        if (
          ::send(c, hello.data(), hello.size(), MSG_NOSIGNAL) ==
          ssize_t(hello.size())
        ) {
          mClients.push_back({c, {}});
          LOGI("input: client connected (%zu total)", mClients.size());
        } else {
          close(c);
        }
      }
    }
    // Command lines from clients; EOF closes the client. Handlers run
    // without the lock (they send replies).
    std::vector<std::string> lines;
    std::function<void(const std::string&)> handler;
    for (size_t i = 2; i < fds.size(); i++) {
      if (!fds[i].revents) continue;
      char buf[1024];
      ssize_t n = recv(fds[i].fd, buf, sizeof buf, MSG_DONTWAIT);
      if (n < 0 && (errno == EAGAIN || errno == EINTR)) continue;
      std::lock_guard lock(mMutex);
      auto it = std::find_if(mClients.begin(), mClients.end(), [&](auto& c) {
        return c.fd == fds[i].fd;
      });
      if (it == mClients.end()) continue;
      if (n <= 0) {
        close(it->fd);
        mClients.erase(it);
        LOGI("input: client disconnected (%zu left)", mClients.size());
        continue;
      }
      it->in.append(buf, size_t(n));
      size_t nl;
      while ((nl = it->in.find('\n')) != std::string::npos) {
        lines.push_back(it->in.substr(0, nl));
        it->in.erase(0, nl + 1);
      }
      if (it->in.size() > kMaxLine) it->in.clear();  // junk without newlines
      handler = mHandler;
    }
    if (handler) {
      for (auto& l : lines) handler(l);
    }
  }
}

}  // namespace linuxapk::hwc
