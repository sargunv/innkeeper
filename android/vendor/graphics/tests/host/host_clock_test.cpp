// SPDX-License-Identifier: AGPL-3.0-or-later
#include <cstdlib>

#include "HostClock.h"
using innkeeper::hwc::HostClock;
#define CHECK(x)            \
  do {                      \
    if (!(x)) std::abort(); \
  } while (0)
int main() {
  HostClock c;
  constexpr int64_t t = 1'000'000'000;
  CHECK(c.period(t) == HostClock::kDefaultPeriod);
  CHECK(c.next(t, t) == t + HostClock::kDefaultPeriod);
  CHECK(c.update(t, 10'000'000, t + 2'000'000));
  CHECK(c.next(t + 2'000'000, t) == t + 10'000'000);
  CHECK(c.next(t + 35'000'000, t + 10'000'000) == t + 40'000'000);
  CHECK(c.next(t + 40'000'000, t + 40'000'000) == t + 50'000'000);
  // A new output's phase and period replace the previous output together.
  CHECK(c.update(t + 45'000'000, 8'333'333, t + 46'000'000));
  CHECK(c.period(t + 46'000'000) == 8'333'333);
  CHECK(c.next(t + 46'000'000, t + 40'000'000) == t + 53'333'333);

  CHECK(!c.update(t + 48'000'000, 8'333'333, t + 46'000'000));
  CHECK(!c.update(t + 47'000'000, 0, t + 47'000'000));
  // A static visible 120 Hz window that draws nothing for two seconds must not
  // change the output period or lose its phase.
  const int64_t silent = t + 2'000'000'000;
  CHECK(c.period(silent) == 8'333'333);
  CHECK((c.next(silent, silent) - (t + 45'000'000)) % 8'333'333 == 0);
  CHECK(c.next(silent, silent) > silent);
  CHECK(c.next(silent, silent) - silent <= 8'333'333);
  // A newly selected output can have an older phase anchor. The bridge
  // filters stale per-window feedback before selecting a source.
  CHECK(c.update(t, 10'000'000, silent));
  CHECK(c.period(silent) == 10'000'000);
  CHECK(c.update(0, 0, silent));
  CHECK(c.period(silent) == HostClock::kDefaultPeriod);
  CHECK(c.next(silent, silent) == silent + HostClock::kDefaultPeriod);
}
