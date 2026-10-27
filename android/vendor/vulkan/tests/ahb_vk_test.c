/*
 * The Vulkan half of ANGLE's AHardwareBuffer import, step by step, for YUV
 * buffers: vkGetAndroidHardwareBufferPropertiesANDROID, then an image with the
 * external format, AHB memory import, bind, and a VkSamplerYcbcrConversion.
 * Prints where it fails (ANGLE only reports EGL_BAD_PARAMETER).
 */
#define VK_USE_PLATFORM_ANDROID_KHR 1
#include <android/hardware_buffer.h>
#include <stdio.h>
#include <string.h>
#include <vulkan/vulkan.h>

static VkDevice dev;
static VkPhysicalDevice pdev;

static int test(uint32_t format, const char* name) {
  AHardwareBuffer_Desc desc = {
    .width = 896,
    .height = 480,
    .layers = 1,
    .format = format,
    .usage = AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE |
             AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN,
  };
  AHardwareBuffer* ahb;
  if (AHardwareBuffer_allocate(&desc, &ahb)) {
    printf("%s: allocate failed\n", name);
    return 1;
  }
  VkAndroidHardwareBufferFormatPropertiesANDROID fp = {
    .sType =
      VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_FORMAT_PROPERTIES_ANDROID,
  };
  VkAndroidHardwareBufferPropertiesANDROID p = {
    .sType = VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_PROPERTIES_ANDROID,
    .pNext = &fp,
  };
  PFN_vkGetAndroidHardwareBufferPropertiesANDROID get_props = (void*)
    vkGetDeviceProcAddr(dev, "vkGetAndroidHardwareBufferPropertiesANDROID");
  VkResult r = get_props(dev, ahb, &p);
  printf(
    "%s: properties = %d; format %d externalFormat %llu features 0x%x model %d "
    "range %d "
    "chroma %d/%d allocationSize %llu memoryTypeBits 0x%x\n",
    name, r, fp.format, (unsigned long long)fp.externalFormat,
    fp.formatFeatures, fp.suggestedYcbcrModel, fp.suggestedYcbcrRange,
    fp.suggestedXChromaOffset, fp.suggestedYChromaOffset,
    (unsigned long long)p.allocationSize, p.memoryTypeBits
  );
  if (r) return 1;

  VkExternalFormatANDROID ext_fmt = {
    .sType = VK_STRUCTURE_TYPE_EXTERNAL_FORMAT_ANDROID,
    .externalFormat = fp.format == VK_FORMAT_UNDEFINED ? fp.externalFormat : 0,
  };
  VkExternalMemoryImageCreateInfo emi = {
    .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
    .pNext = &ext_fmt,
    .handleTypes =
      VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID,
  };
  VkImageCreateInfo ici = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
    .pNext = &emi,
    .imageType = VK_IMAGE_TYPE_2D,
    .format = fp.format,
    .extent = {896, 480, 1},
    .mipLevels = 1,
    .arrayLayers = 1,
    .samples = VK_SAMPLE_COUNT_1_BIT,
    .tiling = VK_IMAGE_TILING_OPTIMAL,
    .usage = VK_IMAGE_USAGE_SAMPLED_BIT,
  };
  VkImage img;
  r = vkCreateImage(dev, &ici, NULL, &img);
  printf("%s: vkCreateImage(external format) = %d\n", name, r);
  if (r) return 1;

  VkImportAndroidHardwareBufferInfoANDROID imp = {
    .sType = VK_STRUCTURE_TYPE_IMPORT_ANDROID_HARDWARE_BUFFER_INFO_ANDROID,
    .buffer = ahb,
  };
  VkMemoryDedicatedAllocateInfo ded = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
    .pNext = &imp,
    .image = img,
  };
  uint32_t type = __builtin_ctz(p.memoryTypeBits);
  VkMemoryAllocateInfo mai = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .pNext = &ded,
    .allocationSize = p.allocationSize,
    .memoryTypeIndex = type,
  };
  VkDeviceMemory mem;
  r = vkAllocateMemory(dev, &mai, NULL, &mem);
  printf("%s: vkAllocateMemory(import AHB) = %d\n", name, r);
  if (r) return 1;
  r = vkBindImageMemory(dev, img, mem, 0);
  printf("%s: vkBindImageMemory = %d\n", name, r);
  if (r) return 1;

  VkSamplerYcbcrConversionCreateInfo yc = {
    .sType = VK_STRUCTURE_TYPE_SAMPLER_YCBCR_CONVERSION_CREATE_INFO,
    .pNext = &ext_fmt,
    .format = fp.format,
    .ycbcrModel = fp.suggestedYcbcrModel,
    .ycbcrRange = fp.suggestedYcbcrRange,
    .components = fp.samplerYcbcrConversionComponents,
    .xChromaOffset = fp.suggestedXChromaOffset,
    .yChromaOffset = fp.suggestedYChromaOffset,
    .chromaFilter = VK_FILTER_NEAREST,
  };
  VkSamplerYcbcrConversion conv;
  r = vkCreateSamplerYcbcrConversion(dev, &yc, NULL, &conv);
  printf("%s: vkCreateSamplerYcbcrConversion = %d\n", name, r);
  return r != VK_SUCCESS;
}

int main(void) {
  setvbuf(stdout, NULL, _IOLBF, 0);
  VkApplicationInfo app = {
    .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
    .apiVersion = VK_API_VERSION_1_3
  };
  VkInstance inst;
  vkCreateInstance(
    &(VkInstanceCreateInfo){
      .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app
    },
    NULL, &inst
  );
  uint32_t n = 1;
  vkEnumeratePhysicalDevices(inst, &n, &pdev);
  const char* exts[] = {
    "VK_ANDROID_external_memory_android_hardware_buffer",
    "VK_KHR_external_memory",
    "VK_KHR_sampler_ycbcr_conversion",
    "VK_EXT_queue_family_foreign",
    "VK_KHR_dedicated_allocation",
  };
  VkPhysicalDeviceSamplerYcbcrConversionFeatures ycf = {
    .sType =
      VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SAMPLER_YCBCR_CONVERSION_FEATURES,
    .samplerYcbcrConversion = VK_TRUE,
  };
  float prio = 1;
  VkResult r = vkCreateDevice(
    pdev,
    &(VkDeviceCreateInfo){
      .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
      .pNext = &ycf,
      .queueCreateInfoCount = 1,
      .pQueueCreateInfos =
        &(VkDeviceQueueCreateInfo){
          .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
          .queueCount = 1,
          .pQueuePriorities = &prio,
        },
      .enabledExtensionCount = 5,
      .ppEnabledExtensionNames = exts,
    },
    NULL, &dev
  );
  if (r) {
    printf("vkCreateDevice = %d\n", r);
    return 1;
  }
  int fails = 0;
  fails += test(0x32315659, "YV12");
  fails += test(AHARDWAREBUFFER_FORMAT_Y8Cb8Cr8_420, "YCbCr_420_888");
  fails += test(0x11, "NV21");
  printf("%s\n", fails ? "SOME FAILED" : "ALL OK");
  return fails != 0;
}
