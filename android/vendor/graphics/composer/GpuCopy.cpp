// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#define EGL_EGLEXT_PROTOTYPES
#define GL_GLEXT_PROTOTYPES
#include <mutex>

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <GLES3/gl3.h>
#include <android/hardware_buffer.h>
#include <dlfcn.h>

#include "GpuCopy.h"

#include "ImportCache.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {
namespace {
struct Context {
  std::mutex mutex;
  EGLDisplay display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
  EGLContext context = EGL_NO_CONTEXT;
  EGLSurface surface = EGL_NO_SURFACE;
  Context() {
    if (!eglInitialize(display, nullptr, nullptr)) return;
    const EGLint attributes[] = {
      EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,
      EGL_OPENGL_ES3_BIT, EGL_NONE
    };
    EGLConfig config;
    EGLint count;
    if (!eglChooseConfig(display, attributes, &config, 1, &count) || count != 1)
      return;
    const EGLint version[] = {EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE};
    context = eglCreateContext(display, config, EGL_NO_CONTEXT, version);
    const EGLint size[] = {EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE};
    surface = eglCreatePbufferSurface(display, config, size);
  }
};

struct Image {
  Context& context;
  AHardwareBuffer* buffer = nullptr;
  EGLImageKHR image = EGL_NO_IMAGE_KHR;
  GLuint texture = 0, framebuffer = 0;
  bool ready = false;
  Image(Context& context, const BufferRef& ref, GLenum attachment)
      : context(context) {
    // LL-NDK entry point, omitted from the public NDK's libnativewindow stub.
    using Create = int (*)(
      const AHardwareBuffer_Desc*, const native_handle_t*, int32_t,
      AHardwareBuffer**
    );
    static auto create = reinterpret_cast<Create>(
      dlsym(RTLD_DEFAULT, "AHardwareBuffer_createFromHandle")
    );
    const auto* h = ref.info();
    if (!h || !create) return;
    // EGL needs the concrete layout for an IMPLEMENTATION_DEFINED sink.
    // This local import view changes neither the producer handle nor pixels.
    BufferHandle view = *h;
    view.format = view.resolvedFormat;
    const AHardwareBuffer_Desc desc{
      .width = h->width,
      .height = h->height,
      .layers = h->layerCount,
      .format = uint32_t(view.format),
      .usage = h->usage.get(),
      .stride = h->stridePixels
    };
    int status = create(&desc, &view.base, 3 /* CLONE */, &buffer);
    if (status != 0) {
      LOGW("GPU import create failed: %d", status);
      return;
    }
    const EGLint attrs[] = {EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE};
    image = eglCreateImageKHR(
      context.display, EGL_NO_CONTEXT, EGL_NATIVE_BUFFER_ANDROID,
      eglGetNativeClientBufferANDROID(buffer), attrs
    );
    if (image == EGL_NO_IMAGE_KHR) {
      LOGW(
        "GPU EGLImage failed: 0x%x attachment=0x%x fmt=%d usage=%llx",
        eglGetError(), attachment, h->format, (unsigned long long)h->usage.get()
      );
      return;
    }
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, image);
    glGenFramebuffers(1, &framebuffer);
    glBindFramebuffer(attachment, framebuffer);
    glFramebufferTexture2D(
      attachment, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0
    );
    GLenum state = glCheckFramebufferStatus(attachment);
    ready = state == GL_FRAMEBUFFER_COMPLETE;
    if (!ready)
      LOGW(
        "GPU framebuffer target=0x%x status=0x%x gl=0x%x", attachment, state,
        glGetError()
      );
  }
  ~Image() {
    glDeleteFramebuffers(1, &framebuffer);
    glDeleteTextures(1, &texture);
    if (image != EGL_NO_IMAGE_KHR) {
      eglDestroyImageKHR(context.display, image);
    }
    if (buffer) AHardwareBuffer_release(buffer);
  }
};
}  // namespace

struct GpuCopy::State {
  Context& context;
  ImportCache<Image> images;
  State() : context(sharedContext()) {}
  static Context& sharedContext() {
    static Context context;
    return context;
  }
  bool bind() {
    return context.context != EGL_NO_CONTEXT &&
           context.surface != EGL_NO_SURFACE &&
           eglMakeCurrent(
             context.display, context.surface, context.surface, context.context
           );
  }
  void unbind() {
    eglMakeCurrent(
      context.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT
    );
  }
  ~State() {
    std::lock_guard lock(context.mutex);
    if (bind()) {
      images.clear();
      unbind();
    }
  }
  Image* get(const BufferRef& ref, GLenum attachment) {
    return images.get(ref.info()->bufferId.get(), [&] {
      auto image = std::make_unique<Image>(context, ref, attachment);
      return image->ready ? std::move(image) : nullptr;
    });
  }
};

GpuCopy::GpuCopy() = default;
GpuCopy::~GpuCopy() = default;

bool GpuCopy::copy(const BufferRef& source, const BufferRef& target) {
  const auto* a = source.info();
  const auto* b = target.info();
  if (!a || !b || a->width != b->width || a->height != b->height) return false;
  if (!mState) mState = std::make_unique<State>();
  auto& state = *mState;
  std::lock_guard lock(state.context.mutex);
  // Teardown runs outside ComposerClient's lock. Serialize it with copies
  // sharing this context. Unbind after each copy because a
  // later binder call can arrive on a different thread. Imports live only
  // for this display, and are freed when it is destroyed.
  if (!state.bind()) return false;
  state.images.prepare(a->bufferId.get(), b->bufferId.get());
  Image* src = state.get(source, GL_READ_FRAMEBUFFER);
  Image* dst = state.get(target, GL_DRAW_FRAMEBUFFER);
  bool ok = src && dst;
  if (ok) {
    glBindFramebuffer(GL_READ_FRAMEBUFFER, src->framebuffer);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, dst->framebuffer);
    glBlitFramebuffer(
      0, 0, a->width, a->height, 0, 0, b->width, b->height, GL_COLOR_BUFFER_BIT,
      GL_NEAREST
    );
    // Complete before releasing the scratch target or handing the sink to
    // Wayland. Pixel memory is never mapped for CPU access.
    glFinish();
    ok = glGetError() == GL_NO_ERROR;
  }
  state.unbind();
  if (!ok) LOGW("GPU copy to virtual-display sink failed");
  return ok;
}
}  // namespace linuxapk::hwc
