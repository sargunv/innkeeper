/*
 * Decodes one H.264 frame with Vulkan Video on the selected driver and checks
 * every output sample.
 *
 * The test writes its own bitstream: an SPS, a PPS and an IDR slice whose
 * macroblocks are all I_PCM. I_PCM carries the samples uncoded, so the decoded
 * frame must equal the pattern exactly. The decode runs on a video decode
 * queue, and a graphics queue copies both planes to a host buffer.
 */
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <vulkan/vulkan.h>

#define CHECK(x)                                             \
  do {                                                       \
    VkResult r_ = (x);                                       \
    if (r_ != VK_SUCCESS) {                                  \
      printf("FAIL: %s = %d (line %d)\n", #x, r_, __LINE__); \
      exit(1);                                               \
    }                                                        \
  } while (0)

/* ---- the bitstream ------------------------------------------------------ */

struct bits {
  uint8_t buf[1 << 16];
  size_t len; /* whole bytes */
  uint32_t acc, nacc;
};

static void put(struct bits* b, uint32_t v, int n) {
  for (int i = n - 1; i >= 0; i--) {
    b->acc = b->acc << 1 | ((v >> i) & 1);
    if (++b->nacc == 8) {
      b->buf[b->len++] = (uint8_t)b->acc;
      b->acc = b->nacc = 0;
    }
  }
}

static void put_ue(struct bits* b, uint32_t v) {
  int n = 0;
  while ((v + 1) >> (n + 1)) n++;
  put(b, 0, n);
  put(b, v + 1, n + 1);
}

static void put_se(struct bits* b, int32_t v) {
  put_ue(b, v > 0 ? 2 * (uint32_t)v - 1 : 2 * (uint32_t)-v);
}

static void align_zero(struct bits* b) {
  while (b->nacc) put(b, 0, 1);
}

static void trailing(struct bits* b) {
  put(b, 1, 1);
  align_zero(b);
}

/* Appends a NAL unit with a start code, adding emulation prevention bytes. */
static void nal(struct bits* out, int ref_idc, int type, const struct bits* p) {
  static const uint8_t sc[] = {0, 0, 0, 1};
  memcpy(out->buf + out->len, sc, sizeof sc);
  out->len += sizeof sc;
  out->buf[out->len++] = (uint8_t)(ref_idc << 5 | type);
  int zeros = 0;
  for (size_t i = 0; i < p->len; i++) {
    if (zeros == 2 && p->buf[i] <= 3) {
      out->buf[out->len++] = 3;
      zeros = 0;
    }
    out->buf[out->len++] = p->buf[i];
    zeros = p->buf[i] ? 0 : zeros + 1;
  }
}

enum { PROFILE_MAIN = 77, LEVEL = 40, LOG2_MAX_FRAME_NUM = 4 };

/* The pattern, kept well inside the limited range. */
static uint8_t luma(uint32_t x, uint32_t y) {
  return (uint8_t)(16 + (x * 7 + y * 13) % 220);
}
static uint8_t cb(uint32_t x, uint32_t y) {
  return (uint8_t)(32 + (x * 5 + y * 3) % 192);
}
static uint8_t cr(uint32_t x, uint32_t y) {
  return (uint8_t)(48 + (x * 11 + y * 2) % 160);
}

static void write_stream(
  struct bits* out, uint32_t w, uint32_t h, struct bits* slice_out
) {
  struct bits* p = calloc(1, sizeof *p);

  /* SPS */
  put(p, PROFILE_MAIN, 8);
  put(p, 0, 8); /* constraint flags */
  put(p, LEVEL, 8);
  put_ue(p, 0);                      /* seq_parameter_set_id */
  put_ue(p, LOG2_MAX_FRAME_NUM - 4); /* log2_max_frame_num_minus4 */
  put_ue(p, 2);                      /* pic_order_cnt_type */
  put_ue(p, 1);                      /* max_num_ref_frames */
  put(p, 0, 1);                      /* gaps_in_frame_num_allowed */
  put_ue(p, w / 16 - 1);
  put_ue(p, h / 16 - 1);
  put(p, 1, 1); /* frame_mbs_only_flag */
  put(p, 1, 1); /* direct_8x8_inference_flag */
  put(p, 0, 1); /* frame_cropping_flag */
  put(p, 0, 1); /* vui_parameters_present_flag */
  trailing(p);
  nal(out, 3, 7, p);

  /* PPS */
  memset(p, 0, sizeof *p);
  put_ue(p, 0); /* pic_parameter_set_id */
  put_ue(p, 0); /* seq_parameter_set_id */
  put(p, 0, 1); /* entropy_coding_mode_flag: CAVLC */
  put(p, 0, 1); /* bottom_field_pic_order_in_frame_present_flag */
  put_ue(p, 0); /* num_slice_groups_minus1 */
  put_ue(p, 0); /* num_ref_idx_l0_default_active_minus1 */
  put_ue(p, 0); /* num_ref_idx_l1_default_active_minus1 */
  put(p, 0, 1); /* weighted_pred_flag */
  put(p, 0, 2); /* weighted_bipred_idc */
  put_se(p, 0); /* pic_init_qp_minus26 */
  put_se(p, 0); /* pic_init_qs_minus26 */
  put_se(p, 0); /* chroma_qp_index_offset */
  put(p, 1, 1); /* deblocking_filter_control_present_flag */
  put(p, 0, 1); /* constrained_intra_pred_flag */
  put(p, 0, 1); /* redundant_pic_cnt_present_flag */
  trailing(p);
  nal(out, 3, 8, p);

  /* IDR slice */
  memset(p, 0, sizeof *p);
  put_ue(p, 0);                  /* first_mb_in_slice */
  put_ue(p, 7);                  /* slice_type: I, for every slice */
  put_ue(p, 0);                  /* pic_parameter_set_id */
  put(p, 0, LOG2_MAX_FRAME_NUM); /* frame_num */
  put_ue(p, 0);                  /* idr_pic_id */
  put(p, 0, 1);                  /* no_output_of_prior_pics_flag */
  put(p, 0, 1);                  /* long_term_reference_flag */
  put_se(p, 0);                  /* slice_qp_delta */
  put_ue(p, 1);                  /* disable_deblocking_filter_idc */
  for (uint32_t my = 0; my < h / 16; my++) {
    for (uint32_t mx = 0; mx < w / 16; mx++) {
      put_ue(p, 25); /* mb_type: I_PCM */
      align_zero(p);
      for (uint32_t y = 0; y < 16; y++)
        for (uint32_t x = 0; x < 16; x++)
          put(p, luma(mx * 16 + x, my * 16 + y), 8);
      for (uint32_t y = 0; y < 8; y++)
        for (uint32_t x = 0; x < 8; x++) put(p, cb(mx * 8 + x, my * 8 + y), 8);
      for (uint32_t y = 0; y < 8; y++)
        for (uint32_t x = 0; x < 8; x++) put(p, cr(mx * 8 + x, my * 8 + y), 8);
    }
  }
  trailing(p);
  /* Vulkan wants the slice with a 3-byte start code. */
  slice_out->len = 0;
  nal(slice_out, 3, 5, p);
  memmove(slice_out->buf, slice_out->buf + 1, slice_out->len - 1);
  slice_out->len--;
  free(p);
}

/* ---- Vulkan ---------------------------------------------------------------
 */

static VkInstance inst;
static VkPhysicalDevice pdev;
static VkDevice dev;
static uint32_t dec_fam = UINT32_MAX, gfx_fam = UINT32_MAX;

static PFN_vkGetPhysicalDeviceVideoCapabilitiesKHR get_caps;
static PFN_vkGetPhysicalDeviceVideoFormatPropertiesKHR get_formats;
static PFN_vkCreateVideoSessionKHR create_session;
static PFN_vkGetVideoSessionMemoryRequirementsKHR session_mem_reqs;
static PFN_vkBindVideoSessionMemoryKHR bind_session_mem;
static PFN_vkCreateVideoSessionParametersKHR create_params;
static PFN_vkCmdBeginVideoCodingKHR cmd_begin;
static PFN_vkCmdControlVideoCodingKHR cmd_control;
static PFN_vkCmdDecodeVideoKHR cmd_decode;
static PFN_vkCmdEndVideoCodingKHR cmd_end;

static uint32_t memtype(uint32_t bits, VkMemoryPropertyFlags want) {
  VkPhysicalDeviceMemoryProperties mp;
  vkGetPhysicalDeviceMemoryProperties(pdev, &mp);
  for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
    if ((bits & 1u << i) && (mp.memoryTypes[i].propertyFlags & want) == want)
      return i;
  for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
    if (bits & 1u << i) return i;
  printf("FAIL: no memory type in 0x%x\n", bits);
  exit(1);
}

static VkDeviceMemory alloc_bind_image(VkImage img) {
  VkMemoryRequirements r;
  vkGetImageMemoryRequirements(dev, img, &r);
  VkMemoryAllocateInfo ai = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .allocationSize = r.size,
    .memoryTypeIndex =
      memtype(r.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT),
  };
  VkDeviceMemory m;
  CHECK(vkAllocateMemory(dev, &ai, NULL, &m));
  CHECK(vkBindImageMemory(dev, img, m, 0));
  return m;
}

static VkBuffer host_buffer(
  VkDeviceSize size, VkBufferUsageFlags usage, const void* pnext, void** map
) {
  VkBufferCreateInfo bi = {
    .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
    .pNext = pnext,
    .size = size,
    .usage = usage,
  };
  VkBuffer b;
  CHECK(vkCreateBuffer(dev, &bi, NULL, &b));
  VkMemoryRequirements r;
  vkGetBufferMemoryRequirements(dev, b, &r);
  VkMemoryAllocateInfo ai = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .allocationSize = r.size,
    .memoryTypeIndex = memtype(
      r.memoryTypeBits,
      VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT
    ),
  };
  VkDeviceMemory m;
  CHECK(vkAllocateMemory(dev, &ai, NULL, &m));
  CHECK(vkBindBufferMemory(dev, b, m, 0));
  CHECK(vkMapMemory(dev, m, 0, VK_WHOLE_SIZE, 0, map));
  return b;
}

static void setup(void) {
  VkApplicationInfo app = {
    .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
    .apiVersion = VK_API_VERSION_1_3,
  };
  VkInstanceCreateInfo ici = {
    .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
    .pApplicationInfo = &app,
  };
  CHECK(vkCreateInstance(&ici, NULL, &inst));
  uint32_t n = 1;
  VkResult r = vkEnumeratePhysicalDevices(inst, &n, &pdev);
  if ((r != VK_SUCCESS && r != VK_INCOMPLETE) || !n) {
    printf("FAIL: no physical device\n");
    exit(1);
  }
  VkPhysicalDeviceProperties props;
  vkGetPhysicalDeviceProperties(pdev, &props);
  printf("device: %s\n", props.deviceName);

  vkGetPhysicalDeviceQueueFamilyProperties2(pdev, &n, NULL);
  VkQueueFamilyProperties2 qf[16];
  VkQueueFamilyVideoPropertiesKHR qv[16];
  if (n > 16) n = 16;
  for (uint32_t i = 0; i < n; i++) {
    qv[i] = (VkQueueFamilyVideoPropertiesKHR){
      .sType = VK_STRUCTURE_TYPE_QUEUE_FAMILY_VIDEO_PROPERTIES_KHR,
    };
    qf[i] = (VkQueueFamilyProperties2){
      .sType = VK_STRUCTURE_TYPE_QUEUE_FAMILY_PROPERTIES_2,
      .pNext = &qv[i],
    };
  }
  vkGetPhysicalDeviceQueueFamilyProperties2(pdev, &n, qf);
  for (uint32_t i = 0; i < n; i++) {
    VkQueueFlags f = qf[i].queueFamilyProperties.queueFlags;
    if (gfx_fam == UINT32_MAX && (f & VK_QUEUE_GRAPHICS_BIT)) gfx_fam = i;
    if (
      dec_fam == UINT32_MAX && (f & VK_QUEUE_VIDEO_DECODE_BIT_KHR) &&
      (qv[i].videoCodecOperations &
       VK_VIDEO_CODEC_OPERATION_DECODE_H264_BIT_KHR)
    )
      dec_fam = i;
  }
  if (dec_fam == UINT32_MAX || gfx_fam == UINT32_MAX) {
    printf(
      "FAIL: no H.264 decode queue (decode %d, graphics %d)\n", (int)dec_fam,
      (int)gfx_fam
    );
    exit(1);
  }

  float prio = 1;
  VkDeviceQueueCreateInfo qci[2] = {
    {
      .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
      .queueFamilyIndex = dec_fam,
      .queueCount = 1,
      .pQueuePriorities = &prio,
    },
    {
      .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
      .queueFamilyIndex = gfx_fam,
      .queueCount = 1,
      .pQueuePriorities = &prio,
    },
  };
  const char* exts[] = {
    VK_KHR_VIDEO_QUEUE_EXTENSION_NAME,
    VK_KHR_VIDEO_DECODE_QUEUE_EXTENSION_NAME,
    VK_KHR_VIDEO_DECODE_H264_EXTENSION_NAME,
  };
  VkPhysicalDeviceVulkan13Features f13 = {
    .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
    .synchronization2 = VK_TRUE,
  };
  VkDeviceCreateInfo dci = {
    .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
    .pNext = &f13,
    .queueCreateInfoCount = 2,
    .pQueueCreateInfos = qci,
    .enabledExtensionCount = 3,
    .ppEnabledExtensionNames = exts,
  };
  CHECK(vkCreateDevice(pdev, &dci, NULL, &dev));

#define I(name) (PFN_##name) vkGetInstanceProcAddr(inst, #name)
#define D(name) (PFN_##name) vkGetDeviceProcAddr(dev, #name)
  get_caps = I(vkGetPhysicalDeviceVideoCapabilitiesKHR);
  get_formats = I(vkGetPhysicalDeviceVideoFormatPropertiesKHR);
  create_session = D(vkCreateVideoSessionKHR);
  session_mem_reqs = D(vkGetVideoSessionMemoryRequirementsKHR);
  bind_session_mem = D(vkBindVideoSessionMemoryKHR);
  create_params = D(vkCreateVideoSessionParametersKHR);
  cmd_begin = D(vkCmdBeginVideoCodingKHR);
  cmd_control = D(vkCmdControlVideoCodingKHR);
  cmd_decode = D(vkCmdDecodeVideoKHR);
  cmd_end = D(vkCmdEndVideoCodingKHR);
#undef I
#undef D
}

static uint32_t align_up(uint32_t v, uint32_t a) {
  return a ? (v + a - 1) / a * a : v;
}

static VkCommandBuffer begin_cmd(uint32_t fam, VkCommandPool* pool) {
  VkCommandPoolCreateInfo pci = {
    .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
    .queueFamilyIndex = fam,
  };
  CHECK(vkCreateCommandPool(dev, &pci, NULL, pool));
  VkCommandBufferAllocateInfo ai = {
    .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
    .commandPool = *pool,
    .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
    .commandBufferCount = 1,
  };
  VkCommandBuffer cmd;
  CHECK(vkAllocateCommandBuffers(dev, &ai, &cmd));
  VkCommandBufferBeginInfo bi = {
    .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
    .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
  };
  CHECK(vkBeginCommandBuffer(cmd, &bi));
  return cmd;
}

static void submit_wait(uint32_t fam, VkCommandBuffer cmd) {
  CHECK(vkEndCommandBuffer(cmd));
  VkQueue q;
  vkGetDeviceQueue(dev, fam, 0, &q);
  VkFence fence;
  VkFenceCreateInfo fci = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
  CHECK(vkCreateFence(dev, &fci, NULL, &fence));
  VkSubmitInfo si = {
    .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
    .commandBufferCount = 1,
    .pCommandBuffers = &cmd,
  };
  CHECK(vkQueueSubmit(q, 1, &si, fence));
  CHECK(vkWaitForFences(dev, 1, &fence, VK_TRUE, 5000000000ull));
}

static void barrier(
  VkCommandBuffer cmd, VkImage img, VkImageLayout from, VkImageLayout to,
  VkPipelineStageFlags2 src_stage, VkAccessFlags2 src_access,
  VkPipelineStageFlags2 dst_stage, VkAccessFlags2 dst_access
) {
  VkImageMemoryBarrier2 b = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
    .srcStageMask = src_stage,
    .srcAccessMask = src_access,
    .dstStageMask = dst_stage,
    .dstAccessMask = dst_access,
    .oldLayout = from,
    .newLayout = to,
    .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
    .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
    .image = img,
    .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1},
  };
  VkDependencyInfo di = {
    .sType = VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
    .imageMemoryBarrierCount = 1,
    .pImageMemoryBarriers = &b,
  };
  vkCmdPipelineBarrier2(cmd, &di);
}

int main(void) {
  setup();

  VkVideoDecodeH264ProfileInfoKHR h264_profile = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_PROFILE_INFO_KHR,
    .stdProfileIdc = STD_VIDEO_H264_PROFILE_IDC_MAIN,
    .pictureLayout = VK_VIDEO_DECODE_H264_PICTURE_LAYOUT_PROGRESSIVE_KHR,
  };
  VkVideoProfileInfoKHR profile = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_PROFILE_INFO_KHR,
    .pNext = &h264_profile,
    .videoCodecOperation = VK_VIDEO_CODEC_OPERATION_DECODE_H264_BIT_KHR,
    .chromaSubsampling = VK_VIDEO_CHROMA_SUBSAMPLING_420_BIT_KHR,
    .lumaBitDepth = VK_VIDEO_COMPONENT_BIT_DEPTH_8_BIT_KHR,
    .chromaBitDepth = VK_VIDEO_COMPONENT_BIT_DEPTH_8_BIT_KHR,
  };
  VkVideoProfileListInfoKHR profiles = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_PROFILE_LIST_INFO_KHR,
    .profileCount = 1,
    .pProfiles = &profile,
  };

  VkVideoDecodeH264CapabilitiesKHR h264_caps = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_CAPABILITIES_KHR,
  };
  VkVideoDecodeCapabilitiesKHR dec_caps = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_CAPABILITIES_KHR,
    .pNext = &h264_caps,
  };
  VkVideoCapabilitiesKHR caps = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_CAPABILITIES_KHR,
    .pNext = &dec_caps,
  };
  CHECK(get_caps(pdev, &profile, &caps));
  bool coincide =
    dec_caps.flags & VK_VIDEO_DECODE_CAPABILITY_DPB_AND_OUTPUT_COINCIDE_BIT_KHR;
  printf(
    "caps: coded %ux%u..%ux%u, granularity %ux%u, dpb %u, level %d, %s\n",
    caps.minCodedExtent.width, caps.minCodedExtent.height,
    caps.maxCodedExtent.width, caps.maxCodedExtent.height,
    caps.pictureAccessGranularity.width, caps.pictureAccessGranularity.height,
    caps.maxDpbSlots, h264_caps.maxLevelIdc,
    coincide ? "dpb and output coincide" : "distinct output"
  );

  uint32_t w = align_up(
    caps.minCodedExtent.width > 64 ? caps.minCodedExtent.width : 64, 16
  );
  uint32_t h = align_up(
    caps.minCodedExtent.height > 48 ? caps.minCodedExtent.height : 48, 16
  );

  /* The picture format. With distinct output, DPB and output formats are
   * queried separately, but both are NV12 for 8-bit 4:2:0. */
  VkPhysicalDeviceVideoFormatInfoKHR fi = {
    .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VIDEO_FORMAT_INFO_KHR,
    .pNext = &profiles,
    .imageUsage = VK_IMAGE_USAGE_VIDEO_DECODE_DPB_BIT_KHR |
                  (coincide ? VK_IMAGE_USAGE_VIDEO_DECODE_DST_BIT_KHR : 0),
  };
  uint32_t nfmt = 0;
  CHECK(get_formats(pdev, &fi, &nfmt, NULL));
  VkVideoFormatPropertiesKHR fmts[8];
  if (nfmt > 8) nfmt = 8;
  for (uint32_t i = 0; i < nfmt; i++)
    fmts[i] = (VkVideoFormatPropertiesKHR){
      .sType = VK_STRUCTURE_TYPE_VIDEO_FORMAT_PROPERTIES_KHR,
    };
  VkResult fr = get_formats(pdev, &fi, &nfmt, fmts);
  if ((fr != VK_SUCCESS && fr != VK_INCOMPLETE) || !nfmt) {
    printf("FAIL: no DPB format\n");
    return 1;
  }
  VkFormat fmt = fmts[0].format;
  if (fmt != VK_FORMAT_G8_B8R8_2PLANE_420_UNORM) {
    printf("FAIL: DPB format %d, expected NV12\n", fmt);
    return 1;
  }

  /* Session */
  VkVideoSessionCreateInfoKHR sci = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_SESSION_CREATE_INFO_KHR,
    .queueFamilyIndex = dec_fam,
    .pVideoProfile = &profile,
    .pictureFormat = fmt,
    .maxCodedExtent = {w, h},
    .referencePictureFormat = fmt,
    .maxDpbSlots = 1,
    .maxActiveReferencePictures = 0,
    .pStdHeaderVersion = &caps.stdHeaderVersion,
  };
  VkVideoSessionKHR session;
  CHECK(create_session(dev, &sci, NULL, &session));
  uint32_t nreq = 0;
  CHECK(session_mem_reqs(dev, session, &nreq, NULL));
  VkVideoSessionMemoryRequirementsKHR reqs[16];
  VkBindVideoSessionMemoryInfoKHR binds[16];
  if (nreq > 16) nreq = 16;
  for (uint32_t i = 0; i < nreq; i++)
    reqs[i] = (VkVideoSessionMemoryRequirementsKHR){
      .sType = VK_STRUCTURE_TYPE_VIDEO_SESSION_MEMORY_REQUIREMENTS_KHR,
    };
  CHECK(session_mem_reqs(dev, session, &nreq, reqs));
  for (uint32_t i = 0; i < nreq; i++) {
    VkMemoryAllocateInfo ai = {
      .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
      .allocationSize = reqs[i].memoryRequirements.size,
      .memoryTypeIndex = memtype(reqs[i].memoryRequirements.memoryTypeBits, 0),
    };
    VkDeviceMemory m;
    CHECK(vkAllocateMemory(dev, &ai, NULL, &m));
    binds[i] = (VkBindVideoSessionMemoryInfoKHR){
      .sType = VK_STRUCTURE_TYPE_BIND_VIDEO_SESSION_MEMORY_INFO_KHR,
      .memoryBindIndex = reqs[i].memoryBindIndex,
      .memory = m,
      .memorySize = ai.allocationSize,
    };
  }
  if (nreq) CHECK(bind_session_mem(dev, session, nreq, binds));

  /* Parameters, matching write_stream(). */
  StdVideoH264SequenceParameterSet sps = {
    .flags = {.frame_mbs_only_flag = 1, .direct_8x8_inference_flag = 1},
    .profile_idc = STD_VIDEO_H264_PROFILE_IDC_MAIN,
    .level_idc = STD_VIDEO_H264_LEVEL_IDC_4_0,
    .chroma_format_idc = STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
    .log2_max_frame_num_minus4 = LOG2_MAX_FRAME_NUM - 4,
    .pic_order_cnt_type = STD_VIDEO_H264_POC_TYPE_2,
    .max_num_ref_frames = 1,
    .pic_width_in_mbs_minus1 = w / 16 - 1,
    .pic_height_in_map_units_minus1 = h / 16 - 1,
  };
  StdVideoH264PictureParameterSet pps = {
    .flags = {.deblocking_filter_control_present_flag = 1},
    .weighted_bipred_idc = STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
  };
  VkVideoDecodeH264SessionParametersAddInfoKHR add = {
    .sType =
      VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_SESSION_PARAMETERS_ADD_INFO_KHR,
    .stdSPSCount = 1,
    .pStdSPSs = &sps,
    .stdPPSCount = 1,
    .pStdPPSs = &pps,
  };
  VkVideoDecodeH264SessionParametersCreateInfoKHR h264_pci = {
    .sType =
      VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_SESSION_PARAMETERS_CREATE_INFO_KHR,
    .maxStdSPSCount = 1,
    .maxStdPPSCount = 1,
    .pParametersAddInfo = &add,
  };
  VkVideoSessionParametersCreateInfoKHR pci = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_SESSION_PARAMETERS_CREATE_INFO_KHR,
    .pNext = &h264_pci,
    .videoSession = session,
  };
  VkVideoSessionParametersKHR params;
  CHECK(create_params(dev, &pci, NULL, &params));

  /* Images. Concurrent sharing lets the graphics queue read what the decode
   * queue wrote without ownership transfers. */
  uint32_t fams[2] = {dec_fam, gfx_fam};
  VkImageCreateInfo ici = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
    .pNext = &profiles,
    .imageType = VK_IMAGE_TYPE_2D,
    .format = fmt,
    .extent = {w, h, 1},
    .mipLevels = 1,
    .arrayLayers = 1,
    .samples = VK_SAMPLE_COUNT_1_BIT,
    .tiling = VK_IMAGE_TILING_OPTIMAL,
    .usage = VK_IMAGE_USAGE_VIDEO_DECODE_DPB_BIT_KHR |
             (coincide ? VK_IMAGE_USAGE_VIDEO_DECODE_DST_BIT_KHR |
                           VK_IMAGE_USAGE_TRANSFER_SRC_BIT
                       : 0),
    .sharingMode = VK_SHARING_MODE_CONCURRENT,
    .queueFamilyIndexCount = 2,
    .pQueueFamilyIndices = fams,
  };
  VkImage dpb, out;
  CHECK(vkCreateImage(dev, &ici, NULL, &dpb));
  alloc_bind_image(dpb);
  if (coincide) {
    out = dpb;
  } else {
    ici.usage =
      VK_IMAGE_USAGE_VIDEO_DECODE_DST_BIT_KHR | VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
    CHECK(vkCreateImage(dev, &ici, NULL, &out));
    alloc_bind_image(out);
  }
  VkImageViewCreateInfo vci = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
    .viewType = VK_IMAGE_VIEW_TYPE_2D,
    .format = fmt,
    .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1},
  };
  VkImageView dpb_view, out_view;
  vci.image = dpb;
  CHECK(vkCreateImageView(dev, &vci, NULL, &dpb_view));
  out_view = dpb_view;
  if (!coincide) {
    vci.image = out;
    CHECK(vkCreateImageView(dev, &vci, NULL, &out_view));
  }

  /* Bitstream */
  struct bits* headers = calloc(1, sizeof *headers);
  struct bits* slice = calloc(1, sizeof *slice);
  write_stream(headers, w, h, slice);
  VkDeviceSize bs_size = align_up(
    (uint32_t)slice->len, (uint32_t)caps.minBitstreamBufferSizeAlignment
  );
  void* bs_map;
  VkBuffer bs = host_buffer(
    bs_size, VK_BUFFER_USAGE_VIDEO_DECODE_SRC_BIT_KHR, &profiles, &bs_map
  );
  memset(bs_map, 0, bs_size);
  memcpy(bs_map, slice->buf, slice->len);
  printf("bitstream: %ux%u, slice %zu bytes\n", w, h, slice->len);

  /* Decode */
  VkCommandPool dec_pool;
  VkCommandBuffer cmd = begin_cmd(dec_fam, &dec_pool);
  barrier(
    cmd, dpb, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_VIDEO_DECODE_DPB_KHR,
    VK_PIPELINE_STAGE_2_NONE, 0, VK_PIPELINE_STAGE_2_VIDEO_DECODE_BIT_KHR,
    VK_ACCESS_2_VIDEO_DECODE_READ_BIT_KHR |
      VK_ACCESS_2_VIDEO_DECODE_WRITE_BIT_KHR
  );
  if (!coincide)
    barrier(
      cmd, out, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_VIDEO_DECODE_DST_KHR,
      VK_PIPELINE_STAGE_2_NONE, 0, VK_PIPELINE_STAGE_2_VIDEO_DECODE_BIT_KHR,
      VK_ACCESS_2_VIDEO_DECODE_WRITE_BIT_KHR
    );
  VkVideoPictureResourceInfoKHR dpb_res = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_PICTURE_RESOURCE_INFO_KHR,
    .codedExtent = {w, h},
    .imageViewBinding = dpb_view,
  };
  VkVideoPictureResourceInfoKHR out_res = dpb_res;
  out_res.imageViewBinding = out_view;
  VkVideoReferenceSlotInfoKHR begin_slot = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_REFERENCE_SLOT_INFO_KHR,
    .slotIndex = -1,
    .pPictureResource = &dpb_res,
  };
  VkVideoBeginCodingInfoKHR bci = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_BEGIN_CODING_INFO_KHR,
    .videoSession = session,
    .videoSessionParameters = params,
    .referenceSlotCount = 1,
    .pReferenceSlots = &begin_slot,
  };
  cmd_begin(cmd, &bci);
  VkVideoCodingControlInfoKHR cci = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_CODING_CONTROL_INFO_KHR,
    .flags = VK_VIDEO_CODING_CONTROL_RESET_BIT_KHR,
  };
  cmd_control(cmd, &cci);
  StdVideoDecodeH264ReferenceInfo std_ref = {0};
  VkVideoDecodeH264DpbSlotInfoKHR dpb_slot = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_DPB_SLOT_INFO_KHR,
    .pStdReferenceInfo = &std_ref,
  };
  VkVideoReferenceSlotInfoKHR setup_slot = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_REFERENCE_SLOT_INFO_KHR,
    .pNext = &dpb_slot,
    .slotIndex = 0,
    .pPictureResource = &dpb_res,
  };
  StdVideoDecodeH264PictureInfo std_pic = {
    .flags = {.is_intra = 1, .IdrPicFlag = 1, .is_reference = 1},
  };
  uint32_t slice_offset = 0;
  VkVideoDecodeH264PictureInfoKHR h264_pic = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_PICTURE_INFO_KHR,
    .pStdPictureInfo = &std_pic,
    .sliceCount = 1,
    .pSliceOffsets = &slice_offset,
  };
  VkVideoDecodeInfoKHR di = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_INFO_KHR,
    .pNext = &h264_pic,
    .srcBuffer = bs,
    .srcBufferOffset = 0,
    .srcBufferRange = bs_size,
    .dstPictureResource = out_res,
    .pSetupReferenceSlot = &setup_slot,
  };
  cmd_decode(cmd, &di);
  VkVideoEndCodingInfoKHR eci = {
    .sType = VK_STRUCTURE_TYPE_VIDEO_END_CODING_INFO_KHR,
  };
  cmd_end(cmd, &eci);
  submit_wait(dec_fam, cmd);

  /* Read back both planes. */
  void* rb_map;
  VkBuffer rb = host_buffer(
    (VkDeviceSize)w * h * 3 / 2, VK_BUFFER_USAGE_TRANSFER_DST_BIT, NULL, &rb_map
  );
  VkCommandPool gfx_pool;
  cmd = begin_cmd(gfx_fam, &gfx_pool);
  barrier(
    cmd, out,
    coincide ? VK_IMAGE_LAYOUT_VIDEO_DECODE_DPB_KHR
             : VK_IMAGE_LAYOUT_VIDEO_DECODE_DST_KHR,
    VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_PIPELINE_STAGE_2_NONE, 0,
    VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_READ_BIT
  );
  VkBufferImageCopy regions[2] = {
    {
      .bufferOffset = 0,
      .imageSubresource = {VK_IMAGE_ASPECT_PLANE_0_BIT, 0, 0, 1},
      .imageExtent = {w, h, 1},
    },
    {
      .bufferOffset = (VkDeviceSize)w * h,
      .imageSubresource = {VK_IMAGE_ASPECT_PLANE_1_BIT, 0, 0, 1},
      .imageExtent = {w / 2, h / 2, 1},
    },
  };
  vkCmdCopyImageToBuffer(
    cmd, out, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rb, 2, regions
  );
  submit_wait(gfx_fam, cmd);

  const uint8_t* y = rb_map;
  const uint8_t* uv = y + (size_t)w * h;
  int bad = 0;
  for (uint32_t j = 0; j < h; j++)
    for (uint32_t i = 0; i < w; i++)
      if (y[j * w + i] != luma(i, j) && bad++ < 5)
        printf("Y(%u,%u) = %u, expected %u\n", i, j, y[j * w + i], luma(i, j));
  for (uint32_t j = 0; j < h / 2; j++)
    for (uint32_t i = 0; i < w / 2; i++) {
      const uint8_t* p = uv + j * w + i * 2;
      if ((p[0] != cb(i, j) || p[1] != cr(i, j)) && bad++ < 10)
        printf(
          "CbCr(%u,%u) = %u,%u, expected %u,%u\n", i, j, p[0], p[1], cb(i, j),
          cr(i, j)
        );
    }
  if (bad) {
    printf("FAIL: %d samples differ\n", bad);
    return 1;
  }
  printf("PASS: decoded %ux%u H.264 matches\n", w, h);
  return 0;
}
