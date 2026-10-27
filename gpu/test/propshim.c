/*
 * Test-only LD_PRELOAD shim: answers Bionic system property lookups for the
 * names in $LAPK_PROPS ("name=value;name=value"), so system libraries such as
 * libvulkan can pick a driver in run-bionic.sh, where there is no init or
 * property service. Everything else goes to libc.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define MAX_PROPS 32

struct fake_prop {
   uint32_t magic;
   char name[128];
   char value[92];
};

static struct fake_prop props[MAX_PROPS];
static int nprops = -1;

static void init(void)
{
   if (nprops >= 0)
      return;
   nprops = 0;
   const char *env = getenv("LAPK_PROPS");
   if (!env)
      return;
   char *dup = strdup(env), *save = NULL;
   for (char *tok = strtok_r(dup, ";", &save); tok && nprops < MAX_PROPS;
        tok = strtok_r(NULL, ";", &save)) {
      char *eq = strchr(tok, '=');
      if (!eq)
         continue;
      *eq = 0;
      struct fake_prop *p = &props[nprops++];
      p->magic = 0x50524f50;
      strncpy(p->name, tok, sizeof(p->name) - 1);
      strncpy(p->value, eq + 1, sizeof(p->value) - 1);
   }
   free(dup);
}

static struct fake_prop *lookup_name(const char *name)
{
   init();
   for (int i = 0; i < nprops; i++)
      if (!strcmp(props[i].name, name))
         return &props[i];
   return NULL;
}

static struct fake_prop *as_fake(const void *pi)
{
   init();
   for (int i = 0; i < nprops; i++)
      if (pi == &props[i])
         return &props[i];
   return NULL;
}

#define REAL(ret, name, ...) \
   static ret (*real)(__VA_ARGS__); \
   if (!real) real = (ret (*)(__VA_ARGS__))dlsym(RTLD_NEXT, name)

__attribute__((visibility("default"))) const void *__system_property_find(const char *name)
{
   struct fake_prop *p = lookup_name(name);
   if (p)
      return p;
   REAL(const void *, "__system_property_find", const char *);
   return real(name);
}

__attribute__((visibility("default"))) void
__system_property_read_callback(const void *pi,
                                void (*cb)(void *, const char *, const char *, uint32_t),
                                void *cookie)
{
   struct fake_prop *p = as_fake(pi);
   if (p) {
      cb(cookie, p->name, p->value, 1);
      return;
   }
   REAL(void, "__system_property_read_callback", const void *,
        void (*)(void *, const char *, const char *, uint32_t), void *);
   real(pi, cb, cookie);
}

__attribute__((visibility("default"))) int __system_property_read(const void *pi, char *name,
                                                                  char *value)
{
   struct fake_prop *p = as_fake(pi);
   if (p) {
      if (name)
         strcpy(name, p->name);
      strcpy(value, p->value);
      return strlen(p->value);
   }
   REAL(int, "__system_property_read", const void *, char *, char *);
   return real(pi, name, value);
}

__attribute__((visibility("default"))) int __system_property_get(const char *name, char *value)
{
   struct fake_prop *p = lookup_name(name);
   if (p) {
      strcpy(value, p->value);
      return strlen(p->value);
   }
   REAL(int, "__system_property_get", const char *, char *);
   return real(name, value);
}

__attribute__((visibility("default"))) uint32_t __system_property_serial(const void *pi)
{
   if (as_fake(pi))
      return 1;
   REAL(uint32_t, "__system_property_serial", const void *);
   return real(pi);
}
