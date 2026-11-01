// SPDX-License-Identifier: AGPL-3.0-or-later
#include <cstdio>
#include <cstdlib>

#include "Output.h"

#define CHECK(cond)                                                   \
  do {                                                                \
    if (!(cond)) {                                                    \
      std::fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #cond); \
      std::abort();                                                   \
    }                                                                 \
  } while (0)

int main() {
  using namespace innkeeper::audio;
  // The HAL must allow a 5 ms burst, which can engage AudioFlinger's FastMixer.
  CHECK(validBufferSize(240));
  CHECK(!validBufferSize(239));
  CHECK(!validBufferSize(-1));
  CHECK(validBufferSize(48000));
  CHECK(!validBufferSize(48001));
  CHECK(kPeriodFrames * kPeriods * 1000 / kRate == 10);
  CHECK(kFrameBytes == 4);
  // Report the whole downstream queue; round up, including partial
  // milliseconds.
  CHECK(latencyMs(0) == 0);
  CHECK(latencyMs(1) == 1);
  CHECK(latencyMs(48) == 1);
  CHECK(latencyMs(49) == 2);
  CHECK(latencyMs(1248) == 26);
  // Presentation follows the device, including a slow or fast device clock.
  CHECK(presentedFrames(48000, 480) == 47520);
  CHECK(presentedFrames(48000, 481) == 47519);
  CHECK(presentedFrames(48000, 479) == 47521);
  CHECK(presentedFrames(480, 960) == 0);
  CHECK(presentedFrames(0, 0) == 0);
}
