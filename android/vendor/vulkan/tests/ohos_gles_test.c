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
 * render_service passes, and read back through a framebuffer. YUV 4:2:0
 * buffers in the layout of video frames are imported the same way, sampled
 * through GL_TEXTURE_EXTERNAL_OES as render_service samples them, and their
 * quadrants are read back as RGB. It runs as root.
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
typedef char GLchar;
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
#define GL_TEXTURE_EXTERNAL_OES 0x8D65
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_NEAREST 0x2600
#define GL_RGBA8 0x8058
#define GL_TRIANGLES 0x0004
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_LINK_STATUS 0x8B82

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
#define PIXEL_FMT_YCBCR_420_SP 24
#define PIXEL_FMT_YCRCB_420_SP 25
#define PIXEL_FMT_YCBCR_420_P 28
#define PIXEL_FMT_YCRCB_420_P 29
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

struct color {
  const char* name;
  uint8_t r, g, b;
};
static const struct color quad[4] = {
  {"red", 200, 40, 40},
  {"green", 40, 200, 60},
  {"blue", 50, 60, 210},
  {"white", 230, 230, 230},
};

/* BT.601 limited range, rounded. */
static void rgb_to_yuv(const struct color* c, uint8_t yuv[3]) {
  double r = c->r, g = c->g, b = c->b;
  yuv[0] = (uint8_t)(16 + (65.481 * r + 128.553 * g + 24.966 * b) / 255 + 0.5);
  yuv[1] = (uint8_t)(128 + (-37.797 * r - 74.203 * g + 112.0 * b) / 255 + 0.5);
  yuv[2] = (uint8_t)(128 + (112.0 * r - 93.786 * g - 18.214 * b) / 255 + 0.5);
}

static int quadrant(int x, int y) { return (y >= H / 2) * 2 + (x >= W / 2); }

/* Writes the quadrants in the layout of a video frame from OpenHarmony's
 * decoder. The luma plane has the buffer's row pitch, and the chroma follows
 * it. NV12 and NV21 interleave the chroma at the luma's pitch. I420 and YV12
 * have a plane for each chroma channel at half the pitch.
 */
static void fill_yuv(uint8_t* p, int format, int stride) {
  const int planar =
    format == PIXEL_FMT_YCBCR_420_P || format == PIXEL_FMT_YCRCB_420_P;
  const int cr_first =
    format == PIXEL_FMT_YCRCB_420_SP || format == PIXEL_FMT_YCRCB_420_P;
  uint8_t* chroma = p + stride * H;
  for (int y = 0; y < H; y++) {
    for (int x = 0; x < W; x++) {
      uint8_t yuv[3];
      rgb_to_yuv(&quad[quadrant(x, y)], yuv);
      p[y * stride + x] = yuv[0];
      if ((x | y) & 1) continue;
      const uint8_t first = cr_first ? yuv[2] : yuv[1];
      const uint8_t second = cr_first ? yuv[1] : yuv[2];
      if (planar) {
        const int pitch = stride / 2, at = y / 2 * pitch + x / 2;
        chroma[at] = first;
        chroma[pitch * (H / 2) + at] = second;
      } else {
        chroma[y / 2 * stride + x] = first;
        chroma[y / 2 * stride + x + 1] = second;
      }
    }
  }
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
  EGLBoolean (*eglDestroyImageKHR)(EGLDisplay, EGLImage) =
    gl("eglDestroyImageKHR");
  GLuint (*glCreateShader)(GLenum) = gl("glCreateShader");
  void (*glShaderSource)(GLuint, GLsizei, const GLchar* const*, const GLint*) =
    gl("glShaderSource");
  void (*glCompileShader)(GLuint) = gl("glCompileShader");
  GLuint (*glCreateProgram)(void) = gl("glCreateProgram");
  void (*glAttachShader)(GLuint, GLuint) = gl("glAttachShader");
  void (*glLinkProgram)(GLuint) = gl("glLinkProgram");
  void (*glGetProgramiv)(GLuint, GLenum, GLint*) = gl("glGetProgramiv");
  void (*glUseProgram)(GLuint) = gl("glUseProgram");
  void (*glTexParameteri)(GLenum, GLenum, GLint) = gl("glTexParameteri");
  void (*glTexStorage2D)(GLenum, GLsizei, GLenum, GLsizei, GLsizei) =
    gl("glTexStorage2D");
  void (*glViewport)(GLint, GLint, GLsizei, GLsizei) = gl("glViewport");
  void (*glDrawArrays)(GLenum, GLint, GLsizei) = gl("glDrawArrays");
  GLenum (*glGetError)(void) = gl("glGetError");

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

  /* YUV 4:2:0: each format's quadrants, drawn through a samplerExternalOES
   * into an RGBA framebuffer.
   */
  const char* vs =
    "#version 300 es\n"
    "out vec2 uv;\n"
    "void main() {\n"
    "  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));\n"
    "  uv = p;\n"
    "  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);\n"
    "}\n";
  const char* fs =
    "#version 300 es\n"
    "#extension GL_OES_EGL_image_external_essl3 : require\n"
    "precision mediump float;\n"
    "uniform samplerExternalOES t;\n"
    "in vec2 uv;\n"
    "out vec4 c;\n"
    "void main() { c = texture(t, uv); }\n";
  GLuint prog = glCreateProgram(), vsh = glCreateShader(GL_VERTEX_SHADER),
         fsh = glCreateShader(GL_FRAGMENT_SHADER);
  glShaderSource(vsh, 1, &vs, NULL);
  glShaderSource(fsh, 1, &fs, NULL);
  glCompileShader(vsh);
  glCompileShader(fsh);
  glAttachShader(prog, vsh);
  glAttachShader(prog, fsh);
  glLinkProgram(prog);
  GLint linked = 0;
  glGetProgramiv(prog, GL_LINK_STATUS, &linked);
  if (!linked) {
    printf("FAIL: the samplerExternalOES program does not link\n");
    return 1;
  }
  GLuint rt;
  glGenTextures(1, &rt);
  glBindTexture(GL_TEXTURE_2D, rt);
  glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, W, H);
  glFramebufferTexture2D(
    GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, rt, 0
  );
  glViewport(0, 0, W, H);
  glUseProgram(prog);
  static const struct {
    int format;
    const char* name;
  } yuv_formats[] = {
    {PIXEL_FMT_YCBCR_420_SP, "NV12"},
    {PIXEL_FMT_YCRCB_420_SP, "NV21"},
    {PIXEL_FMT_YCBCR_420_P, "I420"},
    {PIXEL_FMT_YCRCB_420_P, "YV12"},
  };
  for (unsigned f = 0; f < sizeof(yuv_formats) / sizeof(yuv_formats[0]); f++) {
    const char* name = yuv_formats[f].name;
    const struct NativeBufferConfig ycfg = {
      W, H, yuv_formats[f].format,
      BUFFER_USAGE_CPU_READ | BUFFER_USAGE_CPU_WRITE | BUFFER_USAGE_MEM_DMA |
        BUFFER_USAGE_HW_TEXTURE,
      0
    };
    void* ynb = buffer_alloc(&ycfg);
    void* ynwb = ynb ? window_buffer_from_native(ynb) : NULL;
    struct BufferHandle* yh = ynwb ? buffer_handle(ynwb) : NULL;
    uint8_t* yp =
      yh ? mmap(NULL, yh->size, PROT_READ | PROT_WRITE, MAP_SHARED, yh->fd, 0)
         : MAP_FAILED;
    if (yp == MAP_FAILED) {
      printf("FAIL %s: cannot allocate and map a native buffer\n", name);
      bad++;
      continue;
    }
    fill_yuv(yp, yuv_formats[f].format, yh->stride);
    munmap(yp, yh->size);
    EGLImage yimage =
      eglCreateImageKHR(dpy, ctx, EGL_NATIVE_BUFFER_OHOS, ynwb, image_attrs);
    if (!yimage) {
      printf(
        "FAIL %s: eglCreateImageKHR(EGL_NATIVE_BUFFER_OHOS): 0x%x\n", name,
        eglGetError()
      );
      bad++;
      continue;
    }
    GLuint ytex;
    glGenTextures(1, &ytex);
    glBindTexture(GL_TEXTURE_EXTERNAL_OES, ytex);
    glTexParameteri(GL_TEXTURE_EXTERNAL_OES, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_EXTERNAL_OES, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glEGLImageTargetTexture2DOES(GL_TEXTURE_EXTERNAL_OES, yimage);
    glDrawArrays(GL_TRIANGLES, 0, 3);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, out);
    const GLenum gerr = glGetError();
    int wrong = gerr != 0;
    printf("%s: stride %d, gl error 0x%x\n", name, yh->stride, gerr);
    for (int q = 0; q < 4; q++) {
      /* Texture row 0 is the top of the buffer, and the triangle maps it to
       * framebuffer row 0.
       */
      const int x = (q & 1) ? 3 * W / 4 : W / 4,
                y = (q & 2) ? 3 * H / 4 : H / 4;
      const uint8_t* p = out[y][x];
      const int d =
        abs(p[0] - quad[q].r) + abs(p[1] - quad[q].g) + abs(p[2] - quad[q].b);
      wrong += d > 24;
      printf(
        "  %-5s expected (%3d,%3d,%3d) got (%3d,%3d,%3d)%s\n", quad[q].name,
        quad[q].r, quad[q].g, quad[q].b, p[0], p[1], p[2],
        d > 24 ? "  <-- wrong" : ""
      );
    }
    eglDestroyImageKHR(dpy, yimage);
    bad += wrong != 0;
  }
  printf("%s\n", bad ? "FAIL" : "PASS");
  return bad != 0;
}
