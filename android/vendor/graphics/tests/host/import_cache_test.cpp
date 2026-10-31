// SPDX-License-Identifier: AGPL-3.0-or-later
#include <cstdlib>

#include "ImportCache.h"

using innkeeper::hwc::ImportCache;
struct Image {
  int& alive;
  explicit Image(int& alive) : alive(alive) { ++alive; }
  ~Image() { --alive; }
};
#define CHECK(x)            \
  do {                      \
    if (!(x)) std::abort(); \
  } while (0)

int main() {
  int alive = 0, imported = 0;
  ImportCache<Image> cache;
  auto create = [&] {
    ++imported;
    return std::make_unique<Image>(alive);
  };
  cache.prepare(1, 2);
  auto* source = cache.get(1, create);
  auto* target = cache.get(2, create);
  for (int i = 0; i < 100; ++i) {
    cache.prepare(1, 2);
    CHECK(cache.get(1, create) == source);
    CHECK(cache.get(2, create) == target);
  }
  CHECK(imported == 2);
  // Replacing/resizing targets remains bounded and preserves the current
  // source.
  for (uint64_t id = 3; id < 100; ++id) {
    cache.prepare(1, id);
    CHECK(cache.get(1, create) == source);
    CHECK(cache.get(id, create) != nullptr);
    CHECK(alive <= 8 && cache.size() <= 8);
  }
  cache.prepare(100, 101);
  CHECK(cache.get(100, [] { return std::unique_ptr<Image>{}; }) == nullptr);
  CHECK(cache.get(100, create) != nullptr);  // Failed imports can be retried.
  CHECK(cache.get(101, create) != nullptr);
  CHECK(alive <= 8);
  cache.clear();
  CHECK(alive == 0);
  {  // Display teardown frees its imports too.
    ImportCache<Image> display;
    display.prepare(1, 1);
    display.get(1, create);
  }
  CHECK(alive == 0);
}
