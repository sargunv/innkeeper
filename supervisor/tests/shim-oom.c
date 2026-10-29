// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <stdio.h>

#include "../shim/oom.h"

int main(void) {
  for (int floor = 0; floor <= 1000; ++floor) {
    assert(linuxapk_oom_adj(-1000, floor) == floor);
    assert(linuxapk_oom_adj(1000, floor) == 1000);
    int previous = floor;
    for (int adj = -1000; adj <= 1000; ++adj) {
      int mapped = linuxapk_oom_adj(adj, floor);
      assert(mapped >= previous && mapped >= floor && mapped <= 1000);
      previous = mapped;
    }
  }
  // A typical desktop floor: persistent < foreground < service < cached.
  assert(linuxapk_oom_adj(-900, 200) == 240);
  assert(linuxapk_oom_adj(0, 200) == 600);
  assert(linuxapk_oom_adj(500, 200) == 800);
  assert(linuxapk_oom_adj(900, 200) == 960);
  puts(
    "ok   OOM scores preserve order and allow foreground promotion above the "
    "floor"
  );
}
