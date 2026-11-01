/*
 * Imports YUV AHardwareBuffers through EGL and ANGLE the way SurfaceFlinger's
 * RenderEngine (Skia GL) imports video frames. The path goes through
 * eglGetNativeClientBufferANDROID,
 * eglCreateImageKHR(EGL_NATIVE_BUFFER_ANDROID) and GL_TEXTURE_EXTERNAL_OES,
 * samples in a shader and ends with glReadPixels.
 *
 * For YV12, flexible YCbCr_420_888 (NV12 in our gralloc) and NV21, the test
 * fills four quadrants with known colors (BT.601 limited range) through
 * AHardwareBuffer_lockPlanes, then checks the rendered RGB.
 *
 *   gles_yuv_test [width height]
 *
 * The default size is 896x480, NewPipe's decoder output.
 */
#define EGL_EGLEXT_PROTOTYPES
#define GL_GLEXT_PROTOTYPES
#include <EGL/egl.h>
#include <EGL/eglext.h>
// clang-format off
#include <GLES3/gl3.h>
#include <GLES2/gl2ext.h> /* after gl3.h, which defines the GL types */
// clang-format on
#include <android/hardware_buffer.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define FMT_YV12 0x32315659

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
static void rgb_to_yuv(
  const struct color* c, uint8_t* y, uint8_t* u, uint8_t* v
) {
  double r = c->r, g = c->g, b = c->b;
  *y = (uint8_t)(16 + (65.481 * r + 128.553 * g + 24.966 * b) / 255 + 0.5);
  *u = (uint8_t)(128 + (-37.797 * r - 74.203 * g + 112.0 * b) / 255 + 0.5);
  *v = (uint8_t)(128 + (112.0 * r - 93.786 * g - 18.214 * b) / 255 + 0.5);
}

static int quadrant(int x, int y, int w, int h) {
  return (y >= h / 2) * 2 + (x >= w / 2);
}

static GLuint compile(GLenum type, const char* src) {
  GLuint s = glCreateShader(type);
  glShaderSource(s, 1, &src, NULL);
  glCompileShader(s);
  GLint ok;
  glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
  if (!ok) {
    char log[1024];
    glGetShaderInfoLog(s, sizeof(log), NULL, log);
    printf("shader: %s\n", log);
  }
  return s;
}

static int test_format(
  uint32_t format, const char* fname, int w, int h, GLuint prog
) {
  AHardwareBuffer_Desc desc = {
    .width = w,
    .height = h,
    .layers = 1,
    .format = format,
    .usage = AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE |
             AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN |
             AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN,
  };
  AHardwareBuffer* ahb = NULL;
  int err = AHardwareBuffer_allocate(&desc, &ahb);
  if (err) {
    printf("FAIL %s: AHardwareBuffer_allocate = %d\n", fname, err);
    return 1;
  }

  AHardwareBuffer_Planes planes;
  if (
    AHardwareBuffer_lockPlanes(
      ahb, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, NULL, &planes
    )
  ) {
    printf("FAIL %s: lockPlanes\n", fname);
    return 1;
  }
  printf(
    "%s: %d planes; Y stride %u, U stride %u pixelStride %u, V stride %u "
    "pixelStride %u, "
    "V-U offset %td\n",
    fname, planes.planeCount, planes.planes[0].rowStride,
    planes.planes[1].rowStride, planes.planes[1].pixelStride,
    planes.planes[2].rowStride, planes.planes[2].pixelStride,
    (uint8_t*)planes.planes[2].data - (uint8_t*)planes.planes[1].data
  );
  for (int y = 0; y < h; y++) {
    for (int x = 0; x < w; x++) {
      uint8_t Y, U, V;
      rgb_to_yuv(&quad[quadrant(x, y, w, h)], &Y, &U, &V);
      ((uint8_t*)planes.planes[0].data)[y * planes.planes[0].rowStride + x] = Y;
      if (!(x & 1) && !(y & 1)) {
        int cx = x / 2, cy = y / 2;
        ((uint8_t*)planes.planes[1].data)
          [cy * planes.planes[1].rowStride +
           cx * planes.planes[1].pixelStride] = U;
        ((uint8_t*)planes.planes[2].data)
          [cy * planes.planes[2].rowStride +
           cx * planes.planes[2].pixelStride] = V;
      }
    }
  }
  AHardwareBuffer_unlock(ahb, NULL);

  /* What Skia's GrAHardwareBufferUtils does for GL. */
  EGLDisplay dpy = eglGetCurrentDisplay();
  EGLClientBuffer cb = eglGetNativeClientBufferANDROID(ahb);
  const EGLint attrs[] = {EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE};
  EGLImageKHR img = eglCreateImageKHR(
    dpy, EGL_NO_CONTEXT, EGL_NATIVE_BUFFER_ANDROID, cb, attrs
  );
  if (img == EGL_NO_IMAGE_KHR) {
    printf(
      "FAIL %s: eglCreateImageKHR(EGL_NATIVE_BUFFER_ANDROID) = 0x%x\n", fname,
      eglGetError()
    );
    AHardwareBuffer_release(ahb);
    return 1;
  }
  GLuint tex;
  glGenTextures(1, &tex);
  glBindTexture(GL_TEXTURE_EXTERNAL_OES, tex);
  glTexParameteri(GL_TEXTURE_EXTERNAL_OES, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
  glTexParameteri(GL_TEXTURE_EXTERNAL_OES, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
  glEGLImageTargetTexture2DOES(GL_TEXTURE_EXTERNAL_OES, img);
  GLenum gerr = glGetError();
  if (gerr) {
    printf("FAIL %s: glEGLImageTargetTexture2DOES = 0x%x\n", fname, gerr);
    return 1;
  }

  /* Render it into an RGBA8 FBO of the same size and read back. */
  GLuint rt, fbo;
  glGenTextures(1, &rt);
  glBindTexture(GL_TEXTURE_2D, rt);
  glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, w, h);
  glGenFramebuffers(1, &fbo);
  glBindFramebuffer(GL_FRAMEBUFFER, fbo);
  glFramebufferTexture2D(
    GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, rt, 0
  );
  glViewport(0, 0, w, h);
  glUseProgram(prog);
  glActiveTexture(GL_TEXTURE0);
  glBindTexture(GL_TEXTURE_EXTERNAL_OES, tex);
  glDrawArrays(GL_TRIANGLES, 0, 3);
  uint8_t* px = malloc((size_t)w * h * 4);
  glReadPixels(0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, px);
  gerr = glGetError();

  int bad = 0;
  for (int q = 0; q < 4; q++) {
    /* Quadrant centers; texture row 0 is the top of the buffer and the
     * fullscreen triangle maps it to framebuffer row 0. */
    int x = (q & 1) ? 3 * w / 4 : w / 4, y = (q & 2) ? 3 * h / 4 : h / 4;
    const uint8_t* p = &px[(y * w + x) * 4];
    int d =
      abs(p[0] - quad[q].r) + abs(p[1] - quad[q].g) + abs(p[2] - quad[q].b);
    bad += d > 24;
    printf(
      "  %-5s expected (%3d,%3d,%3d) got (%3d,%3d,%3d)%s\n", quad[q].name,
      quad[q].r, quad[q].g, quad[q].b, p[0], p[1], p[2],
      d > 24 ? "  <-- wrong" : ""
    );
  }
  printf(
    "%s %s: %dx%d sampled through EGLImage/samplerExternalOES (gl error "
    "0x%x)\n",
    bad || gerr ? "FAIL" : "PASS", fname, w, h, gerr
  );
  free(px);
  eglDestroyImageKHR(dpy, img);
  AHardwareBuffer_release(ahb);
  return bad || gerr;
}

int main(int argc, char** argv) {
  int w = argc > 2 ? atoi(argv[1]) : 896, h = argc > 2 ? atoi(argv[2]) : 480;
  setvbuf(stdout, NULL, _IOLBF, 0);
  EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
  if (!eglInitialize(dpy, NULL, NULL)) {
    printf("FAIL: eglInitialize\n");
    return 1;
  }
  const EGLint cfg_attr[] = {
    EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT,
    EGL_NONE
  };
  EGLConfig cfg;
  EGLint n;
  eglChooseConfig(dpy, cfg_attr, &cfg, 1, &n);
  const EGLint pb[] = {EGL_WIDTH, 16, EGL_HEIGHT, 16, EGL_NONE};
  EGLSurface surf = eglCreatePbufferSurface(dpy, cfg, pb);
  const EGLint ctx_attr[] = {EGL_CONTEXT_MAJOR_VERSION, 3, EGL_NONE};
  EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctx_attr);
  eglMakeCurrent(dpy, surf, surf, ctx);
  printf("GL_RENDERER=%s\n", glGetString(GL_RENDERER));

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
  GLuint prog = glCreateProgram();
  glAttachShader(prog, compile(GL_VERTEX_SHADER, vs));
  glAttachShader(prog, compile(GL_FRAGMENT_SHADER, fs));
  glLinkProgram(prog);

  int fails = 0;
  fails += test_format(FMT_YV12, "YV12", w, h, prog);
  fails += test_format(
    AHARDWAREBUFFER_FORMAT_Y8Cb8Cr8_420, "YCbCr_420_888", w, h, prog
  );
  fails += test_format(
    0x11 /* HAL_PIXEL_FORMAT_YCrCb_420_SP (NV21) */, "NV21", w, h, prog
  );
  printf("%s\n", fails ? "SOME TESTS FAILED" : "ALL PASSED");
  return fails != 0;
}
