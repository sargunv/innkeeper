// SPDX-License-Identifier: AGPL-3.0-or-later
// The keys follow AOSP's external (USB) camera HAL, which reports the same
// kind of device: a fixed-focus webcam with automatic exposure and white
// balance, hardware level EXTERNAL.
#include <cstring>

#include <system/camera_metadata.h>

#include "Metadata.h"

namespace innkeeper::camera {

namespace {

// A JPEG holds at most this many bytes, plus the CameraBlob trailer.
constexpr int32_t kJpegMaxSize = 1280 * 720 * 3 / 2 + 1024;
constexpr int64_t kFrameNs = 1'000'000'000LL / kMaxFps;

// The request and result keys a client may use.
const std::vector<int32_t> kRequestKeys = {
  ANDROID_COLOR_CORRECTION_ABERRATION_MODE,
  ANDROID_CONTROL_AE_ANTIBANDING_MODE,
  ANDROID_CONTROL_AE_EXPOSURE_COMPENSATION,
  ANDROID_CONTROL_AE_LOCK,
  ANDROID_CONTROL_AE_MODE,
  ANDROID_CONTROL_AE_PRECAPTURE_TRIGGER,
  ANDROID_CONTROL_AE_TARGET_FPS_RANGE,
  ANDROID_CONTROL_AF_MODE,
  ANDROID_CONTROL_AF_TRIGGER,
  ANDROID_CONTROL_AWB_LOCK,
  ANDROID_CONTROL_AWB_MODE,
  ANDROID_CONTROL_CAPTURE_INTENT,
  ANDROID_CONTROL_EFFECT_MODE,
  ANDROID_CONTROL_MODE,
  ANDROID_CONTROL_SCENE_MODE,
  ANDROID_CONTROL_VIDEO_STABILIZATION_MODE,
  ANDROID_FLASH_MODE,
  ANDROID_JPEG_ORIENTATION,
  ANDROID_JPEG_QUALITY,
  ANDROID_JPEG_THUMBNAIL_QUALITY,
  ANDROID_JPEG_THUMBNAIL_SIZE,
  ANDROID_NOISE_REDUCTION_MODE,
  ANDROID_SCALER_CROP_REGION,
  ANDROID_STATISTICS_FACE_DETECT_MODE,
};

const std::vector<int32_t> kResultKeys = {
  ANDROID_COLOR_CORRECTION_ABERRATION_MODE,
  ANDROID_CONTROL_AE_ANTIBANDING_MODE,
  ANDROID_CONTROL_AE_EXPOSURE_COMPENSATION,
  ANDROID_CONTROL_AE_LOCK,
  ANDROID_CONTROL_AE_MODE,
  ANDROID_CONTROL_AE_PRECAPTURE_TRIGGER,
  ANDROID_CONTROL_AE_STATE,
  ANDROID_CONTROL_AE_TARGET_FPS_RANGE,
  ANDROID_CONTROL_AF_MODE,
  ANDROID_CONTROL_AF_STATE,
  ANDROID_CONTROL_AF_TRIGGER,
  ANDROID_CONTROL_AWB_LOCK,
  ANDROID_CONTROL_AWB_MODE,
  ANDROID_CONTROL_AWB_STATE,
  ANDROID_CONTROL_CAPTURE_INTENT,
  ANDROID_CONTROL_EFFECT_MODE,
  ANDROID_CONTROL_MODE,
  ANDROID_CONTROL_SCENE_MODE,
  ANDROID_CONTROL_VIDEO_STABILIZATION_MODE,
  ANDROID_FLASH_MODE,
  ANDROID_FLASH_STATE,
  ANDROID_JPEG_ORIENTATION,
  ANDROID_JPEG_QUALITY,
  ANDROID_JPEG_THUMBNAIL_QUALITY,
  ANDROID_JPEG_THUMBNAIL_SIZE,
  ANDROID_LENS_STATE,
  ANDROID_NOISE_REDUCTION_MODE,
  ANDROID_REQUEST_PIPELINE_DEPTH,
  ANDROID_SCALER_CROP_REGION,
  ANDROID_SENSOR_TIMESTAMP,
  ANDROID_STATISTICS_FACE_DETECT_MODE,
  ANDROID_STATISTICS_HOT_PIXEL_MAP_MODE,
  ANDROID_STATISTICS_LENS_SHADING_MAP_MODE,
  ANDROID_STATISTICS_SCENE_FLICKER,
};

// Bytes per value of each TYPE_* (camera_metadata_type_size is a data
// symbol, which the libcamera_metadata stub can't provide).
size_t typeSize(int type) {
  switch (type) {
    case TYPE_BYTE:
      return 1;
    case TYPE_INT32:
    case TYPE_FLOAT:
      return 4;
    case TYPE_INT64:
    case TYPE_DOUBLE:
    case TYPE_RATIONAL:
      return 8;
    default:
      return 0;
  }
}

}  // namespace

std::vector<uint8_t> Metadata::serialize() const {
  size_t data = 0;
  for (const auto& [tag, e] : mEntries) {
    int type = get_camera_metadata_tag_type(tag);
    if (type < 0) return {};
    data += calculate_camera_metadata_entry_data_size(uint8_t(type), e.count);
  }
  camera_metadata_t* m = allocate_camera_metadata(mEntries.size(), data);
  if (!m) return {};
  for (const auto& [tag, e] : mEntries) {
    int type = get_camera_metadata_tag_type(tag);
    if (
      e.bytes.size() != e.count * typeSize(type) ||
      add_camera_metadata_entry(m, tag, e.bytes.data(), e.count) != 0
    ) {
      free_camera_metadata(m);
      return {};
    }
  }
  sort_camera_metadata(m);
  auto* p = reinterpret_cast<const uint8_t*>(m);
  std::vector<uint8_t> out(p, p + get_camera_metadata_size(m));
  free_camera_metadata(m);
  return out;
}

Metadata characteristics() {
  Metadata m;
  const Size max = kSizes[0];
  m.set<uint8_t>(
    ANDROID_COLOR_CORRECTION_AVAILABLE_ABERRATION_MODES,
    {ANDROID_COLOR_CORRECTION_ABERRATION_MODE_OFF}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_AE_AVAILABLE_ANTIBANDING_MODES,
    {ANDROID_CONTROL_AE_ANTIBANDING_MODE_AUTO}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_AE_AVAILABLE_MODES, {ANDROID_CONTROL_AE_MODE_ON}
  );
  m.set<int32_t>(
    ANDROID_CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES,
    {kMinFps, kMaxFps, kMaxFps, kMaxFps}
  );
  m.set<int32_t>(ANDROID_CONTROL_AE_COMPENSATION_RANGE, {0, 0});
  m.set<camera_metadata_rational_t>(
    ANDROID_CONTROL_AE_COMPENSATION_STEP, {{0, 1}}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_AE_LOCK_AVAILABLE, {ANDROID_CONTROL_AE_LOCK_AVAILABLE_FALSE}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_AF_AVAILABLE_MODES, {ANDROID_CONTROL_AF_MODE_OFF}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_AVAILABLE_EFFECTS, {ANDROID_CONTROL_EFFECT_MODE_OFF}
  );
  m.set<uint8_t>(ANDROID_CONTROL_AVAILABLE_MODES, {ANDROID_CONTROL_MODE_AUTO});
  m.set<uint8_t>(
    ANDROID_CONTROL_AVAILABLE_SCENE_MODES, {ANDROID_CONTROL_SCENE_MODE_DISABLED}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES,
    {ANDROID_CONTROL_VIDEO_STABILIZATION_MODE_OFF}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_AWB_AVAILABLE_MODES, {ANDROID_CONTROL_AWB_MODE_AUTO}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_AWB_LOCK_AVAILABLE,
    {ANDROID_CONTROL_AWB_LOCK_AVAILABLE_FALSE}
  );
  m.set<int32_t>(ANDROID_CONTROL_MAX_REGIONS, {0, 0, 0});
  m.set<uint8_t>(
    ANDROID_FLASH_INFO_AVAILABLE, {ANDROID_FLASH_INFO_AVAILABLE_FALSE}
  );
  m.set<uint8_t>(
    ANDROID_INFO_SUPPORTED_HARDWARE_LEVEL,
    {ANDROID_INFO_SUPPORTED_HARDWARE_LEVEL_EXTERNAL}
  );
  m.set<int32_t>(
    ANDROID_JPEG_AVAILABLE_THUMBNAIL_SIZES,
    {0, 0, 176, 144, 240, 144, 256, 144, 240, 160, 256, 154, 240, 240, 320, 240}
  );
  m.set<int32_t>(ANDROID_JPEG_MAX_SIZE, {kJpegMaxSize});
  m.set<uint8_t>(ANDROID_LENS_FACING, {ANDROID_LENS_FACING_EXTERNAL});
  m.set<float>(ANDROID_LENS_INFO_AVAILABLE_FOCAL_LENGTHS, {3.0f});
  m.set<uint8_t>(
    ANDROID_LENS_INFO_FOCUS_DISTANCE_CALIBRATION,
    {ANDROID_LENS_INFO_FOCUS_DISTANCE_CALIBRATION_UNCALIBRATED}
  );
  m.set<uint8_t>(
    ANDROID_NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES,
    {ANDROID_NOISE_REDUCTION_MODE_OFF}
  );
  m.set<uint8_t>(
    ANDROID_REQUEST_AVAILABLE_CAPABILITIES,
    {ANDROID_REQUEST_AVAILABLE_CAPABILITIES_BACKWARD_COMPATIBLE}
  );
  m.set<int32_t>(ANDROID_REQUEST_MAX_NUM_OUTPUT_STREAMS, {0, 2, 1});
  m.set<int32_t>(ANDROID_REQUEST_MAX_NUM_INPUT_STREAMS, {0});
  m.set<int32_t>(ANDROID_REQUEST_PARTIAL_RESULT_COUNT, {1});
  m.set<uint8_t>(ANDROID_REQUEST_PIPELINE_MAX_DEPTH, {4});
  m.set<float>(ANDROID_SCALER_AVAILABLE_MAX_DIGITAL_ZOOM, {1.0f});
  m.set<uint8_t>(
    ANDROID_SCALER_CROPPING_TYPE, {ANDROID_SCALER_CROPPING_TYPE_CENTER_ONLY}
  );

  std::vector<int32_t> configs;
  std::vector<int64_t> durations, stalls;
  for (int32_t f : {kFormatImplDefined, kFormatYcbcr420, kFormatBlob}) {
    for (const Size& s : kSizes) {
      configs.insert(
        configs.end(), {f, s.width, s.height,
                        ANDROID_SCALER_AVAILABLE_STREAM_CONFIGURATIONS_OUTPUT}
      );
      durations.insert(durations.end(), {f, s.width, s.height, kFrameNs});
      stalls.insert(
        stalls.end(), {f, s.width, s.height, f == kFormatBlob ? kFrameNs : 0}
      );
    }
  }
  m.set(ANDROID_SCALER_AVAILABLE_STREAM_CONFIGURATIONS, configs);
  m.set(ANDROID_SCALER_AVAILABLE_MIN_FRAME_DURATIONS, durations);
  m.set(ANDROID_SCALER_AVAILABLE_STALL_DURATIONS, stalls);

  m.set<int32_t>(
    ANDROID_SENSOR_INFO_ACTIVE_ARRAY_SIZE, {0, 0, max.width, max.height}
  );
  m.set<int32_t>(
    ANDROID_SENSOR_INFO_PRE_CORRECTION_ACTIVE_ARRAY_SIZE,
    {0, 0, max.width, max.height}
  );
  m.set<int32_t>(ANDROID_SENSOR_INFO_PIXEL_ARRAY_SIZE, {max.width, max.height});
  // A typical 1/4" webcam sensor.
  m.set<float>(ANDROID_SENSOR_INFO_PHYSICAL_SIZE, {3.6f, 2.0f});
  m.set<int64_t>(
    ANDROID_SENSOR_INFO_MAX_FRAME_DURATION, {1'000'000'000LL / kMinFps}
  );
  m.set<uint8_t>(
    ANDROID_SENSOR_INFO_TIMESTAMP_SOURCE,
    {ANDROID_SENSOR_INFO_TIMESTAMP_SOURCE_UNKNOWN}
  );
  m.set<int32_t>(ANDROID_SENSOR_ORIENTATION, {0});
  m.set<int32_t>(
    ANDROID_SENSOR_AVAILABLE_TEST_PATTERN_MODES,
    {ANDROID_SENSOR_TEST_PATTERN_MODE_OFF}
  );
  m.set<uint8_t>(ANDROID_SHADING_AVAILABLE_MODES, {ANDROID_SHADING_MODE_FAST});
  m.set<uint8_t>(
    ANDROID_STATISTICS_INFO_AVAILABLE_FACE_DETECT_MODES,
    {ANDROID_STATISTICS_FACE_DETECT_MODE_OFF}
  );
  m.set<int32_t>(ANDROID_STATISTICS_INFO_MAX_FACE_COUNT, {0});
  m.set<uint8_t>(
    ANDROID_STATISTICS_INFO_AVAILABLE_HOT_PIXEL_MAP_MODES,
    {ANDROID_STATISTICS_HOT_PIXEL_MAP_MODE_OFF}
  );
  m.set<uint8_t>(
    ANDROID_STATISTICS_INFO_AVAILABLE_LENS_SHADING_MAP_MODES,
    {ANDROID_STATISTICS_LENS_SHADING_MAP_MODE_OFF}
  );
  m.set<int32_t>(ANDROID_SYNC_MAX_LATENCY, {ANDROID_SYNC_MAX_LATENCY_UNKNOWN});
  m.set(ANDROID_REQUEST_AVAILABLE_REQUEST_KEYS, kRequestKeys);
  m.set(ANDROID_REQUEST_AVAILABLE_RESULT_KEYS, kResultKeys);
  // Every static key above, and this list itself.
  std::vector<int32_t> chars = {
    ANDROID_COLOR_CORRECTION_AVAILABLE_ABERRATION_MODES,
    ANDROID_CONTROL_AE_AVAILABLE_ANTIBANDING_MODES,
    ANDROID_CONTROL_AE_AVAILABLE_MODES,
    ANDROID_CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES,
    ANDROID_CONTROL_AE_COMPENSATION_RANGE,
    ANDROID_CONTROL_AE_COMPENSATION_STEP,
    ANDROID_CONTROL_AE_LOCK_AVAILABLE,
    ANDROID_CONTROL_AF_AVAILABLE_MODES,
    ANDROID_CONTROL_AVAILABLE_EFFECTS,
    ANDROID_CONTROL_AVAILABLE_MODES,
    ANDROID_CONTROL_AVAILABLE_SCENE_MODES,
    ANDROID_CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES,
    ANDROID_CONTROL_AWB_AVAILABLE_MODES,
    ANDROID_CONTROL_AWB_LOCK_AVAILABLE,
    ANDROID_CONTROL_MAX_REGIONS,
    ANDROID_FLASH_INFO_AVAILABLE,
    ANDROID_INFO_SUPPORTED_HARDWARE_LEVEL,
    ANDROID_JPEG_AVAILABLE_THUMBNAIL_SIZES,
    ANDROID_JPEG_MAX_SIZE,
    ANDROID_LENS_FACING,
    ANDROID_LENS_INFO_AVAILABLE_FOCAL_LENGTHS,
    ANDROID_LENS_INFO_FOCUS_DISTANCE_CALIBRATION,
    ANDROID_NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES,
    ANDROID_REQUEST_AVAILABLE_CAPABILITIES,
    ANDROID_REQUEST_MAX_NUM_OUTPUT_STREAMS,
    ANDROID_REQUEST_MAX_NUM_INPUT_STREAMS,
    ANDROID_REQUEST_PARTIAL_RESULT_COUNT,
    ANDROID_REQUEST_PIPELINE_MAX_DEPTH,
    ANDROID_SCALER_AVAILABLE_MAX_DIGITAL_ZOOM,
    ANDROID_SCALER_CROPPING_TYPE,
    ANDROID_SCALER_AVAILABLE_STREAM_CONFIGURATIONS,
    ANDROID_SCALER_AVAILABLE_MIN_FRAME_DURATIONS,
    ANDROID_SCALER_AVAILABLE_STALL_DURATIONS,
    ANDROID_SENSOR_INFO_ACTIVE_ARRAY_SIZE,
    ANDROID_SENSOR_INFO_PRE_CORRECTION_ACTIVE_ARRAY_SIZE,
    ANDROID_SENSOR_INFO_PIXEL_ARRAY_SIZE,
    ANDROID_SENSOR_INFO_PHYSICAL_SIZE,
    ANDROID_SENSOR_INFO_MAX_FRAME_DURATION,
    ANDROID_SENSOR_INFO_TIMESTAMP_SOURCE,
    ANDROID_SENSOR_ORIENTATION,
    ANDROID_SENSOR_AVAILABLE_TEST_PATTERN_MODES,
    ANDROID_SHADING_AVAILABLE_MODES,
    ANDROID_STATISTICS_INFO_AVAILABLE_FACE_DETECT_MODES,
    ANDROID_STATISTICS_INFO_MAX_FACE_COUNT,
    ANDROID_STATISTICS_INFO_AVAILABLE_HOT_PIXEL_MAP_MODES,
    ANDROID_STATISTICS_INFO_AVAILABLE_LENS_SHADING_MAP_MODES,
    ANDROID_SYNC_MAX_LATENCY,
    ANDROID_REQUEST_AVAILABLE_REQUEST_KEYS,
    ANDROID_REQUEST_AVAILABLE_RESULT_KEYS,
    ANDROID_REQUEST_AVAILABLE_CHARACTERISTICS_KEYS,
  };
  m.set(ANDROID_REQUEST_AVAILABLE_CHARACTERISTICS_KEYS, chars);
  return m;
}

std::vector<uint8_t> defaultRequest(int32_t requestTemplate) {
  uint8_t intent;
  switch (requestTemplate) {
    case 1:
      intent = ANDROID_CONTROL_CAPTURE_INTENT_PREVIEW;
      break;
    case 2:
      intent = ANDROID_CONTROL_CAPTURE_INTENT_STILL_CAPTURE;
      break;
    case 3:
      intent = ANDROID_CONTROL_CAPTURE_INTENT_VIDEO_RECORD;
      break;
    case 4:
      intent = ANDROID_CONTROL_CAPTURE_INTENT_VIDEO_SNAPSHOT;
      break;
    default:
      // ZERO_SHUTTER_LAG and MANUAL need capabilities a webcam lacks.
      return {};
  }
  const Size max = kSizes[0];
  Metadata m;
  m.set<uint8_t>(
    ANDROID_COLOR_CORRECTION_ABERRATION_MODE,
    {ANDROID_COLOR_CORRECTION_ABERRATION_MODE_OFF}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_AE_ANTIBANDING_MODE,
    {ANDROID_CONTROL_AE_ANTIBANDING_MODE_AUTO}
  );
  m.set<int32_t>(ANDROID_CONTROL_AE_EXPOSURE_COMPENSATION, {0});
  m.set<uint8_t>(ANDROID_CONTROL_AE_LOCK, {ANDROID_CONTROL_AE_LOCK_OFF});
  m.set<uint8_t>(ANDROID_CONTROL_AE_MODE, {ANDROID_CONTROL_AE_MODE_ON});
  m.set<uint8_t>(
    ANDROID_CONTROL_AE_PRECAPTURE_TRIGGER,
    {ANDROID_CONTROL_AE_PRECAPTURE_TRIGGER_IDLE}
  );
  // Video holds a steady rate; the others may slow down in the dark.
  int32_t lo = requestTemplate == 3 || requestTemplate == 4 ? kMaxFps : kMinFps;
  m.set<int32_t>(ANDROID_CONTROL_AE_TARGET_FPS_RANGE, {lo, kMaxFps});
  m.set<uint8_t>(ANDROID_CONTROL_AF_MODE, {ANDROID_CONTROL_AF_MODE_OFF});
  m.set<uint8_t>(ANDROID_CONTROL_AF_TRIGGER, {ANDROID_CONTROL_AF_TRIGGER_IDLE});
  m.set<uint8_t>(ANDROID_CONTROL_AWB_LOCK, {ANDROID_CONTROL_AWB_LOCK_OFF});
  m.set<uint8_t>(ANDROID_CONTROL_AWB_MODE, {ANDROID_CONTROL_AWB_MODE_AUTO});
  m.set<uint8_t>(ANDROID_CONTROL_CAPTURE_INTENT, {intent});
  m.set<uint8_t>(
    ANDROID_CONTROL_EFFECT_MODE, {ANDROID_CONTROL_EFFECT_MODE_OFF}
  );
  m.set<uint8_t>(ANDROID_CONTROL_MODE, {ANDROID_CONTROL_MODE_AUTO});
  m.set<uint8_t>(
    ANDROID_CONTROL_SCENE_MODE, {ANDROID_CONTROL_SCENE_MODE_DISABLED}
  );
  m.set<uint8_t>(
    ANDROID_CONTROL_VIDEO_STABILIZATION_MODE,
    {ANDROID_CONTROL_VIDEO_STABILIZATION_MODE_OFF}
  );
  m.set<uint8_t>(ANDROID_FLASH_MODE, {ANDROID_FLASH_MODE_OFF});
  m.set<int32_t>(ANDROID_JPEG_ORIENTATION, {0});
  m.set<uint8_t>(ANDROID_JPEG_QUALITY, {90});
  m.set<uint8_t>(ANDROID_JPEG_THUMBNAIL_QUALITY, {90});
  m.set<int32_t>(ANDROID_JPEG_THUMBNAIL_SIZE, {0, 0});
  m.set<uint8_t>(
    ANDROID_NOISE_REDUCTION_MODE, {ANDROID_NOISE_REDUCTION_MODE_OFF}
  );
  m.set<int32_t>(ANDROID_SCALER_CROP_REGION, {0, 0, max.width, max.height});
  m.set<uint8_t>(
    ANDROID_STATISTICS_FACE_DETECT_MODE,
    {ANDROID_STATISTICS_FACE_DETECT_MODE_OFF}
  );
  return m.serialize();
}

bool validMetadata(const std::vector<uint8_t>& blob) {
  if (blob.size() < sizeof(uint32_t) * 8) return false;
  size_t size = blob.size();
  return validate_camera_metadata_structure(
           reinterpret_cast<const camera_metadata_t*>(blob.data()), &size
         ) == 0;
}

std::vector<uint8_t> captureResult(
  const std::vector<uint8_t>& settings, int64_t timestampNs
) {
  Metadata m;
  if (!settings.empty()) {
    auto* src = reinterpret_cast<const camera_metadata_t*>(settings.data());
    size_t n = get_camera_metadata_entry_count(src);
    for (size_t i = 0; i < n; i++) {
      camera_metadata_ro_entry_t e;
      if (get_camera_metadata_ro_entry(src, i, &e) != 0) continue;
      size_t bytes = e.count * typeSize(e.type);
      if (bytes) m.setRaw(e.tag, e.count, e.data.u8, bytes);
    }
  }
  m.set<int64_t>(ANDROID_SENSOR_TIMESTAMP, {timestampNs});
  m.set<uint8_t>(
    ANDROID_CONTROL_AE_STATE, {ANDROID_CONTROL_AE_STATE_CONVERGED}
  );
  m.set<uint8_t>(ANDROID_CONTROL_AF_STATE, {ANDROID_CONTROL_AF_STATE_INACTIVE});
  m.set<uint8_t>(
    ANDROID_CONTROL_AWB_STATE, {ANDROID_CONTROL_AWB_STATE_CONVERGED}
  );
  m.set<uint8_t>(ANDROID_FLASH_STATE, {ANDROID_FLASH_STATE_UNAVAILABLE});
  m.set<uint8_t>(ANDROID_LENS_STATE, {ANDROID_LENS_STATE_STATIONARY});
  m.set<uint8_t>(ANDROID_REQUEST_PIPELINE_DEPTH, {2});
  m.set<uint8_t>(
    ANDROID_STATISTICS_HOT_PIXEL_MAP_MODE,
    {ANDROID_STATISTICS_HOT_PIXEL_MAP_MODE_OFF}
  );
  m.set<uint8_t>(
    ANDROID_STATISTICS_LENS_SHADING_MAP_MODE,
    {ANDROID_STATISTICS_LENS_SHADING_MAP_MODE_OFF}
  );
  m.set<uint8_t>(
    ANDROID_STATISTICS_SCENE_FLICKER, {ANDROID_STATISTICS_SCENE_FLICKER_NONE}
  );
  return m.serialize();
}

int jpegQuality(const std::vector<uint8_t>& settings) {
  if (settings.empty()) return 90;
  camera_metadata_ro_entry_t e;
  auto* src = reinterpret_cast<const camera_metadata_t*>(settings.data());
  if (
    find_camera_metadata_ro_entry(src, ANDROID_JPEG_QUALITY, &e) != 0 ||
    e.count != 1
  ) {
    return 90;
  }
  return e.data.u8[0];
}

}  // namespace innkeeper::camera
