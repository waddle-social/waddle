#ifndef WADDLE_FLUTTER_RUNTIME_H
#define WADDLE_FLUTTER_RUNTIME_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* Independent handles. Zero means initialization/acceptance failure. */
uint64_t waddle_flutter_create(void);
/* request_json remains caller-owned and readable through this nonblocking call. */
uint64_t waddle_flutter_submit(uint64_t handle, const char *request_json);
/* Owned UTF-8 JSON array. Release exactly once with free_json. */
char *waddle_flutter_poll(uint64_t handle);
void waddle_flutter_free_json(char *buffer);
/* Idempotent. Retires the handle immediately, then disconnects asynchronously. */
void waddle_flutter_destroy(uint64_t handle);
#ifdef __cplusplus
}
#endif
#endif
