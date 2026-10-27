/*
 * Test-only stand-in for SurfaceFlinger's "SurfaceFlingerAIDL" service, so
 * libvulkan's swapchain can run on an AImageReader window without a session.
 * libgui's Surface asks it only for getDisplayStats() (the refresh period);
 * this answers that with 60 Hz and rejects every other transaction.
 */
#include <android/binder_ibinder.h>
#include <android/binder_parcel.h>
#include <android/binder_status.h>
#include <dlfcn.h>
#include <stdio.h>
#include <time.h>

/* android.gui.ISurfaceComposer: FIRST_CALL_TRANSACTION + index of
 * getDisplayStats */
#define TRANSACTION_getDisplayStats (FIRST_CALL_TRANSACTION + 9)

static void* on_create(void* args) { return args; }
static void on_destroy(void* data) { (void)data; }

static binder_status_t on_transact(
  AIBinder* binder, transaction_code_t code, const AParcel* in, AParcel* out
) {
  (void)binder;
  (void)in;
  if (code != TRANSACTION_getDisplayStats) return STATUS_UNKNOWN_TRANSACTION;
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  AStatus* ok = AStatus_newOk();
  AParcel_writeStatusHeader(out, ok);
  AStatus_delete(ok);
  /* Non-null parcelable DisplayStatInfo { long vsyncTime; long vsyncPeriod; }
   */
  AParcel_writeInt32(out, 1);
  AParcel_writeInt32(out, 4 + 8 + 8);
  AParcel_writeInt64(out, (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec);
  AParcel_writeInt64(out, 16666667);
  return STATUS_OK;
}

int main(void) {
  void* lib = dlopen("libbinder_ndk.so", RTLD_NOW);
  binder_status_t (*add_service)(AIBinder*, const char*) =
    (binder_status_t (*)(AIBinder*, const char*))dlsym(
      lib, "AServiceManager_addService"
    );
  void (*join)(void) =
    (void (*)(void))dlsym(lib, "ABinderProcess_joinThreadPool");
  if (!add_service || !join) {
    printf("fake_sf: libbinder_ndk symbols missing\n");
    return 1;
  }
  AIBinder_Class* clazz = AIBinder_Class_define(
    "android.gui.ISurfaceComposer", on_create, on_destroy, on_transact
  );
  AIBinder* binder = AIBinder_new(clazz, NULL);
  if (add_service(binder, "SurfaceFlingerAIDL") != STATUS_OK) {
    printf("fake_sf: addService failed\n");
    return 1;
  }
  printf("fake_sf: SurfaceFlingerAIDL registered\n");
  fflush(stdout);
  join();
  return 0;
}
