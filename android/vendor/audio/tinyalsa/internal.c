// Symbols the stock libtinyalsav2.so exports beyond its public headers
// (plugin and sync helpers). No HAL we know of uses them; they exist so a
// consumer that does still loads, and they report "no such device".

#include <errno.h>
#include <stddef.h>
#include <tinyalsa/pcm.h>

#define API __attribute__((visibility("default")))

struct mixer_ops;
struct mixer_plugin;
struct snd_node;

API int mixer_hw_open(
  unsigned int card, void** data, const struct mixer_ops** ops
) {
  (void)card, (void)data, (void)ops;
  return -ENODEV;
}
API int mixer_plugin_open(
  unsigned int card, void** data, const struct mixer_ops** ops
) {
  (void)card, (void)data, (void)ops;
  return -ENODEV;
}
API void mixer_plug_notifier_cb(struct mixer_plugin* plugin) { (void)plugin; }

API int pcm_avail_update(struct pcm* pcm) { return pcm_mmap_avail(pcm); }
API int pcm_state(struct pcm* pcm) {
  return pcm_is_ready(pcm) ? PCM_STATE_RUNNING : -EBADFD;
}

API struct snd_node* snd_utils_open_pcm(
  unsigned int card, unsigned int device
) {
  (void)card, (void)device;
  return NULL;
}
API struct snd_node* snd_utils_open_mixer(unsigned int card) {
  (void)card;
  return NULL;
}
API void snd_utils_close_dev_node(struct snd_node* node) { (void)node; }
API int snd_utils_get_node_type(struct snd_node* node) {
  (void)node;
  return 0;  // SND_NODE_TYPE_HW
}
API int snd_utils_get_int(struct snd_node* node, const char* prop, int* val) {
  (void)node, (void)prop, (void)val;
  return -EINVAL;
}
API int snd_utils_get_str(struct snd_node* node, const char* prop, char** val) {
  (void)node, (void)prop, (void)val;
  return -EINVAL;
}
