/*
 * Runs an acquire/present loop through OpenHarmony's Vulkan loader and its
 * swapchain layer, which drives the driver's VK_OHOS_native_buffer. The
 * window is the producer of an in-process consumer surface (OH_NativeImage),
 * and its buffers come from the system's buffer VDI.
 *
 *   ohos_swapchain_test [--frames N]
 *
 * Each frame clears the acquired image to a frame-specific color and presents
 * it. The consumer side acquires the buffer, waits on its fence, maps it and
 * checks the color. The program prints the device name first, so a caller
 * can check which driver ran. It runs as root in an OpenHarmony profile.
 */
#define VK_USE_PLATFORM_OHOS 1
#include <dlfcn.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

#define W 320
#define H 200

#define CHECK(x)                                             \
  do {                                                       \
    VkResult r_ = (x);                                       \
    if (r_ != VK_SUCCESS) {                                  \
      printf("FAIL: %s = %d (line %d)\n", #x, r_, __LINE__); \
      exit(1);                                               \
    }                                                        \
  } while (0)

/* OpenHarmony's BufferHandle (display buffer_handle.h). */
struct BufferHandle {
  int32_t fd;
  int32_t width;
  int32_t stride;
  int32_t height;
  int32_t size;
  int32_t format;
  uint64_t usage;
  void* virAddr;
  uint64_t phyAddr;
  uint32_t reserveFds;
  uint32_t reserveInts;
  int32_t reserve[];
};

/* BufferUsage (display_type.h). */
#define BUFFER_USAGE_CPU_READ (1ull << 0)
#define BUFFER_USAGE_MEM_DMA (1ull << 3)
#define BUFFER_USAGE_HW_RENDER (1ull << 8)
#define BUFFER_USAGE_HW_TEXTURE (1ull << 9)

struct OH_NativeImage;
struct NativeWindowBuffer;
static struct OH_NativeImage* (*ConsumerSurface_Create)(void);
static int32_t (*ConsumerSurface_SetDefaultUsage)(
  struct OH_NativeImage*, uint64_t
);
static int32_t (*ConsumerSurface_SetDefaultSize)(
  struct OH_NativeImage*, int32_t, int32_t
);
static OHNativeWindow* (*NativeImage_AcquireNativeWindow)(
  struct OH_NativeImage*
);
static int32_t (*NativeImage_AcquireNativeWindowBuffer)(
  struct OH_NativeImage*, struct NativeWindowBuffer**, int*
);
static int32_t (*NativeImage_ReleaseNativeWindowBuffer)(
  struct OH_NativeImage*, struct NativeWindowBuffer*, int
);
static struct BufferHandle* (*GetBufferHandleFromNative)(
  struct NativeWindowBuffer*
);

static void* sym(void* lib, const char* name) {
  void* p = dlsym(lib, name);
  if (!p) {
    printf("FAIL: %s: %s\n", name, dlerror());
    exit(1);
  }
  return p;
}

static void* open_lib(const char* path) {
  void* lib = dlopen(path, RTLD_NOW);
  if (!lib) {
    printf("FAIL: %s\n", dlerror());
    exit(1);
  }
  return lib;
}

static void frame_color(int f, uint8_t rgba[4]) {
  rgba[0] = (uint8_t)(37 * f + 11);
  rgba[1] = (uint8_t)(91 * f + 5);
  rgba[2] = (uint8_t)(173 * f + 29);
  rgba[3] = 255;
}

#define VK(name) PFN_vk##name vk##name = (PFN_vk##name)gipa(inst, "vk" #name)

int main(int argc, char** argv) {
  int frames = 30;
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "--frames") && i + 1 < argc) frames = atoi(argv[++i]);
  }
  setvbuf(stdout, NULL, _IOLBF, 0);

  void* image_lib = open_lib("/system/lib64/platformsdk/libnative_image.so");
  void* window_lib =
    open_lib("/system/lib64/chipset-sdk-sp/libnative_window.so");
  void* vulkan_lib = open_lib("/system/lib64/platformsdk/libvulkan.so");
  ConsumerSurface_Create = sym(image_lib, "OH_ConsumerSurface_Create");
  ConsumerSurface_SetDefaultUsage =
    sym(image_lib, "OH_ConsumerSurface_SetDefaultUsage");
  ConsumerSurface_SetDefaultSize =
    sym(image_lib, "OH_ConsumerSurface_SetDefaultSize");
  NativeImage_AcquireNativeWindow =
    sym(image_lib, "OH_NativeImage_AcquireNativeWindow");
  NativeImage_AcquireNativeWindowBuffer =
    sym(image_lib, "OH_NativeImage_AcquireNativeWindowBuffer");
  NativeImage_ReleaseNativeWindowBuffer =
    sym(image_lib, "OH_NativeImage_ReleaseNativeWindowBuffer");
  GetBufferHandleFromNative =
    sym(window_lib, "OH_NativeWindow_GetBufferHandleFromNative");
  PFN_vkGetInstanceProcAddr gipa = sym(vulkan_lib, "vkGetInstanceProcAddr");

  /* Consumer: a consumer surface with GPU-renderable, mappable buffers. */
  struct OH_NativeImage* consumer = ConsumerSurface_Create();
  if (!consumer) {
    printf("FAIL: OH_ConsumerSurface_Create\n");
    return 1;
  }
  ConsumerSurface_SetDefaultUsage(
    consumer, BUFFER_USAGE_CPU_READ | BUFFER_USAGE_MEM_DMA |
                BUFFER_USAGE_HW_RENDER | BUFFER_USAGE_HW_TEXTURE
  );
  ConsumerSurface_SetDefaultSize(consumer, W, H);
  OHNativeWindow* window = NativeImage_AcquireNativeWindow(consumer);
  if (!window) {
    printf("FAIL: OH_NativeImage_AcquireNativeWindow\n");
    return 1;
  }

  /* Producer: Vulkan through the system loader and its swapchain layer. */
  VkInstance inst = VK_NULL_HANDLE;
  PFN_vkCreateInstance vkCreateInstance =
    (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance");
  const char* inst_exts[] = {
    VK_KHR_SURFACE_EXTENSION_NAME, VK_OHOS_SURFACE_EXTENSION_NAME
  };
  CHECK(vkCreateInstance(
    &(VkInstanceCreateInfo){
      .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
      .pApplicationInfo = &(
        VkApplicationInfo
      ){.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .apiVersion = VK_API_VERSION_1_3},
      .enabledExtensionCount = 2,
      .ppEnabledExtensionNames = inst_exts
    },
    NULL, &inst
  ));
  VK(EnumeratePhysicalDevices);
  VK(GetPhysicalDeviceProperties);
  VK(CreateSurfaceOHOS);
  VK(CreateDevice);
  VK(GetDeviceQueue);
  VK(GetPhysicalDeviceSurfaceCapabilitiesKHR);
  VK(CreateSwapchainKHR);
  VK(GetSwapchainImagesKHR);
  VK(CreateCommandPool);
  VK(AllocateCommandBuffers);
  VK(CreateSemaphore);
  VK(CreateFence);
  VK(AcquireNextImageKHR);
  VK(ResetCommandBuffer);
  VK(BeginCommandBuffer);
  VK(CmdPipelineBarrier);
  VK(CmdClearColorImage);
  VK(EndCommandBuffer);
  VK(QueueSubmit);
  VK(QueuePresentKHR);
  VK(WaitForFences);
  VK(ResetFences);

  uint32_t n = 1;
  VkPhysicalDevice pdev;
  CHECK(vkEnumeratePhysicalDevices(inst, &n, &pdev));
  VkPhysicalDeviceProperties props;
  vkGetPhysicalDeviceProperties(pdev, &props);
  printf("device: %s\n", props.deviceName);

  VkSurfaceKHR surface;
  CHECK(vkCreateSurfaceOHOS(
    inst,
    &(VkSurfaceCreateInfoOHOS){
      .sType = VK_STRUCTURE_TYPE_SURFACE_CREATE_INFO_OHOS, .window = window
    },
    NULL, &surface
  ));

  float prio = 1;
  const char* dev_exts[] = {VK_KHR_SWAPCHAIN_EXTENSION_NAME};
  VkDevice dev;
  CHECK(vkCreateDevice(
    pdev,
    &(VkDeviceCreateInfo){
      .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
      .queueCreateInfoCount = 1,
      .pQueueCreateInfos = &(
        VkDeviceQueueCreateInfo
      ){.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = 0,
        .queueCount = 1,
        .pQueuePriorities = &prio},
      .enabledExtensionCount = 1,
      .ppEnabledExtensionNames = dev_exts
    },
    NULL, &dev
  ));
  VkQueue queue;
  vkGetDeviceQueue(dev, 0, 0, &queue);

  VkSurfaceCapabilitiesKHR caps;
  CHECK(vkGetPhysicalDeviceSurfaceCapabilitiesKHR(pdev, surface, &caps));
  VkSwapchainKHR swapchain;
  CHECK(vkCreateSwapchainKHR(
    dev,
    &(VkSwapchainCreateInfoKHR){
      .sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
      .surface = surface,
      .minImageCount = caps.minImageCount,
      .imageFormat = VK_FORMAT_R8G8B8A8_UNORM,
      .imageColorSpace = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
      .imageExtent = {W, H},
      .imageArrayLayers = 1,
      .imageUsage =
        VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT,
      .imageSharingMode = VK_SHARING_MODE_EXCLUSIVE,
      .preTransform = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR,
      .compositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
      .presentMode = VK_PRESENT_MODE_FIFO_KHR,
      .clipped = VK_TRUE
    },
    NULL, &swapchain
  ));
  uint32_t nimg = 0;
  CHECK(vkGetSwapchainImagesKHR(dev, swapchain, &nimg, NULL));
  VkImage images[8];
  nimg = nimg > 8 ? 8 : nimg;
  CHECK(vkGetSwapchainImagesKHR(dev, swapchain, &nimg, images));
  printf("swapchain: %u images %dx%d\n", nimg, W, H);

  VkCommandPool pool;
  CHECK(vkCreateCommandPool(
    dev,
    &(VkCommandPoolCreateInfo){
      .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
      .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
      .queueFamilyIndex = 0
    },
    NULL, &pool
  ));
  VkCommandBuffer cmd;
  CHECK(vkAllocateCommandBuffers(
    dev,
    &(VkCommandBufferAllocateInfo){
      .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
      .commandPool = pool,
      .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
      .commandBufferCount = 1
    },
    &cmd
  ));
  VkSemaphore acquired, rendered;
  VkSemaphoreCreateInfo sci = {
    .sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO
  };
  CHECK(vkCreateSemaphore(dev, &sci, NULL, &acquired));
  CHECK(vkCreateSemaphore(dev, &sci, NULL, &rendered));
  VkFence done;
  CHECK(vkCreateFence(
    dev, &(VkFenceCreateInfo){.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO},
    NULL, &done
  ));

  int bad = 0, fences_real = 0;
  for (int f = 0; f < frames; f++) {
    uint32_t idx;
    CHECK(vkAcquireNextImageKHR(
      dev, swapchain, UINT64_MAX, acquired, VK_NULL_HANDLE, &idx
    ));
    uint8_t c[4];
    frame_color(f, c);
    CHECK(vkResetCommandBuffer(cmd, 0));
    CHECK(vkBeginCommandBuffer(
      cmd, &(VkCommandBufferBeginInfo){
             .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO
           }
    ));
    VkImageMemoryBarrier b = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
      .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
      .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
      .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
      .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .image = images[idx],
      .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1},
    };
    vkCmdPipelineBarrier(
      cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0,
      NULL, 0, NULL, 1, &b
    );
    VkClearColorValue cv = {
      .float32 = {c[0] / 255.f, c[1] / 255.f, c[2] / 255.f, 1}
    };
    vkCmdClearColorImage(
      cmd, images[idx], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &cv, 1,
      &b.subresourceRange
    );
    b.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
    b.dstAccessMask = 0;
    b.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
    b.newLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;
    vkCmdPipelineBarrier(
      cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT,
      0, 0, NULL, 0, NULL, 1, &b
    );
    CHECK(vkEndCommandBuffer(cmd));
    VkPipelineStageFlags wait_stage = VK_PIPELINE_STAGE_TRANSFER_BIT;
    CHECK(vkQueueSubmit(
      queue, 1,
      &(VkSubmitInfo){
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .waitSemaphoreCount = 1,
        .pWaitSemaphores = &acquired,
        .pWaitDstStageMask = &wait_stage,
        .commandBufferCount = 1,
        .pCommandBuffers = &cmd,
        .signalSemaphoreCount = 1,
        .pSignalSemaphores = &rendered
      },
      done
    ));
    CHECK(vkQueuePresentKHR(
      queue, &(VkPresentInfoKHR){
               .sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
               .waitSemaphoreCount = 1,
               .pWaitSemaphores = &rendered,
               .swapchainCount = 1,
               .pSwapchains = &swapchain,
               .pImageIndices = &idx
             }
    ));
    CHECK(vkWaitForFences(dev, 1, &done, VK_TRUE, UINT64_MAX));
    CHECK(vkResetFences(dev, 1, &done));

    /* Consumer: take the frame, wait on its fence, read it back. */
    struct NativeWindowBuffer* buffer = NULL;
    int fence = -1;
    if (
      NativeImage_AcquireNativeWindowBuffer(consumer, &buffer, &fence) != 0 ||
      !buffer
    ) {
      printf("FAIL: frame %d not delivered to the consumer\n", f);
      return 1;
    }
    if (fence >= 0) {
      fences_real++;
      struct pollfd p = {.fd = fence, .events = POLLIN};
      poll(&p, 1, 1000);
      close(fence);
    }
    struct BufferHandle* h = GetBufferHandleFromNative(buffer);
    uint8_t* px =
      h ? mmap(NULL, h->size, PROT_READ, MAP_SHARED, h->fd, 0) : MAP_FAILED;
    if (px == MAP_FAILED) {
      printf("FAIL: frame %d: cannot map the buffer\n", f);
      return 1;
    }
    /* Check a few pixels, including the last row, which needs the stride. */
    int ys[] = {0, H / 2, H - 1}, xs[] = {0, W / 3, W - 1};
    for (int i = 0; i < 3; i++) {
      const uint8_t* p = px + ys[i] * h->stride + xs[i] * 4;
      int d = abs(p[0] - c[0]) + abs(p[1] - c[1]) + abs(p[2] - c[2]);
      bad += d > 3;
    }
    if (f == 0 || f == frames - 1) {
      printf(
        "frame %d: image %u, stride %d bytes, pixel(0,0)=(%d,%d,%d) expected "
        "(%d,%d,%d), consumer fence %s\n",
        f, idx, h->stride, px[0], px[1], px[2], c[0], c[1], c[2],
        fence >= 0 ? "sync_file" : "none"
      );
    }
    munmap(px, h->size);
    NativeImage_ReleaseNativeWindowBuffer(consumer, buffer, -1);
  }
  printf(
    "%s: %d frames presented and read back, %d bad pixels, %d with a fence\n",
    bad ? "FAIL" : "PASS", frames, bad, fences_real
  );
  return bad != 0;
}
