// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
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
  std::string env = "ANDROID_SOCKET_" + base;
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
  for (int c : mClients) close(c);
  mClients.clear();
  if (mListenFd >= 0) close(mListenFd);
  if (mWakeFd >= 0) close(mWakeFd);
  mListenFd = mWakeFd = -1;
}

std::string InputServer::helloLocked() const {
  char buf[128];
  snprintf(buf, sizeof buf, "hello %d %u %u", kVersion, mDisplayW, mDisplayH);
  std::string s = buf;
  if (mWinW > 0) {
    snprintf(buf, sizeof buf, "\nwindow %d %d %.3f", mWinW, mWinH, mScale);
    s += buf;
  }
  return s;
}

void InputServer::setDisplaySize(uint32_t w, uint32_t h) {
  std::lock_guard lock(mMutex);
  mDisplayW = w;
  mDisplayH = h;
}

void InputServer::setWindow(int32_t w, int32_t h, double scale) {
  std::lock_guard lock(mMutex);
  if (w == mWinW && h == mWinH && scale == mScale) return;
  mWinW = w;
  mWinH = h;
  mScale = scale;
  char buf[96];
  snprintf(buf, sizeof buf, "window %d %d %.3f", w, h, scale);
  sendLocked(buf);
}

void InputServer::send(const char* fmt, ...) {
  char buf[256];
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
    ssize_t n =
      ::send(mClients[i], msg.data(), msg.size(), MSG_DONTWAIT | MSG_NOSIGNAL);
    if (n != ssize_t(msg.size())) {
      LOGW(
        "input: dropping client (%s)", n < 0 ? strerror(errno) : "short write"
      );
      close(mClients[i]);
      mClients.erase(mClients.begin() + long(i));
    } else {
      i++;
    }
  }
}

void InputServer::loop() {
  while (true) {
    std::vector<pollfd> fds = {{mWakeFd, POLLIN, 0}, {mListenFd, POLLIN, 0}};
    {
      std::lock_guard lock(mMutex);
      for (int c : mClients) fds.push_back({c, POLLIN, 0});
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
          mClients.push_back(c);
          LOGI("input: client connected (%zu total)", mClients.size());
        } else {
          close(c);
        }
      }
    }
    // Clients don't send anything; readable means EOF (or junk to discard).
    for (size_t i = 2; i < fds.size(); i++) {
      if (!fds[i].revents) continue;
      char junk[256];
      ssize_t n = recv(fds[i].fd, junk, sizeof junk, MSG_DONTWAIT);
      if (n <= 0 && !(n < 0 && errno == EAGAIN)) {
        std::lock_guard lock(mMutex);
        for (size_t j = 0; j < mClients.size(); j++) {
          if (mClients[j] == fds[i].fd) {
            close(fds[i].fd);
            mClients.erase(mClients.begin() + long(j));
            break;
          }
        }
      }
    }
  }
}

}  // namespace linuxapk::hwc
