// SPDX-License-Identifier: AGPL-3.0-or-later
// The kernel lets an unprivileged process lower a score only as far as the
// inherited oom_score_adj_min. This compresses Android's range into
// [session floor, 1000].
static int innkeeper_oom_adj(int adj, int floor) {
  return floor + (adj + 1000) * (1000 - floor) / 2000;
}
