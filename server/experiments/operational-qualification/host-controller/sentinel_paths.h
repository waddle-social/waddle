#ifndef WADDLE_SENTINEL_PATHS_H
#define WADDLE_SENTINEL_PATHS_H

#include <stddef.h>
#include <stdio.h>
#include <string.h>

#define SENTINEL_PATH_BYTES 108

/* Pure fixed-path formatting, shared with fixtures; no filesystem access. */
static inline int sentinel_paths(char *file, size_t file_size, char *path,
                                 size_t path_size, unsigned long uid,
                                 const char *nonce) {
  if (uid == 0 || uid >= 4294967295UL || strlen(nonce) != 32 ||
      strspn(nonce, "0123456789abcdef") != 32)
    return -1;
  int count =
      snprintf(file, file_size, "/run/user/%lu/waddle-host-%lu-%s/host-file",
               uid, uid, nonce);
  if (count <= 0 || (size_t)count >= file_size)
    return -1;
  count =
      snprintf(path, path_size, "/run/user/%lu/waddle-host-%lu-%s/host.sock",
               uid, uid, nonce);
  return count > 0 && (size_t)count < path_size ? 0 : -1;
}
#endif
