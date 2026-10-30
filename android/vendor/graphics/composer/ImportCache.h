// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <cstdint>
#include <map>
#include <memory>

namespace linuxapk::hwc {
// Per-display GPU imports. Reserve both ends of a blit together so importing
// its destination cannot destroy the source. Destruction happens with GL bound.
template <typename T>
class ImportCache {
 public:
  void prepare(uint64_t source, uint64_t target) {
    const size_t missing = !mImages.contains(source) +
                           (target != source && !mImages.contains(target));
    for (auto it = mImages.begin(); mImages.size() + missing > kLimit;) {
      if (it->first != source && it->first != target)
        it = mImages.erase(it);
      else
        ++it;
    }
  }

  template <typename Create>
  T* get(uint64_t id, Create create) {
    auto it = mImages.find(id);
    if (it != mImages.end()) return it->second.get();
    auto image = create();
    if (!image) return nullptr;
    return mImages.emplace(id, std::move(image)).first->second.get();
  }

  void clear() { mImages.clear(); }
  size_t size() const { return mImages.size(); }

 private:
  static constexpr size_t kLimit = 8;
  std::map<uint64_t, std::unique_ptr<T>> mImages;
};
}  // namespace linuxapk::hwc
