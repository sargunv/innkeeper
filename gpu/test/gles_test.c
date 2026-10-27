/*
 * GLES through the system libEGL (ro.hardware.egl=angle -> ANGLE -> Vulkan
 * HAL): render into a pbuffer and check glReadPixels.
 */
#include <EGL/egl.h>
#include <GLES3/gl3.h>
#include <stdio.h>
#include <string.h>

int main(void) {
  EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
  EGLint major, minor;
  if (!eglInitialize(dpy, &major, &minor)) {
    printf("FAIL: eglInitialize 0x%x\n", eglGetError());
    return 1;
  }
  printf(
    "EGL %d.%d vendor=\"%s\" version=\"%s\"\n", major, minor,
    eglQueryString(dpy, EGL_VENDOR), eglQueryString(dpy, EGL_VERSION)
  );

  const EGLint cfg_attr[] = {
    EGL_SURFACE_TYPE,
    EGL_PBUFFER_BIT,
    EGL_RENDERABLE_TYPE,
    EGL_OPENGL_ES3_BIT,
    EGL_RED_SIZE,
    8,
    EGL_GREEN_SIZE,
    8,
    EGL_BLUE_SIZE,
    8,
    EGL_ALPHA_SIZE,
    8,
    EGL_NONE
  };
  EGLConfig cfg;
  EGLint n = 0;
  if (!eglChooseConfig(dpy, cfg_attr, &cfg, 1, &n) || n < 1) {
    printf("FAIL: eglChooseConfig\n");
    return 1;
  }
  const EGLint pb_attr[] = {EGL_WIDTH, 64, EGL_HEIGHT, 64, EGL_NONE};
  EGLSurface surf = eglCreatePbufferSurface(dpy, cfg, pb_attr);
  // ES 3.0: ANGLE on lavapipe offers no 3.2 context (EGL_BAD_MATCH).
  const EGLint ctx_attr[] = {EGL_CONTEXT_MAJOR_VERSION, 3, EGL_NONE};
  EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctx_attr);
  if (
    surf == EGL_NO_SURFACE || ctx == EGL_NO_CONTEXT ||
    !eglMakeCurrent(dpy, surf, surf, ctx)
  ) {
    printf("FAIL: surface/context 0x%x\n", eglGetError());
    return 1;
  }
  printf(
    "GL_VENDOR=%s\nGL_RENDERER=%s\nGL_VERSION=%s\n", glGetString(GL_VENDOR),
    glGetString(GL_RENDERER), glGetString(GL_VERSION)
  );

  glClearColor(0.25f, 0.5f, 0.75f, 1.0f);
  glClear(GL_COLOR_BUFFER_BIT);
  glEnable(GL_SCISSOR_TEST);
  glScissor(16, 16, 8, 8);
  glClearColor(1.0f, 0.0f, 0.0f, 1.0f);
  glClear(GL_COLOR_BUFFER_BIT);
  glDisable(GL_SCISSOR_TEST);
  unsigned char px[64 * 64 * 4];
  glReadPixels(0, 0, 64, 64, GL_RGBA, GL_UNSIGNED_BYTE, px);
  const unsigned char *bg = &px[(2 * 64 + 2) * 4],
                      *fg = &px[(20 * 64 + 20) * 4];
  int ok = glGetError() == GL_NO_ERROR && bg[0] >= 63 && bg[0] <= 65 &&
           bg[1] >= 127 && bg[1] <= 128 && bg[2] >= 191 && bg[2] <= 192 &&
           fg[0] == 255 && fg[1] == 0 && fg[2] == 0;
  printf(
    "%s: readback bg=(%d,%d,%d,%d) scissored=(%d,%d,%d,%d)\n",
    ok ? "PASS" : "FAIL", bg[0], bg[1], bg[2], bg[3], fg[0], fg[1], fg[2], fg[3]
  );
  return !ok;
}
