// SPDX-License-Identifier: AGPL-3.0-or-later
// Allocate RGB render targets through the selected Vulkan driver. The driver
// chooses the tile layout and exports the allocation as the handle's pixel fd.
#include <vector>

#include <unistd.h>
#include <vulkan/vulkan.h>

#include "gpu_buffer.h"

#include "innkeeper/log.h"

namespace innkeeper {
namespace {
constexpr auto kUsage =
  VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_SAMPLED_BIT |
  VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
constexpr auto kHandle = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;

VkFormat format(int32_t f) {
  switch (f) {
    case fmt::RGBA_8888:
    case fmt::RGBX_8888:
      return VK_FORMAT_R8G8B8A8_UNORM;
    case fmt::BGRA_8888:
      return VK_FORMAT_B8G8R8A8_UNORM;
    case fmt::RGB_565:
      return VK_FORMAT_R5G6B5_UNORM_PACK16;
    case fmt::RGBA_1010102:
      return VK_FORMAT_A2B10G10R10_UNORM_PACK32;
    case fmt::RGBA_FP16:
      return VK_FORMAT_R16G16B16A16_SFLOAT;
    default:
      return VK_FORMAT_UNDEFINED;
  }
}

struct Context {
  VkInstance instance = VK_NULL_HANDLE;
  VkPhysicalDevice physical = VK_NULL_HANDLE;
  VkDevice device = VK_NULL_HANDLE;
  VkPhysicalDeviceMemoryProperties memory{};
  PFN_vkGetImageDrmFormatModifierPropertiesEXT modifier = nullptr;
  PFN_vkGetMemoryFdKHR exportFd = nullptr;
  Context() {
    const VkApplicationInfo app{
      .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
      .apiVersion = VK_API_VERSION_1_3
    };
    const VkInstanceCreateInfo ci{
      .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app
    };
    if (vkCreateInstance(&ci, nullptr, &instance) != VK_SUCCESS) return;
    uint32_t count = 1;
    if (
      vkEnumeratePhysicalDevices(instance, &count, &physical) != VK_SUCCESS ||
      !count
    )
      return;
    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(physical, &props);
    if (props.deviceType == VK_PHYSICAL_DEVICE_TYPE_CPU) return;
    const char* extensions[] = {
      VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME,
      VK_EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION_NAME,
      VK_EXT_IMAGE_DRM_FORMAT_MODIFIER_EXTENSION_NAME
    };
    const float priority = 1;
    const VkDeviceQueueCreateInfo queue{
      .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
      .queueFamilyIndex = 0,
      .queueCount = 1,
      .pQueuePriorities = &priority
    };
    const VkDeviceCreateInfo dc{
      .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
      .queueCreateInfoCount = 1,
      .pQueueCreateInfos = &queue,
      .enabledExtensionCount = 3,
      .ppEnabledExtensionNames = extensions
    };
    if (vkCreateDevice(physical, &dc, nullptr, &device) != VK_SUCCESS) return;
    modifier = reinterpret_cast<PFN_vkGetImageDrmFormatModifierPropertiesEXT>(
      vkGetDeviceProcAddr(device, "vkGetImageDrmFormatModifierPropertiesEXT")
    );
    exportFd = reinterpret_cast<PFN_vkGetMemoryFdKHR>(
      vkGetDeviceProcAddr(device, "vkGetMemoryFdKHR")
    );
    vkGetPhysicalDeviceMemoryProperties(physical, &memory);
    LOGI("GPU allocator: %s", props.deviceName);
  }
  ~Context() {
    if (device) vkDestroyDevice(device, nullptr);
    if (instance) vkDestroyInstance(instance, nullptr);
  }
};
}  // namespace

GpuBuffer allocateGpuBuffer(const AllocRequest& req, Layout* layout) {
  const VkFormat fmt = format(layout->resolvedFormat);
  if (fmt == VK_FORMAT_UNDEFINED) return {};
  static Context ctx;
  if (!ctx.device || !ctx.modifier || !ctx.exportFd) return {};
  VkDrmFormatModifierPropertiesListEXT list{
    .sType = VK_STRUCTURE_TYPE_DRM_FORMAT_MODIFIER_PROPERTIES_LIST_EXT
  };
  VkFormatProperties2 properties{
    .sType = VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_2, .pNext = &list
  };
  vkGetPhysicalDeviceFormatProperties2(ctx.physical, fmt, &properties);
  std::vector<VkDrmFormatModifierPropertiesEXT> supported(
    list.drmFormatModifierCount
  );
  list.pDrmFormatModifierProperties = supported.data();
  vkGetPhysicalDeviceFormatProperties2(ctx.physical, fmt, &properties);
  std::vector<uint64_t> modifiers;
  for (const auto& mod : supported) {
    // Compression may need auxiliary planes, and the handle has one RGB
    // plane.
    if (
      mod.drmFormatModifier && mod.drmFormatModifierPlaneCount == 1 &&
      (mod.drmFormatModifierTilingFeatures &
       VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT)
    )
      modifiers.push_back(mod.drmFormatModifier);
  }
  if (modifiers.empty()) return {};
  const VkExternalMemoryImageCreateInfo external{
    .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
    .handleTypes = kHandle
  };
  const VkImageDrmFormatModifierListCreateInfoEXT choices{
    .sType = VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_LIST_CREATE_INFO_EXT,
    .pNext = &external,
    .drmFormatModifierCount = uint32_t(modifiers.size()),
    .pDrmFormatModifiers = modifiers.data()
  };
  const VkImageCreateInfo imageInfo{
    .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
    .pNext = &choices,
    .imageType = VK_IMAGE_TYPE_2D,
    .format = fmt,
    .extent = {req.width, req.height, 1},
    .mipLevels = 1,
    .arrayLayers = 1,
    .samples = VK_SAMPLE_COUNT_1_BIT,
    .tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
    .usage = kUsage,
    .sharingMode = VK_SHARING_MODE_EXCLUSIVE
  };
  VkImage image;
  if (vkCreateImage(ctx.device, &imageInfo, nullptr, &image) != VK_SUCCESS)
    return {};
  VkMemoryDedicatedRequirements dedicated{
    .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_REQUIREMENTS
  };
  VkMemoryRequirements2 requirements{
    .sType = VK_STRUCTURE_TYPE_MEMORY_REQUIREMENTS_2, .pNext = &dedicated
  };
  const VkImageMemoryRequirementsInfo2 getRequirements{
    .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_REQUIREMENTS_INFO_2, .image = image
  };
  vkGetImageMemoryRequirements2(ctx.device, &getRequirements, &requirements);
  const auto& memory = requirements.memoryRequirements;
  uint32_t type = ctx.memory.memoryTypeCount;
  for (uint32_t i = 0; i < ctx.memory.memoryTypeCount; i++)
    if (
      (memory.memoryTypeBits & (1u << i)) &&
      (ctx.memory.memoryTypes[i].propertyFlags &
       (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
        VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)) ==
        (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
         VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
    ) {
      type = i;
      break;
    }
  VkDeviceMemory allocation = VK_NULL_HANDLE;
  // RADV requires a dedicated allocation for shareable tiled images.
  const VkMemoryDedicatedAllocateInfo dedicatedInfo{
    .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO, .image = image
  };
  const VkExportMemoryAllocateInfo exportInfo{
    .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
    .pNext = dedicated.requiresDedicatedAllocation ? &dedicatedInfo : nullptr,
    .handleTypes = kHandle
  };
  const VkMemoryAllocateInfo allocInfo{
    .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .pNext = &exportInfo,
    .allocationSize = memory.size,
    .memoryTypeIndex = type
  };
  int fd = -1;
  VkImageDrmFormatModifierPropertiesEXT actual{
    .sType = VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_PROPERTIES_EXT
  };
  VkSubresourceLayout plane{};
  if (
    type < ctx.memory.memoryTypeCount &&
    vkAllocateMemory(ctx.device, &allocInfo, nullptr, &allocation) ==
      VK_SUCCESS &&
    vkBindImageMemory(ctx.device, image, allocation, 0) == VK_SUCCESS &&
    ctx.modifier(ctx.device, image, &actual) == VK_SUCCESS
  ) {
    const VkMemoryGetFdInfoKHR fdInfo{
      .sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
      .memory = allocation,
      .handleType = kHandle
    };
    if (ctx.exportFd(ctx.device, &fdInfo, &fd) == VK_SUCCESS) {
      const VkImageSubresource sub{
        .aspectMask = VK_IMAGE_ASPECT_MEMORY_PLANE_0_BIT_EXT
      };
      vkGetImageSubresourceLayout(ctx.device, image, &sub, &plane);
    }
  }
  vkDestroyImage(ctx.device, image, nullptr);
  if (allocation) vkFreeMemory(ctx.device, allocation, nullptr);
  if (fd < 0) return {};
  Layout l = *layout;
  const uint32_t bpp = l.planes[0].stride / l.stridePixels;
  l.planes[0].offset = plane.offset;
  l.planes[0].stride = uint32_t(plane.rowPitch);
  l.planes[0].size = plane.size;
  l.stridePixels = uint32_t(plane.rowPitch) / bpp;
  l.layerSize = memory.size;
  l.pixelBytes = memory.size;
  *layout = l;
  LOGD(
    "GPU buffer %.*s modifier=0x%llx pitch=%u size=%llu", int(req.name.size()),
    req.name.data(), (unsigned long long)actual.drmFormatModifier,
    l.planes[0].stride, (unsigned long long)memory.size
  );
  return {fd, actual.drmFormatModifier};
}
}  // namespace innkeeper
