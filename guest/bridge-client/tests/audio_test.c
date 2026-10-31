// Shared-ring capture reads, including a producer disappearing during a read.
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <unistd.h>

#include "linuxapk/audio.h"

struct fixture {
  struct lxb_stream s;
  int notify;
};

static void setup(struct fixture* f) {
  memset(f, 0, sizeof(*f));
  f->s = (struct lxb_stream){
    .input = true,
    .capacity = 4,
    .frame_bytes = 2,
    .rate = 48000,
    .map_size = 8192
  };
  f->s.map =
    mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
  assert(f->s.map != MAP_FAILED);
  f->s.data_fd = eventfd(0, EFD_NONBLOCK);
  int pipefd[2];
  assert(pipe(pipefd) == 0);
  f->s.space_fd = pipefd[0];
  f->notify = pipefd[1];
}

static void teardown(struct fixture* f) {
  munmap(f->s.map, 8192);
  close(f->s.data_fd);
  close(f->s.space_fd);
  if (f->notify >= 0) close(f->notify);
}

static void* partial_then_close(void* arg) {
  struct fixture* f = arg;
  // Observe that the reader consumed our initial samples and is waiting.
  while (atomic_load((_Atomic uint64_t*)(f->s.map + 128)) != 2) sched_yield();
  atomic_store((_Atomic uint32_t*)(f->s.map + 224), 2);
  close(f->notify);  // POLLHUP also wakes clients after abrupt bridge death
  return NULL;
}

int main(void) {
  struct fixture f;
  setup(&f);
  int16_t out[8] = {0};
  assert(lxb_read(&f.s, out, 4, 5) == 0);  // underrun, bounded wait
  const int16_t samples[] = {123, -456};
  memcpy(f.s.map + 4096, samples, sizeof(samples));
  atomic_store((_Atomic uint64_t*)(f.s.map + 64), 2);
  pthread_t thread;
  assert(pthread_create(&thread, NULL, partial_then_close, &f) == 0);
  assert(lxb_read(&f.s, out, 4, 100) == 2);  // preserves partial read
  assert(memcmp(out, samples, sizeof(samples)) == 0);
  assert(pthread_join(thread, NULL) == 0);
  f.notify = -1;
  assert(lxb_read(&f.s, out, 4, 100) == -EPIPE);
  teardown(&f);

  setup(&f);
  close(f.notify);  // bridge killed before it can set STATE_DEAD
  f.notify = -1;
  assert(lxb_read(&f.s, out, 4, 100) == -EPIPE);
  teardown(&f);
  return 0;
}
