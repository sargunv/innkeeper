// SPDX-License-Identifier: AGPL-3.0-or-later
// The system app's native module, `libinnkeeper.so`. It gives ArkTS the bridge
// protocol (docs/bridge-protocol.md), which needs SOCK_SEQPACKET and fd
// passing that ArkTS's own sockets lack, and a wait on a system parameter.
// types/libinnkeeper/index.d.ts declares what it exports. Every N-API call
// runs on the ArkTS thread, and the reader threads hand their packets over
// through a threadsafe function.

#include <errno.h>
#include <fcntl.h>
#include <node_api.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include "connection.h"

// From libbegetutil (base/startup/init interfaces/innerkits/include/syspara/
// parameter.h). A timeout of 0 waits for init's default, and a timeout
// returns SYSPARAM_WAIT_TIMEOUT.
extern int WaitParameter(const char* key, const char* value, int timeout);
#define SYSPARAM_WAIT_TIMEOUT (-14700105)

#define MAX_CONNECTIONS 8

struct conn {
  struct ik_connection c;
  uint32_t service;
  char name[LXB_MAX_NAME + 1];
  napi_threadsafe_function tsfn;
  napi_ref callback;  // the ArkTS thread's onEvent callback, or NULL
};

static struct conn* conns[MAX_CONNECTIONS];
static int nconns;

enum kind { CONNECTED, EVENT, REPLY, FAILED };

struct item {
  enum kind kind;
  struct lxb_packet p;     // EVENT and REPLY
  napi_deferred deferred;  // REPLY and FAILED
  int err;                 // FAILED
};

static void throw_errno(napi_env env, const char* what, int err) {
  char msg[96];
  snprintf(msg, sizeof(msg), "%s: %s", what, strerror(-err));
  napi_throw_error(env, NULL, msg);
}

static napi_value make_error(napi_env env, const char* what, int err) {
  char text[96];
  snprintf(text, sizeof(text), "%s: %s", what, strerror(-err));
  napi_value msg, error;
  napi_create_string_utf8(env, text, NAPI_AUTO_LENGTH, &msg);
  napi_create_error(env, NULL, msg, &error);
  return error;
}

static napi_value number(napi_env env, int32_t v) {
  napi_value n;
  napi_create_int32(env, v, &n);
  return n;
}

// The packet's body as an ArrayBuffer and its fds as numbers, which ArkTS
// then owns.
static void packet_values(
  napi_env env, struct lxb_packet* p, napi_value* body, napi_value* fds
) {
  void* data;
  napi_create_arraybuffer(env, p->len, &data, body);
  if (p->len > 0) memcpy(data, p->body, p->len);
  napi_create_array_with_length(env, (size_t)p->nfds, fds);
  for (int i = 0; i < p->nfds; i++)
    napi_set_element(env, *fds, (uint32_t)i, number(env, p->fds[i]));
  p->nfds = 0;
}

static void call_js(napi_env env, napi_value js_cb, void* context, void* data) {
  (void)js_cb;
  struct conn* conn = context;
  struct item* it = data;
  if (env == NULL) {  // the function is being torn down
    lxb_close_fds(&it->p);
    free(it);
    return;
  }
  napi_value body, fds;
  switch (it->kind) {
    case CONNECTED:
      it->p = (struct lxb_packet){.type = 0};
      // fall through
    case EVENT: {
      packet_values(env, &it->p, &body, &fds);
      napi_value cb, undefined, result;
      if (
        conn->callback &&
        napi_get_reference_value(env, conn->callback, &cb) == napi_ok
      ) {
        napi_value argv[] = {number(env, it->p.type), body, fds};
        napi_get_undefined(env, &undefined);
        napi_call_function(env, undefined, cb, 3, argv, &result);
      } else {
        // Nobody listens yet, so nobody takes the fds.
        uint32_t n = 0;
        napi_get_array_length(env, fds, &n);
        for (uint32_t i = 0; i < n; i++) {
          napi_value v;
          int32_t fd;
          napi_get_element(env, fds, i, &v);
          if (napi_get_value_int32(env, v, &fd) == napi_ok) close(fd);
        }
      }
      break;
    }
    case REPLY: {
      napi_value reply;
      packet_values(env, &it->p, &body, &fds);
      napi_create_object(env, &reply);
      napi_set_named_property(env, reply, "status", number(env, it->p.status));
      napi_set_named_property(env, reply, "body", body);
      napi_set_named_property(env, reply, "fds", fds);
      napi_resolve_deferred(env, it->deferred, reply);
      break;
    }
    case FAILED:
      napi_reject_deferred(
        env, it->deferred, make_error(env, "bridge request", it->err)
      );
      break;
  }
  free(it);
}

static void post(struct conn* conn, struct item* it) {
  if (
    napi_call_threadsafe_function(conn->tsfn, it, napi_tsfn_blocking) != napi_ok
  ) {
    lxb_close_fds(&it->p);
    free(it);
  }
}

static struct item* new_item(enum kind kind) {
  struct item* it = calloc(1, sizeof(*it));
  if (it) it->kind = kind;
  return it;
}

static int on_connect(void* ctx) {
  struct conn* conn = ctx;
  return lxb_connect(conn->service, conn->name);
}

static void on_connected(void* ctx) {
  struct item* it = new_item(CONNECTED);
  if (it) post(ctx, it);
}

static void on_event(void* ctx, struct lxb_packet* p) {
  struct item* it = new_item(EVENT);
  if (!it) {
    lxb_close_fds(p);
    return;
  }
  it->p = *p;
  post(ctx, it);
}

static void on_reply(void* ctx, void* cookie, struct lxb_packet* p) {
  struct item* it = new_item(REPLY);
  if (!it) {
    // The promise stays pending. calloc of a few hundred bytes fails only
    // when the process is out of memory.
    lxb_close_fds(p);
    return;
  }
  it->p = *p;
  it->deferred = cookie;
  post(ctx, it);
}

static void on_failed(void* ctx, void* cookie, int err) {
  struct item* it = new_item(FAILED);
  if (!it) return;
  it->deferred = cookie;
  it->err = err;
  post(ctx, it);
}

static bool get_args(
  napi_env env, napi_callback_info info, size_t want, napi_value* argv
) {
  size_t argc = want;
  if (
    napi_get_cb_info(env, info, &argc, argv, NULL, NULL) != napi_ok ||
    argc < want
  ) {
    napi_throw_type_error(env, NULL, "missing arguments");
    return false;
  }
  return true;
}

static struct conn* get_conn(napi_env env, napi_value v) {
  int32_t h;
  if (napi_get_value_int32(env, v, &h) != napi_ok || h < 0 || h >= nconns) {
    napi_throw_range_error(env, NULL, "no such connection");
    return NULL;
  }
  return conns[h];
}

// open(service: number, name: string): number
static napi_value js_open(napi_env env, napi_callback_info info) {
  napi_value argv[2];
  if (!get_args(env, info, 2, argv)) return NULL;
  uint32_t service;
  size_t len;
  struct conn* conn = calloc(1, sizeof(*conn));
  if (!conn) {
    napi_throw_error(env, NULL, "out of memory");
    return NULL;
  }
  if (
    napi_get_value_uint32(env, argv[0], &service) != napi_ok ||
    napi_get_value_string_utf8(
      env, argv[1], conn->name, sizeof(conn->name), &len
    ) != napi_ok
  ) {
    free(conn);
    napi_throw_type_error(env, NULL, "open(service: number, name: string)");
    return NULL;
  }
  if (nconns == MAX_CONNECTIONS) {
    free(conn);
    napi_throw_range_error(env, NULL, "too many connections");
    return NULL;
  }
  conn->service = service;
  napi_value resource;
  napi_create_string_utf8(env, conn->name, NAPI_AUTO_LENGTH, &resource);
  if (
    napi_create_threadsafe_function(
      env, NULL, NULL, resource, 0, 1, NULL, NULL, conn, call_js, &conn->tsfn
    ) != napi_ok
  ) {
    free(conn);
    napi_throw_error(env, NULL, "napi_create_threadsafe_function failed");
    return NULL;
  }
  const struct ik_callbacks cb = {
    .connect = on_connect,
    .connected = on_connected,
    .event = on_event,
    .reply = on_reply,
    .failed = on_failed,
    .ctx = conn,
  };
  const int r = ik_connection_start(&conn->c, &cb);
  if (r != 0) {
    napi_release_threadsafe_function(conn->tsfn, napi_tsfn_abort);
    free(conn);
    throw_errno(env, "open", r);
    return NULL;
  }
  conns[nconns] = conn;
  return number(env, nconns++);
}

// onEvent(handle: number, callback: (type, body, fds) => void): void
static napi_value js_on_event(napi_env env, napi_callback_info info) {
  napi_value argv[2];
  if (!get_args(env, info, 2, argv)) return NULL;
  struct conn* conn = get_conn(env, argv[0]);
  if (!conn) return NULL;
  if (conn->callback) napi_delete_reference(env, conn->callback);
  conn->callback = NULL;
  napi_create_reference(env, argv[1], 1, &conn->callback);
  return NULL;
}

// request(handle: number, type: number, body: ArrayBuffer, fds: number[]):
// Promise<Reply>
static napi_value js_request(napi_env env, napi_callback_info info) {
  napi_value argv[4];
  if (!get_args(env, info, 4, argv)) return NULL;
  struct conn* conn = get_conn(env, argv[0]);
  if (!conn) return NULL;
  uint32_t type, nfds;
  void* data;
  size_t len;
  int32_t fd = -1;
  if (
    napi_get_value_uint32(env, argv[1], &type) != napi_ok || type > 0xffff ||
    napi_get_arraybuffer_info(env, argv[2], &data, &len) != napi_ok ||
    napi_get_array_length(env, argv[3], &nfds) != napi_ok
  ) {
    napi_throw_type_error(
      env, NULL, "request(handle, type, body: ArrayBuffer, fds: number[])"
    );
    return NULL;
  }
  if (nfds > 1) {
    napi_throw_range_error(env, NULL, "a request carries at most one fd");
    return NULL;
  }
  if (nfds == 1) {
    napi_value v;
    napi_get_element(env, argv[3], 0, &v);
    if (napi_get_value_int32(env, v, &fd) != napi_ok || fd < 0) {
      napi_throw_type_error(env, NULL, "an fd is a number");
      return NULL;
    }
  }
  napi_deferred deferred;
  napi_value promise;
  napi_create_promise(env, &deferred, &promise);
  const int r =
    ik_connection_request(&conn->c, (uint16_t)type, data, len, fd, deferred);
  if (fd >= 0) close(fd);
  if (r != 0)
    napi_reject_deferred(env, deferred, make_error(env, "bridge request", r));
  return promise;
}

// blob(bytes: ArrayBuffer): number, a memfd sealed as the bridge requires.
static napi_value js_blob(napi_env env, napi_callback_info info) {
  napi_value argv[1];
  if (!get_args(env, info, 1, argv)) return NULL;
  void* data;
  size_t len;
  if (napi_get_arraybuffer_info(env, argv[0], &data, &len) != napi_ok) {
    napi_throw_type_error(env, NULL, "blob(bytes: ArrayBuffer)");
    return NULL;
  }
  const int fd =
    memfd_create("innkeeper-blob", MFD_CLOEXEC | MFD_ALLOW_SEALING);
  if (fd < 0) {
    throw_errno(env, "memfd_create", -errno);
    return NULL;
  }
  for (size_t off = 0; off < len;) {
    const ssize_t n = write(fd, (const char*)data + off, len - off);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) {
      const int err = n < 0 ? -errno : -EIO;
      close(fd);
      throw_errno(env, "blob write", err);
      return NULL;
    }
    off += (size_t)n;
  }
  if (
    fcntl(
      fd, F_ADD_SEALS, F_SEAL_WRITE | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL
    ) != 0
  ) {
    const int err = -errno;
    close(fd);
    throw_errno(env, "blob seal", err);
    return NULL;
  }
  return number(env, fd);
}

struct wait {
  char name[LXB_MAX_NAME + 1], value[LXB_MAX_NAME + 1];
  int result;
  napi_deferred deferred;
  napi_async_work work;
};

static void wait_execute(napi_env env, void* data) {
  (void)env;
  struct wait* w = data;
  do {
    w->result = WaitParameter(w->name, w->value, 0);
  } while (w->result == SYSPARAM_WAIT_TIMEOUT);
}

static void wait_complete(napi_env env, napi_status status, void* data) {
  struct wait* w = data;
  napi_value undefined;
  napi_get_undefined(env, &undefined);
  if (status == napi_ok && w->result == 0) {
    napi_resolve_deferred(env, w->deferred, undefined);
  } else {
    char text[64];
    snprintf(text, sizeof(text), "WaitParameter %s: %d", w->name, w->result);
    napi_value msg, error;
    napi_create_string_utf8(env, text, NAPI_AUTO_LENGTH, &msg);
    napi_create_error(env, NULL, msg, &error);
    napi_reject_deferred(env, w->deferred, error);
  }
  napi_delete_async_work(env, w->work);
  free(w);
}

// waitParameter(name: string, value: string): Promise<void>
static napi_value js_wait_parameter(napi_env env, napi_callback_info info) {
  napi_value argv[2];
  if (!get_args(env, info, 2, argv)) return NULL;
  struct wait* w = calloc(1, sizeof(*w));
  size_t len;
  if (
    !w ||
    napi_get_value_string_utf8(env, argv[0], w->name, sizeof(w->name), &len) !=
      napi_ok ||
    napi_get_value_string_utf8(
      env, argv[1], w->value, sizeof(w->value), &len
    ) != napi_ok
  ) {
    free(w);
    napi_throw_type_error(
      env, NULL, "waitParameter(name: string, value: string)"
    );
    return NULL;
  }
  napi_value promise, resource;
  napi_create_promise(env, &w->deferred, &promise);
  napi_create_string_utf8(env, "waitParameter", NAPI_AUTO_LENGTH, &resource);
  napi_create_async_work(
    env, NULL, resource, wait_execute, wait_complete, w, &w->work
  );
  napi_queue_async_work(env, w->work);
  return promise;
}

static napi_value init(napi_env env, napi_value exports) {
  const napi_property_descriptor props[] = {
    {"open", NULL, js_open, NULL, NULL, NULL, napi_default, NULL},
    {"onEvent", NULL, js_on_event, NULL, NULL, NULL, napi_default, NULL},
    {"request", NULL, js_request, NULL, NULL, NULL, napi_default, NULL},
    {"blob", NULL, js_blob, NULL, NULL, NULL, napi_default, NULL},
    {"waitParameter", NULL, js_wait_parameter, NULL, NULL, NULL, napi_default,
     NULL},
  };
  napi_define_properties(env, exports, sizeof(props) / sizeof(props[0]), props);
  return exports;
}

// OpenHarmony's runtime loads a module by its file name, libinnkeeper.so,
// and finds it by the name that it registers when the library loads.
static napi_module module = {
  .nm_version = 1,
  .nm_register_func = init,
  .nm_modname = "innkeeper",
};

__attribute__((constructor)) static void register_module(void) {
  napi_module_register(&module);
}
