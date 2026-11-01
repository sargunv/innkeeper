// SPDX-License-Identifier: AGPL-3.0-or-later
// Android-side low-level FMQ test. The PCM device is fake and does not
// access host audio.
#include <cstdio>
#include <cstdlib>

#include "StreamInBridge.h"

using namespace innkeeper::audio;
#define CHECK(c)                                      \
  do {                                                \
    if (!(c)) {                                       \
      std::fprintf(stderr, "%d: %s\n", __LINE__, #c); \
      std::exit(1);                                   \
    }                                                 \
  } while (0)

struct bridge_pcm {
  int generation;
};
static int opens = 0, closes = 0;
extern "C" bridge_pcm* bridge_pcm_open(
  bool input, uint32_t rate, uint32_t, uint32_t period_frames, uint32_t
) {
  CHECK(input && rate == 48000 && period_frames == 480);
  return new bridge_pcm{++opens};
}
extern "C" void bridge_pcm_close(bridge_pcm* p) {
  ++closes;
  delete p;
}
extern "C" int bridge_pcm_read(bridge_pcm* p, void* data, uint32_t bytes) {
  auto* samples = static_cast<int16_t*>(data);
  std::fill(samples, samples + bytes / 2, 100 + p->generation);
  return 0;
}

// Independent one-slot FMQ client, using the public descriptor's shared ABI.
// Command and reply each have one slot; the data queue is drained after bursts.
template <class T>
struct Client {
  uint8_t* map;
  size_t bytes;
  std::atomic<uint64_t>*r, *w;
  std::atomic<uint32_t>* event;
  T* data;
  explicit Client(const typename Queue<T>::Descriptor& d) {
    bytes = d.grantors[2].offset + d.grantors[2].extent;
    map = static_cast<uint8_t*>(mmap(
      nullptr, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, d.handle.fds[0].get(),
      0
    ));
    CHECK(map != MAP_FAILED);
    r = reinterpret_cast<std::atomic<uint64_t>*>(map + d.grantors[0].offset);
    w = reinterpret_cast<std::atomic<uint64_t>*>(map + d.grantors[1].offset);
    event =
      reinterpret_cast<std::atomic<uint32_t>*>(map + d.grantors[3].offset);
    data = reinterpret_cast<T*>(map + d.grantors[2].offset);
  }
  ~Client() { munmap(map, bytes); }
  void write(const T& value) {
    CHECK(w->load() == r->load());
    *data = value;
    w->fetch_add(sizeof(T), std::memory_order_release);
    event->fetch_or(2);
    syscall(SYS_futex, event, FUTEX_WAKE_BITSET, INT_MAX, nullptr, nullptr, 2);
  }
  T read() {
    for (int i = 0; w->load(std::memory_order_acquire) == r->load(); ++i) {
      CHECK(i < 20000);
      usleep(100);
    }
    T value = *data;
    r->fetch_add(sizeof(T), std::memory_order_release);
    return value;
  }
};
int main() {
  for (uint32_t channels : {1u, 2u}) {
    int previous = closes;
    auto muted = std::make_shared<std::atomic<bool>>(false);
    auto stream =
      ndk::SharedRefBase::make<StreamInBridge>(480, channels, muted);
    Descriptor desc;
    stream->describe(&desc);
    CHECK(desc.frameSizeBytes == static_cast<int>(channels * 2));
    Client<Descriptor::Command> command(desc.command);
    Client<Descriptor::Reply> reply(desc.reply);
    Client<int8_t> data(desc.audio.get<Descriptor::AudioBuffer::Tag::fmq>());
    auto send = [&](const Descriptor::Command& c) {
      command.write(c);
      return reply.read();
    };
    using Tag = Descriptor::Command::Tag;
    auto start = Descriptor::Command::make<Tag::start>(
      aidl::android::media::audio::common::Void{}
    );
    auto burst = Descriptor::Command::make<Tag::burst>(480 * channels * 2);
    auto capture = [&](int value) {
      auto result = send(burst);
      CHECK(
        result.status == 0 &&
        result.fmqByteCount == static_cast<int>(480 * channels * 2)
      );
      CHECK(result.state == Descriptor::State::ACTIVE);
      auto* samples = reinterpret_cast<int16_t*>(data.data);
      for (size_t i = 0; i < 480 * channels; ++i) CHECK(samples[i] == value);
      data.r->store(data.w->load(), std::memory_order_release);
    };
    CHECK(send(start).state == Descriptor::State::IDLE);
    capture(100 + opens);
    CHECK(
      send(
        Descriptor::Command::make<Tag::pause>(
          aidl::android::media::audio::common::Void{}
        )
      )
        .state == Descriptor::State::PAUSED
    );
    CHECK(
      send(
        Descriptor::Command::make<Tag::flush>(
          aidl::android::media::audio::common::Void{}
        )
      )
        .state == Descriptor::State::STANDBY
    );
    CHECK(closes == previous + 1);
    int old = opens;
    CHECK(send(start).state == Descriptor::State::IDLE && opens == old + 1);
    capture(100 + opens);
    *muted = true;
    capture(0);
    CHECK(send(Descriptor::Command::make<Tag::burst>(-1)).status == -EINVAL);
    CHECK(send(Descriptor::Command::make<Tag::burst>(1)).status == -EINVAL);
    CHECK(
      send(Descriptor::Command::make<Tag::burst>(480 * channels * 2 + 2))
        .status == -EINVAL
    );
    CHECK(stream->close().isOk() && closes == previous + 2);
    CHECK(stream->close().isOk() && closes == previous + 2);
  }
  std::puts("PASS input: mono/stereo, standby/reopen, mute, bounds, close");
}
