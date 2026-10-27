/*
 * Acquire/present loop through Android's real WSI: libvulkan's swapchain on an
 * ANativeWindow from AImageReader (an in-process BufferQueue consumer), with
 * buffers from the system gralloc (our allocator + mapper).
 *
 *   swapchain_test [--deferred] [--frames N]
 *
 * Each frame clears the acquired image to a frame-specific color and presents;
 * the consumer side acquires the image, waits on its fence, locks the
 * AHardwareBuffer and checks the color. --deferred asks for
 * VK_SWAPCHAIN_CREATE_DEFERRED_MEMORY_ALLOCATION_BIT_EXT, as ANGLE does.
 * Needs a session (servicemanager + IAllocator) and ro.hardware.vulkan.
 */
#define VK_USE_PLATFORM_ANDROID_KHR 1
#include <android/hardware_buffer.h>
#include <android/sync.h>
#include <media/NdkImageReader.h>
#include <poll.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
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

static double now_ms(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

static void frame_color(int f, uint8_t rgba[4]) {
  rgba[0] = (uint8_t)(37 * f + 11);
  rgba[1] = (uint8_t)(91 * f + 5);
  rgba[2] = (uint8_t)(173 * f + 29);
  rgba[3] = 255;
}

int main(int argc, char** argv) {
  bool deferred = false;
  int frames = 120;
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "--deferred"))
      deferred = true;
    else if (!strcmp(argv[i], "--frames") && i + 1 < argc)
      frames = atoi(argv[++i]);
  }
  setvbuf(stdout, NULL, _IOLBF, 0);

  /* Consumer: AImageReader with GPU render-target buffers. */
  AImageReader* reader;
  media_status_t ms = AImageReader_newWithUsage(
    W, H, AIMAGE_FORMAT_RGBA_8888,
    AHARDWAREBUFFER_USAGE_GPU_FRAMEBUFFER |
      AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN,
    4, &reader
  );
  ANativeWindow* window = NULL;
  if (ms != AMEDIA_OK || AImageReader_getWindow(reader, &window) != AMEDIA_OK) {
    printf("FAIL: AImageReader %d\n", ms);
    return 1;
  }

  /* Producer: Vulkan through the system loader. */
  const char* inst_exts[] = {
    VK_KHR_SURFACE_EXTENSION_NAME, VK_KHR_ANDROID_SURFACE_EXTENSION_NAME,
    "VK_KHR_get_surface_capabilities2", "VK_EXT_surface_maintenance1"
  };
  VkApplicationInfo app = {
    .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
    .apiVersion = VK_API_VERSION_1_3
  };
  VkInstance inst;
  CHECK(vkCreateInstance(
    &(VkInstanceCreateInfo){
      .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
      .pApplicationInfo = &app,
      .enabledExtensionCount = deferred ? 4 : 2,
      .ppEnabledExtensionNames = inst_exts
    },
    NULL, &inst
  ));
  uint32_t n = 1;
  VkPhysicalDevice pdev;
  vkEnumeratePhysicalDevices(inst, &n, &pdev);
  VkPhysicalDeviceProperties props;
  vkGetPhysicalDeviceProperties(pdev, &props);
  printf("device: %s\n", props.deviceName);

  VkSurfaceKHR surface;
  CHECK(vkCreateAndroidSurfaceKHR(
    inst,
    &(VkAndroidSurfaceCreateInfoKHR){
      .sType = VK_STRUCTURE_TYPE_ANDROID_SURFACE_CREATE_INFO_KHR,
      .window = window
    },
    NULL, &surface
  ));

  float prio = 1;
  const char* dev_exts[] = {
    VK_KHR_SWAPCHAIN_EXTENSION_NAME, "VK_EXT_swapchain_maintenance1"
  };
  VkPhysicalDeviceSwapchainMaintenance1FeaturesEXT sm1 = {
    .sType =
      VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SWAPCHAIN_MAINTENANCE_1_FEATURES_EXT,
    .swapchainMaintenance1 = VK_TRUE
  };
  VkDevice dev;
  CHECK(vkCreateDevice(
    pdev,
    &(VkDeviceCreateInfo){
      .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
      .pNext = deferred ? &sm1 : NULL,
      .queueCreateInfoCount = 1,
      .pQueueCreateInfos = &(
        VkDeviceQueueCreateInfo
      ){.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = 0,
        .queueCount = 1,
        .pQueuePriorities = &prio},
      .enabledExtensionCount = deferred ? 2 : 1,
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
      .flags =
        deferred ? VK_SWAPCHAIN_CREATE_DEFERRED_MEMORY_ALLOCATION_BIT_EXT : 0,
      .surface = surface,
      .minImageCount = caps.minImageCount < 3 ? 3 : caps.minImageCount,
      .imageFormat = VK_FORMAT_R8G8B8A8_UNORM,
      .imageColorSpace = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
      .imageExtent = {W, H},
      .imageArrayLayers = 1,
      .imageUsage =
        VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT,
      .imageSharingMode = VK_SHARING_MODE_EXCLUSIVE,
      .preTransform = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR,
      .compositeAlpha = VK_COMPOSITE_ALPHA_INHERIT_BIT_KHR,
      .presentMode = VK_PRESENT_MODE_FIFO_KHR,
      .clipped = VK_TRUE
    },
    NULL, &swapchain
  ));
  uint32_t nimg = 0;
  vkGetSwapchainImagesKHR(dev, swapchain, &nimg, NULL);
  VkImage images[8];
  nimg = nimg > 8 ? 8 : nimg;
  vkGetSwapchainImagesKHR(dev, swapchain, &nimg, images);
  printf(
    "swapchain: %u images %dx%d%s\n", nimg, W, H,
    deferred ? " (deferred allocation)" : ""
  );

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

  int checked = 0, bad = 0, fences_real = 0;
  double t0 = now_ms();
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

    /* Consumer: take the frame, wait on its acquire fence, read it back. */
    AImage* image = NULL;
    int fence = -1;
    if (
      AImageReader_acquireNextImageAsync(reader, &image, &fence) != AMEDIA_OK ||
      !image
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
    AHardwareBuffer* ahb;
    AImage_getHardwareBuffer(image, &ahb);
    AHardwareBuffer_Desc desc;
    AHardwareBuffer_describe(ahb, &desc);
    uint8_t* px;
    if (
      AHardwareBuffer_lock(
        ahb, AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, -1, NULL, (void**)&px
      )
    ) {
      printf("FAIL: lock\n");
      return 1;
    }
    /* Check a few pixels, including the last row (exercises the stride). */
    int ys[] = {0, H / 2, H - 1}, xs[] = {0, W / 3, W - 1};
    for (int i = 0; i < 3; i++) {
      const uint8_t* p = px + (ys[i] * desc.stride + xs[i]) * 4;
      int d = abs(p[0] - c[0]) + abs(p[1] - c[1]) + abs(p[2] - c[2]);
      bad += d > 3;
    }
    if (f == 0 || f == frames - 1)
      printf(
        "frame %d: image %u, buffer stride %u px, pixel(0,0)=(%d,%d,%d) "
        "expected (%d,%d,%d), "
        "consumer fence %s\n",
        f, idx, desc.stride, px[0], px[1], px[2], c[0], c[1], c[2],
        fence >= 0 ? "sync_file" : "none (-1)"
      );
    AHardwareBuffer_unlock(ahb, NULL);
    AImage_delete(image);
    checked++;
  }
  double dt = now_ms() - t0;
  printf(
    "%s: %d frames acquired/presented/read back in %.0f ms (%.1f fps), %d bad "
    "pixels, "
    "%d/%d frames with a real release fence\n",
    bad ? "FAIL" : "PASS", checked, dt, checked * 1000.0 / dt, bad, fences_real,
    checked
  );
  return bad != 0;
}
