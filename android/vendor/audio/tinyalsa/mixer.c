// tinyalsa mixer API for Linux.apk's virtual card: the controls Android's
// audio HALs look for. "Master Playback Volume/Switch" are forwarded to the
// bridge (PipeWire stream volume and mute); capture controls are stored only.

#include <errno.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <tinyalsa/mixer.h>

#include "linuxapk/audio.h"

#define API __attribute__((visibility("default")))

struct ctl_def {
  const char* name;
  enum mixer_ctl_type type;
  int min, max, def;
  bool master;  // forwarded to the bridge
};

static const struct ctl_def kCtls[] = {
  {"Master Playback Volume", MIXER_CTL_TYPE_INT, 0, 100, 100, true},
  {"Master Playback Switch", MIXER_CTL_TYPE_BOOL, 0, 1, 1, true},
  {"Capture Volume", MIXER_CTL_TYPE_INT, 0, 100, 100, false},
  {"Capture Switch", MIXER_CTL_TYPE_BOOL, 0, 1, 1, false},
};
#define NCTLS (sizeof(kCtls) / sizeof(kCtls[0]))

// Values are per card, shared by every mixer handle in the process.
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_values[NCTLS] = {100, 1, 100, 1};

struct mixer_ctl {
  struct mixer* mixer;
  unsigned int id;
};

struct mixer {
  struct mixer_ctl ctls[NCTLS];
};

API struct mixer* mixer_open(unsigned int card) {
  if (card != 0) return NULL;
  struct mixer* m = calloc(1, sizeof(*m));
  if (!m) return NULL;
  for (unsigned int i = 0; i < NCTLS; i++)
    m->ctls[i] = (struct mixer_ctl){m, i};
  return m;
}

API void mixer_close(struct mixer* m) { free(m); }
API int mixer_add_new_ctls(struct mixer* m) {
  (void)m;
  return 0;
}
API const char* mixer_get_name(const struct mixer* m) {
  (void)m;
  return "Linux.apk";
}
API unsigned int mixer_get_num_ctls(const struct mixer* m) {
  return m ? NCTLS : 0;
}

API unsigned int mixer_get_num_ctls_by_name(
  const struct mixer* m, const char* name
) {
  unsigned int n = 0;
  for (unsigned int i = 0; m && name && i < NCTLS; i++)
    n += strcmp(kCtls[i].name, name) == 0;
  return n;
}

API const struct mixer_ctl* mixer_get_ctl_const(
  const struct mixer* m, unsigned int id
) {
  return m && id < NCTLS ? &m->ctls[id] : NULL;
}
API struct mixer_ctl* mixer_get_ctl(struct mixer* m, unsigned int id) {
  return m && id < NCTLS ? &m->ctls[id] : NULL;
}

API struct mixer_ctl* mixer_get_ctl_by_name_and_index(
  struct mixer* m, const char* name, unsigned int index
) {
  for (unsigned int i = 0; m && name && i < NCTLS; i++) {
    if (strcmp(kCtls[i].name, name) == 0 && index-- == 0) return &m->ctls[i];
  }
  return NULL;
}
API struct mixer_ctl* mixer_get_ctl_by_name(struct mixer* m, const char* name) {
  return mixer_get_ctl_by_name_and_index(m, name, 0);
}
API struct mixer_ctl* mixer_get_ctl_by_name_and_device(
  struct mixer* m, const char* name, unsigned int device
) {
  return device == 0 ? mixer_get_ctl_by_name(m, name) : NULL;
}

API int mixer_subscribe_events(struct mixer* m, int subscribe) {
  (void)m, (void)subscribe;
  return -ENOSYS;
}
API int mixer_wait_event(struct mixer* m, int timeout) {
  (void)m, (void)timeout;
  return 0;
}
API int mixer_read_event(struct mixer* m, struct mixer_ctl_event* event) {
  (void)m, (void)event;
  return -ENOSYS;
}
API int mixer_consume_event(struct mixer* m) {
  (void)m;
  return -ENOSYS;
}

API unsigned int mixer_ctl_get_id(const struct mixer_ctl* c) {
  return c ? c->id : 0;
}
API const char* mixer_ctl_get_name(const struct mixer_ctl* c) {
  return c ? kCtls[c->id].name : NULL;
}
API enum mixer_ctl_type mixer_ctl_get_type(const struct mixer_ctl* c) {
  return c ? kCtls[c->id].type : MIXER_CTL_TYPE_UNKNOWN;
}
API const char* mixer_ctl_get_type_string(const struct mixer_ctl* c) {
  return c && kCtls[c->id].type == MIXER_CTL_TYPE_BOOL ? "BOOL" : "INT";
}
API unsigned int mixer_ctl_get_num_values(const struct mixer_ctl* c) {
  return c ? 1 : 0;
}
API unsigned int mixer_ctl_get_num_enums(const struct mixer_ctl* c) {
  (void)c;
  return 0;
}
API const char* mixer_ctl_get_enum_string(
  struct mixer_ctl* c, unsigned int enum_id
) {
  (void)c, (void)enum_id;
  return NULL;
}
API void mixer_ctl_update(struct mixer_ctl* c) { (void)c; }
API int mixer_ctl_is_access_tlv_rw(const struct mixer_ctl* c) {
  (void)c;
  return 0;
}
API int mixer_ctl_get_range_min(const struct mixer_ctl* c) {
  return c ? kCtls[c->id].min : -EINVAL;
}
API int mixer_ctl_get_range_max(const struct mixer_ctl* c) {
  return c ? kCtls[c->id].max : -EINVAL;
}
API unsigned int mixer_ctl_get_device(const struct mixer_ctl* c) {
  (void)c;
  return 0;
}

API int mixer_ctl_get_value(const struct mixer_ctl* c, unsigned int id) {
  if (!c || id != 0) return -EINVAL;
  pthread_mutex_lock(&g_lock);
  int v = g_values[c->id];
  pthread_mutex_unlock(&g_lock);
  return v;
}

API int mixer_ctl_set_value(struct mixer_ctl* c, unsigned int id, int value) {
  if (!c || id != 0) return -EINVAL;
  const struct ctl_def* d = &kCtls[c->id];
  if (value < d->min) value = d->min;
  if (value > d->max) value = d->max;
  pthread_mutex_lock(&g_lock);
  g_values[c->id] = value;
  float vol = g_values[0] / 100.0f;
  int mute = !g_values[1];
  pthread_mutex_unlock(&g_lock);
  // Not connected yet is fine: lxb_set_master remembers and applies it
  // when the first stream connects.
  if (d->master) lxb_set_master(vol, mute);
  return 0;
}

API int mixer_ctl_get_percent(const struct mixer_ctl* c, unsigned int id) {
  int v = mixer_ctl_get_value(c, id);
  if (v < 0) return v;
  const struct ctl_def* d = &kCtls[c->id];
  return (v - d->min) * 100 / (d->max - d->min);
}

API int mixer_ctl_set_percent(
  struct mixer_ctl* c, unsigned int id, int percent
) {
  if (!c) return -EINVAL;
  const struct ctl_def* d = &kCtls[c->id];
  if (percent < 0) percent = 0;
  if (percent > 100) percent = 100;
  return mixer_ctl_set_value(c, id, d->min + percent * (d->max - d->min) / 100);
}

API int mixer_ctl_get_array(
  const struct mixer_ctl* c, void* array, size_t count
) {
  if (!c || !array || count < 1) return -EINVAL;
  int v = mixer_ctl_get_value(c, 0);
  memcpy(array, &v, sizeof(v));
  return 0;
}

API int mixer_ctl_set_array(
  struct mixer_ctl* c, const void* array, size_t count
) {
  if (!c || !array || count < 1) return -EINVAL;
  int v;
  memcpy(&v, array, sizeof(v));
  return mixer_ctl_set_value(c, 0, v);
}

API int mixer_ctl_set_enum_by_string(struct mixer_ctl* c, const char* string) {
  (void)c, (void)string;
  return -EINVAL;
}
