// Exercise the real connection handler with a private socketpair, without
// binding the production /dev/socket path or starting Android.
#define main relay_main
#include "../innkeeper-exec.c"
#undef main

int main(int argc, char** argv) {
  if (argc != 2) return 1;
  serve_one(atoi(argv[1]));
  return 1;
}
