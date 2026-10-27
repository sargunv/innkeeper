// SPDX-License-Identifier: AGPL-3.0-or-later
// The composer's local socket (/dev/socket/linuxapk-input): broadcasts input
// events from the host compositor (wl_seat) and window events to every client
// as newline-terminated text records, and takes window manager commands from
// clients (protocol v2). The format is in docs/hal-display.md ("Input event
// stream").
#pragma once

#include <cstdint>
#include <functional>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace linuxapk::hwc {

class InputServer {
 public:
  static constexpr int kVersion = 2;

  InputServer() = default;
  ~InputServer();

  // Uses init's socket ($ANDROID_SOCKET_<basename>) if present, else binds
  // `path` itself. Starts the accept thread.
  bool start(const std::string& path);
  void stop();

  // Called on the server thread for every line a client sends (without the
  // '\n'). It may call send().
  void setCommandHandler(std::function<void(const std::string&)> f);

  // Display 0's size in pixels: sent in the hello line to new clients.
  void setDisplaySize(uint32_t w, uint32_t h);
  // A window's logical size and scale: sent now and to new clients.
  void setWindow(int32_t display, int32_t w, int32_t h, double scale);
  void removeWindow(int32_t display);

  // Sends one line (without '\n') to every client; slow clients are dropped.
  void send(const char* fmt, ...) __attribute__((format(printf, 2, 3)));

 private:
  struct Client {
    int fd;
    std::string in;  // partial command line
  };
  struct WindowState {
    int32_t w, h;
    double scale;
  };
  void loop();
  void sendLocked(const std::string& line);
  std::string helloLocked() const;

  std::mutex mMutex;
  int mListenFd = -1;
  int mWakeFd = -1;
  std::thread mThread;
  std::vector<Client> mClients;
  std::function<void(const std::string&)> mHandler;
  uint32_t mDisplayW = 0, mDisplayH = 0;
  std::map<int32_t, WindowState> mWindows;
};

}  // namespace linuxapk::hwc
