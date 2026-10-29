// SPDX-License-Identifier: AGPL-3.0-or-later
// The kernel permits unprivileged lowering only as far as the inherited
// oom_score_adj_min. Compress Android's range into [session floor, 1000].
static int linuxapk_oom_adj(int adj, int floor) {
  return floor + (adj + 1000) * (1000 - floor) / 2000;
}
