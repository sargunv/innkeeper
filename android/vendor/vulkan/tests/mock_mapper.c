/*
 * Test double for a stable-C IMapper v5 (mapper.<suffix>.so). It answers the
 * StandardMetadata queries that Mesa's u_gralloc makes, from a native_handle_t
 * with fds [dmabuf] and ints [MOCK_MAGIC, width, height, stride_bytes, fourcc].
 *
 * It is built as mapper.inkptest.so for the vulkan tests only and is not
 * installed. The metadata encoding here is what a real IMapper v5 must produce
 * (see
 * hardware/interfaces/graphics/mapper/stable-c/implutils/.../IMapperMetadataTypes.h).
 */
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#define MOCK_MAGIC 0x504b4e49 /* "INKP" */

typedef struct native_handle {
  int version, numFds, numInts;
  int data[];
} native_handle_t;
typedef const native_handle_t* buffer_handle_t;

typedef int32_t (*fn)(void);
typedef struct {
  fn importBuffer, freeBuffer, getTransportSize, lock, unlock;
  fn flushLockedBuffer, rereadLockedBuffer, getMetadata;
  int32_t (*getStandardMetadata)(buffer_handle_t, int64_t, void*, size_t);
  fn setMetadata, setStandardMetadata, listSupportedMetadataTypes;
  fn dumpBuffer, dumpAllBuffers, getReservedRegion;
} AIMapperV5;
typedef struct {
  _Alignas(max_align_t) uint32_t version;
  AIMapperV5 v5;
} AIMapper;

enum { AIMAPPER_ERROR_BAD_BUFFER = 2, AIMAPPER_ERROR_UNSUPPORTED = 7 };

struct writer {
  uint8_t* dst;
  size_t cap;
  size_t len;
};

static void put(struct writer* w, const void* p, size_t n) {
  if (w->len + n <= w->cap) memcpy(w->dst + w->len, p, n);
  w->len += n;
}
static void put_i64(struct writer* w, int64_t v) { put(w, &v, 8); }
static void put_str(struct writer* w, const char* s) {
  put_i64(w, (int64_t)strlen(s));
  put(w, s, strlen(s));
}
static void put_header(struct writer* w, int64_t type) {
  put_str(w, "android.hardware.graphics.common.StandardMetadataType");
  put_i64(w, type);
}
static void put_component(
  struct writer* w, int64_t type, int64_t off, int64_t bits
) {
  put_str(w, "android.hardware.graphics.common.PlaneLayoutComponentType");
  put_i64(w, type);
  put_i64(w, off);
  put_i64(w, bits);
}

static int32_t get_standard_metadata(
  buffer_handle_t h, int64_t type, void* dst, size_t cap
) {
  if (!h || h->numFds < 1 || h->numInts < 5 || h->data[h->numFds] != MOCK_MAGIC)
    return -AIMAPPER_ERROR_BAD_BUFFER;
  const int* ints = &h->data[h->numFds];
  int64_t width = ints[1], height = ints[2], stride = ints[3];
  uint32_t fourcc = (uint32_t)ints[4];
  struct writer w = {dst, dst ? cap : 0, 0};

  put_header(&w, type);
  switch (type) {
    case 3: /* WIDTH */
      put_i64(&w, width);
      break;
    case 4: /* HEIGHT */
      put_i64(&w, height);
      break;
    case 7: /* PIXEL_FORMAT_FOURCC */
      put(&w, &fourcc, 4);
      break;
    case 8: /* PIXEL_FORMAT_MODIFIER */
      put_i64(&w, 0 /* DRM_FORMAT_MOD_LINEAR */);
      break;
    case 15: /* PLANE_LAYOUTS: one RGBA8888 plane */
      put_i64(&w, 1);
      put_i64(&w, 4);
      put_component(&w, 1 << 10 /* R */, 0, 8);
      put_component(&w, 1 << 11 /* G */, 8, 8);
      put_component(&w, 1 << 12 /* B */, 16, 8);
      put_component(&w, 1 << 13 /* A */, 24, 8);
      put_i64(&w, 0);               /* offsetInBytes */
      put_i64(&w, 32);              /* sampleIncrementInBits */
      put_i64(&w, stride);          /* strideInBytes */
      put_i64(&w, width);           /* widthInSamples */
      put_i64(&w, height);          /* heightInSamples */
      put_i64(&w, stride * height); /* totalSizeInBytes */
      put_i64(&w, 1);               /* horizontalSubsampling */
      put_i64(&w, 1);               /* verticalSubsampling */
      break;
    case 17: /* DATASPACE */ {
      int32_t ds = 0;
      put(&w, &ds, 4);
      break;
    }
    default:
      return -AIMAPPER_ERROR_UNSUPPORTED;
  }
  return (int32_t)w.len;
}

static int32_t unsupported(void) { return AIMAPPER_ERROR_UNSUPPORTED; }

static AIMapper mapper = {
  .version = 5,
  .v5 = {
    unsupported,
    unsupported,
    unsupported,
    unsupported,
    unsupported,
    unsupported,
    unsupported,
    unsupported,
    get_standard_metadata,
    unsupported,
    unsupported,
    unsupported,
    unsupported,
    unsupported,
    unsupported,
  },
};

__attribute__((visibility("default"))) int32_t
AIMapper_loadIMapper(AIMapper** out) {
  *out = &mapper;
  return 0;
}
