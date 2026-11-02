// SPDX-License-Identifier: AGPL-3.0-or-later
// The probe's native module, `libprobe.so`. A third-party app loads its N-API
// modules from its own HAP's libs/x86_64, and the probe's `--ps mode native`
// checks that this works. oh_modules/libprobe.so/index.d.ts declares what it
// exports.

#include <node_api.h>

// add(a, b) returns a + b.
static napi_value add(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  double a = 0, b = 0;
  napi_value sum;
  napi_get_cb_info(env, info, &argc, argv, NULL, NULL);
  if (argc == 2) {
    napi_get_value_double(env, argv[0], &a);
    napi_get_value_double(env, argv[1], &b);
  }
  napi_create_double(env, a + b, &sum);
  return sum;
}

static napi_value init(napi_env env, napi_value exports) {
  napi_property_descriptor props[] = {
    {"add", NULL, add, NULL, NULL, NULL, napi_default, NULL},
  };
  napi_define_properties(env, exports, 1, props);
  return exports;
}

// OpenHarmony's runtime loads a module by its file name, libprobe.so, and
// finds it by the name that it registers when the library loads.
static napi_module module = {
  .nm_version = 1,
  .nm_register_func = init,
  .nm_modname = "probe",
};

__attribute__((constructor)) static void register_module(void) {
  napi_module_register(&module);
}
