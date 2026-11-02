/*
 * Checks Mesa's OpenHarmony EGL platform in an OpenHarmony profile.
 *
 *   ohos_gles_test [EGL library]
 *
 * It loads the EGL library (by default our libEGL_impl.so in
 * /vendor/lib64/chipsetsdk) and prints GL_RENDERER. A window surface on an
 * in-process consumer surface (OH_NativeImage) is cleared and swapped, and
 * the consumer reads the color back. A NativeWindowBuffer with a known
 * pattern is imported with EGL_NATIVE_BUFFER_OHOS, with a current context as
 * render_service passes, and read back through a framebuffer. It runs as root.
 */
#include <dlfcn.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define W 64
#define H 48

typedef int EGLint;
typedef unsigned int EGLBoolean, EGLenum, GLenum, GLuint, GLbitfield;
typedef int GLint, GLsizei;
typedef float GLfloat;
typedef unsigned char GLubyte;
typedef void *EGLDisplay, *EGLConfig, *EGLContext, *EGLSurface, *EGLImage;

#define EGL_NONE 0x3038
#define EGL_RED_SIZE 0x3024
#define EGL_GREEN_SIZE 0x3023
#define EGL_BLUE_SIZE 0x3022
#define EGL_ALPHA_SIZE 0x3021
#define EGL_SURFACE_TYPE 0x3033
#define EGL_WINDOW_BIT 0x0004
#define EGL_RENDERABLE_TYPE 0x3040
#define EGL_OPENGL_ES2_BIT 0x0004
#define EGL_CONTEXT_CLIENT_VERSION 0x3098
#define EGL_IMAGE_PRESERVED_KHR 0x30D2
#define EGL_TRUE 1
#define EGL_NATIVE_BUFFER_OHOS 0x34E1
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_RENDERER 0x1F01
#define GL_TEXTURE_2D 0x0DE1
#define GL_FRAMEBUFFER 0x8D40
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5
#define GL_RGBA 0x1908
#define GL_UNSIGNED_BYTE 0x1401

/* OpenHarmony's BufferHandle (display buffer_handle.h). */
struct BufferHandle {
  int32_t fd, width, stride, height, size, format;
  uint64_t usage;
  void* virAddr;
  uint64_t phyAddr;
  uint32_t reserveFds, reserveInts;
  int32_t reserve[];
};

/* OH_NativeBuffer_Config (native_buffer.h). */
struct NativeBufferConfig {
  int32_t width, height, format, usage, stride;
};

#define PIXEL_FMT_RGBA_8888 12
#define BUFFER_USAGE_CPU_READ (1 << 0)
#define BUFFER_USAGE_CPU_WRITE (1 << 1)
#define BUFFER_USAGE_MEM_DMA (1 << 3)
#define BUFFER_USAGE_HW_RENDER (1 << 8)
#define BUFFER_USAGE_HW_TEXTURE (1 << 9)

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

static void* (*egl_proc)(const char*);
static void* gl(const char* name) {
  void* p = egl_proc(name);
  if (!p) {
    printf("FAIL: eglGetProcAddress(%s)\n", name);
    exit(1);
  }
  return p;
}

static int near(const uint8_t* p, const uint8_t* want) {
  return abs(p[0] - want[0]) + abs(p[1] - want[1]) + abs(p[2] - want[2]) <= 3;
}

int main(int argc, char** argv) {
  const char* egl_path =
    argc > 1 ? argv[1] : "/vendor/lib64/chipsetsdk/libEGL_impl.so";
  setvbuf(stdout, NULL, _IOLBF, 0);

  void* image_lib = open_lib("/system/lib64/platformsdk/libnative_image.so");
  void* window_lib =
    open_lib("/system/lib64/chipset-sdk-sp/libnative_window.so");
  void* egl = open_lib(egl_path);

  struct OH_NativeImage* (*consumer_create)(void) =
    sym(image_lib, "OH_ConsumerSurface_Create");
  int32_t (*consumer_usage)(struct OH_NativeImage*, uint64_t) =
    sym(image_lib, "OH_ConsumerSurface_SetDefaultUsage");
  int32_t (*consumer_size)(struct OH_NativeImage*, int32_t, int32_t) =
    sym(image_lib, "OH_ConsumerSurface_SetDefaultSize");
  void* (*acquire_window)(struct OH_NativeImage*) =
    sym(image_lib, "OH_NativeImage_AcquireNativeWindow");
  int32_t (*acquire_buffer)(struct OH_NativeImage*, void**, int*) =
    sym(image_lib, "OH_NativeImage_AcquireNativeWindowBuffer");
  struct BufferHandle* (*buffer_handle)(void*) =
    sym(window_lib, "OH_NativeWindow_GetBufferHandleFromNative");
  void* (*buffer_alloc)(const struct NativeBufferConfig*) =
    sym(window_lib, "OH_NativeBuffer_Alloc");
  void* (*window_buffer_from_native)(void*) =
    sym(window_lib, "OH_NativeWindow_CreateNativeWindowBufferFromNativeBuffer");

  egl_proc = sym(egl, "eglGetProcAddress");
  EGLDisplay (*eglGetDisplay)(void*) = sym(egl, "eglGetDisplay");
  EGLBoolean (*eglInitialize)(EGLDisplay, EGLint*, EGLint*) =
    sym(egl, "eglInitialize");
  EGLBoolean (*eglChooseConfig)(
    EGLDisplay, const EGLint*, EGLConfig*, EGLint, EGLint*
  ) = sym(egl, "eglChooseConfig");
  EGLContext (*eglCreateContext)(
    EGLDisplay, EGLConfig, EGLContext, const EGLint*
  ) = sym(egl, "eglCreateContext");
  EGLSurface (*eglCreateWindowSurface)(
    EGLDisplay, EGLConfig, void*, const EGLint*
  ) = sym(egl, "eglCreateWindowSurface");
  EGLBoolean (*eglMakeCurrent)(EGLDisplay, EGLSurface, EGLSurface, EGLContext) =
    sym(egl, "eglMakeCurrent");
  EGLBoolean (*eglSwapBuffers)(EGLDisplay, EGLSurface) =
    sym(egl, "eglSwapBuffers");
  EGLint (*eglGetError)(void) = sym(egl, "eglGetError");
  EGLImage (*eglCreateImageKHR)(
    EGLDisplay, EGLContext, EGLenum, void*, const EGLint*
  ) = gl("eglCreateImageKHR");

  void (*glClearColor)(GLfloat, GLfloat, GLfloat, GLfloat) = gl("glClearColor");
  void (*glClear)(GLbitfield) = gl("glClear");
  const GLubyte* (*glGetString)(GLenum) = gl("glGetString");
  void (*glGenTextures)(GLsizei, GLuint*) = gl("glGenTextures");
  void (*glBindTexture)(GLenum, GLuint) = gl("glBindTexture");
  void (*glEGLImageTargetTexture2DOES)(GLenum, void*) =
    gl("glEGLImageTargetTexture2DOES");
  void (*glGenFramebuffers)(GLsizei, GLuint*) = gl("glGenFramebuffers");
  void (*glBindFramebuffer)(GLenum, GLuint) = gl("glBindFramebuffer");
  void (*glFramebufferTexture2D)(GLenum, GLenum, GLenum, GLuint, GLint) =
    gl("glFramebufferTexture2D");
  GLenum (*glCheckFramebufferStatus)(GLenum) = gl("glCheckFramebufferStatus");
  void (*glReadPixels)(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void*) =
    gl("glReadPixels");

  EGLDisplay dpy = eglGetDisplay(NULL);
  if (!dpy || !eglInitialize(dpy, NULL, NULL)) {
    printf("FAIL: eglInitialize: 0x%x\n", eglGetError());
    return 1;
  }
  const EGLint config_attrs[] = {
    EGL_RED_SIZE,
    8,
    EGL_GREEN_SIZE,
    8,
    EGL_BLUE_SIZE,
    8,
    EGL_ALPHA_SIZE,
    8,
    EGL_SURFACE_TYPE,
    EGL_WINDOW_BIT,
    EGL_RENDERABLE_TYPE,
    EGL_OPENGL_ES2_BIT,
    EGL_NONE
  };
  EGLConfig config;
  EGLint n = 0;
  if (!eglChooseConfig(dpy, config_attrs, &config, 1, &n) || n < 1) {
    printf("FAIL: no RGBA window config\n");
    return 1;
  }
  const EGLint ctx_attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE};
  EGLContext ctx = eglCreateContext(dpy, config, NULL, ctx_attrs);

  struct OH_NativeImage* consumer = consumer_create();
  consumer_usage(
    consumer, BUFFER_USAGE_CPU_READ | BUFFER_USAGE_MEM_DMA |
                BUFFER_USAGE_HW_RENDER | BUFFER_USAGE_HW_TEXTURE
  );
  consumer_size(consumer, W, H);
  EGLSurface surf =
    eglCreateWindowSurface(dpy, config, acquire_window(consumer), NULL);
  if (!ctx || !surf || !eglMakeCurrent(dpy, surf, surf, ctx)) {
    printf("FAIL: window surface: 0x%x\n", eglGetError());
    return 1;
  }
  printf("renderer: %s\n", (const char*)glGetString(GL_RENDERER));

  /* Window surface: clear, swap, read the frame from the consumer. */
  const uint8_t clear[4] = {200, 100, 50, 255};
  glClearColor(clear[0] / 255.f, clear[1] / 255.f, clear[2] / 255.f, 1);
  glClear(GL_COLOR_BUFFER_BIT);
  if (!eglSwapBuffers(dpy, surf)) {
    printf("FAIL: eglSwapBuffers: 0x%x\n", eglGetError());
    return 1;
  }
  /* Kopper presents on its own thread, so the frame arrives after the swap
   * returns.
   */
  void* frame = NULL;
  int fence = -1;
  for (int i = 0;
       i < 200 && (acquire_buffer(consumer, &frame, &fence) != 0 || !frame);
       i++)
    usleep(10000);
  if (!frame) {
    printf("FAIL: the swapped frame did not reach the consumer\n");
    return 1;
  }
  if (fence >= 0) {
    struct pollfd p = {.fd = fence, .events = POLLIN};
    poll(&p, 1, 1000);
    close(fence);
  }
  struct BufferHandle* h = buffer_handle(frame);
  uint8_t* px = mmap(NULL, h->size, PROT_READ, MAP_SHARED, h->fd, 0);
  if (px == MAP_FAILED) {
    printf("FAIL: cannot map the frame\n");
    return 1;
  }
  int bad = !near(px, clear) || !near(px + (H - 1) * h->stride, clear);
  printf(
    "window: pixel(0,0)=(%d,%d,%d) expected (%d,%d,%d)\n", px[0], px[1], px[2],
    clear[0], clear[1], clear[2]
  );
  munmap(px, h->size);

  /* EGL_NATIVE_BUFFER_OHOS: left half red, right half blue. */
  const struct NativeBufferConfig cfg = {
    W, H, PIXEL_FMT_RGBA_8888,
    BUFFER_USAGE_CPU_READ | BUFFER_USAGE_CPU_WRITE | BUFFER_USAGE_MEM_DMA |
      BUFFER_USAGE_HW_TEXTURE,
    0
  };
  void* nb = buffer_alloc(&cfg);
  void* nwb = nb ? window_buffer_from_native(nb) : NULL;
  struct BufferHandle* bh = nwb ? buffer_handle(nwb) : NULL;
  uint8_t* src =
    bh ? mmap(NULL, bh->size, PROT_READ | PROT_WRITE, MAP_SHARED, bh->fd, 0)
       : MAP_FAILED;
  if (src == MAP_FAILED) {
    printf("FAIL: cannot allocate and map a native buffer\n");
    return 1;
  }
  const uint8_t red[4] = {255, 0, 0, 255}, blue[4] = {0, 0, 255, 255};
  for (int y = 0; y < H; y++) {
    for (int x = 0; x < W; x++) {
      memcpy(src + y * bh->stride + x * 4, x < W / 2 ? red : blue, 4);
    }
  }
  munmap(src, bh->size);
  const EGLint image_attrs[] = {EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE};
  EGLImage image =
    eglCreateImageKHR(dpy, ctx, EGL_NATIVE_BUFFER_OHOS, nwb, image_attrs);
  if (!image) {
    printf(
      "FAIL: eglCreateImageKHR(EGL_NATIVE_BUFFER_OHOS): 0x%x\n", eglGetError()
    );
    return 1;
  }
  GLuint tex, fbo;
  glGenTextures(1, &tex);
  glBindTexture(GL_TEXTURE_2D, tex);
  glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, image);
  glGenFramebuffers(1, &fbo);
  glBindFramebuffer(GL_FRAMEBUFFER, fbo);
  glFramebufferTexture2D(
    GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0
  );
  if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
    printf("FAIL: the imported image is not a complete framebuffer\n");
    return 1;
  }
  uint8_t out[H][W][4];
  glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, out);
  int import_bad = !near(out[0][0], red) || !near(out[H - 1][W - 1], blue);
  printf(
    "import: left (%d,%d,%d) right (%d,%d,%d)\n", out[0][0][0], out[0][0][1],
    out[0][0][2], out[H - 1][W - 1][0], out[H - 1][W - 1][1],
    out[H - 1][W - 1][2]
  );
  bad += import_bad;
  printf("%s\n", bad ? "FAIL" : "PASS");
  return bad != 0;
}
