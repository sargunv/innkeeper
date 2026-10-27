// SPDX-License-Identifier: AGPL-3.0-or-later
// Broadcasts input events from the host compositor (wl_seat) to local
// clients as newline-terminated text records on a Unix stream socket.
// The format is documented in docs/hal-display.md ("Input event stream").
#pragma once

#include <cstdint>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace linuxapk::hwc {

class InputServer {
 public:
  static constexpr int kVersion = 1;

  InputServer() = default;
  ~InputServer();

  // Uses init's socket ($ANDROID_SOCKET_<basename>) if present, else binds
  // `path` itself. Starts the accept thread.
  bool start(const std::string& path);
  void stop();

  // Display size in pixels: sent in the hello line to new clients.
  void setDisplaySize(uint32_t w, uint32_t h);
  // Logical window size and scale: sent now and to new clients.
  void setWindow(int32_t w, int32_t h, double scale);

  // Sends one line (without '\n') to every client; slow clients are dropped.
  void send(const char* fmt, ...) __attribute__((format(printf, 2, 3)));

 private:
  void loop();
  void sendLocked(const std::string& line);
  std::string helloLocked() const;

  std::mutex mMutex;
  int mListenFd = -1;
  int mWakeFd = -1;
  std::thread mThread;
  std::vector<int> mClients;
  uint32_t mDisplayW = 0, mDisplayH = 0;
  int32_t mWinW = 0, mWinH = 0;
  double mScale = 1.0;
};

}  // namespace linuxapk::hwc
