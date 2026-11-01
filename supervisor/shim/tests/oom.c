// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <stdio.h>

#include "../oom.h"

int main(void) {
  for (int floor = 0; floor <= 1000; ++floor) {
    assert(innkeeper_oom_adj(-1000, floor) == floor);
    assert(innkeeper_oom_adj(1000, floor) == 1000);
    int previous = floor;
    for (int adj = -1000; adj <= 1000; ++adj) {
      int mapped = innkeeper_oom_adj(adj, floor);
      assert(mapped >= previous && mapped >= floor && mapped <= 1000);
      previous = mapped;
    }
  }
  // With a typical desktop floor, persistent < foreground < service < cached.
  assert(innkeeper_oom_adj(-900, 200) == 240);
  assert(innkeeper_oom_adj(0, 200) == 600);
  assert(innkeeper_oom_adj(500, 200) == 800);
  assert(innkeeper_oom_adj(900, 200) == 960);
  puts(
    "ok   OOM scores preserve order and allow foreground promotion above the "
    "floor"
  );
}
