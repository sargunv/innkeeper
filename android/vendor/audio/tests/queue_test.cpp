// SPDX-License-Identifier: AGPL-3.0-or-later
#include <cstdio>
#include <cstdlib>
#include <thread>

#include "Queue.h"

#include "Input.h"

#define CHECK(cond)                                                   \
  do {                                                                \
    if (!(cond)) {                                                    \
      std::fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #cond); \
      std::abort();                                                   \
    }                                                                 \
  } while (0)

int main() {
  using innkeeper::audio::Queue;
  using namespace innkeeper::audio::input;
  CHECK(validBufferSize(480));
  CHECK(!validBufferSize(479) && !validBufferSize(48001));
  CHECK(validBurst(960, 2, 960));
  CHECK(!validBurst(-2, 2, 960));
  CHECK(!validBurst(959, 2, 960));
  CHECK(!validBurst(964, 4, 960));
  Queue<uint32_t> queue(3);
  auto descriptor = queue.descriptor();
  CHECK(descriptor.quantum == 4 && descriptor.flags == 1);
  auto* map = static_cast<uint8_t*>(mmap(
    nullptr, 36, PROT_READ | PROT_WRITE, MAP_SHARED,
    descriptor.handle.fds[0].get(), 0
  ));
  CHECK(map != MAP_FAILED);
  auto* read = reinterpret_cast<std::atomic<uint64_t>*>(
    map + descriptor.grantors[0].offset
  );
  auto* write = reinterpret_cast<std::atomic<uint64_t>*>(
    map + descriptor.grantors[1].offset
  );
  auto* data = reinterpret_cast<uint32_t*>(map + descriptor.grantors[2].offset);
  auto* event = reinterpret_cast<std::atomic<uint32_t>*>(
    map + descriptor.grantors[3].offset
  );
  // An independent FMQ client publishes byte offsets, not element indices.
  data[0] = 11;
  data[1] = 22;
  data[2] = 33;
  write->store(12, std::memory_order_release);
  CHECK(queue.available() == 3);
  CHECK(!queue.write(44));
  uint32_t values[2];
  CHECK(queue.read(values, 2));
  CHECK(values[0] == 11 && values[1] == 22);
  CHECK(read->load() == 8);
  CHECK(queue.write(44));
  CHECK(queue.write(55));
  uint32_t wrapped[3];
  CHECK(queue.read(wrapped, 3));
  CHECK(wrapped[0] == 33 && wrapped[1] == 44 && wrapped[2] == 55);
  CHECK(!queue.read(values, 1));
  uint32_t capture[] = {71, 72, 73, 74};
  CHECK(!queue.write(capture, 4));
  CHECK(queue.write(capture, 2));
  CHECK(!queue.write(capture, 2));
  CHECK(queue.available() == 2);
  CHECK(queue.read(values, 1) && values[0] == 71);
  // Capture publishes a block across the ring end, leaving unread data intact.
  CHECK(queue.write(capture + 2, 2));
  CHECK(queue.read(wrapped, 3));
  CHECK(wrapped[0] == 72 && wrapped[1] == 73 && wrapped[2] == 74);
  std::atomic<bool> closed{false};
  uint32_t received = 0;
  std::thread reader([&] { CHECK(queue.readBlocking(&received, closed)); });
  // Publish from the client mapping and use the standard shared EventFlag.
  data[(write->load() / 4) % 3] = 66;
  write->fetch_add(4, std::memory_order_release);
  event->fetch_or(2);
  syscall(SYS_futex, event, FUTEX_WAKE_BITSET, INT_MAX, nullptr, nullptr, 2);
  reader.join();
  CHECK(received == 66);
  std::thread waiting([&] { CHECK(!queue.readBlocking(&received, closed)); });
  closed = true;
  queue.wake(2);
  waiting.join();
  munmap(map, 36);
}
