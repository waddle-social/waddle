#include "sentinel_paths.h"
#include <sys/un.h>

_Static_assert(SENTINEL_PATH_BYTES ==
                   sizeof(((struct sockaddr_un *)0)->sun_path),
               "Sentinel paths must fit the Linux pathname socket buffer");

int main(void) {
  static const char nonce[] = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const unsigned long uids[] = {1, 1000, 4294967294UL};
  char file[SENTINEL_PATH_BYTES], path[SENTINEL_PATH_BYTES];
  for (size_t i = 0; i < sizeof(uids) / sizeof(uids[0]); ++i) {
    if (sentinel_paths(file, sizeof(file), path, sizeof(path), uids[i],
                       nonce) != 0 ||
        strlen(file) >= SENTINEL_PATH_BYTES ||
        strlen(path) >= SENTINEL_PATH_BYTES || puts(file) < 0 || puts(path) < 0)
      return 1;
  }
  const unsigned long invalid[] = {0, 4294967295UL};
  for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i)
    if (sentinel_paths(file, sizeof(file), path, sizeof(path), invalid[i],
                       nonce) == 0)
      return 1;
  if (sentinel_paths(file, 8, path, sizeof(path), 1000, nonce) == 0 ||
      sentinel_paths(file, sizeof(file), path, 8, 1000, nonce) == 0 ||
      sentinel_paths(file, sizeof(file), path, sizeof(path), 1000, "../") == 0)
    return 1;
  return 0;
}
