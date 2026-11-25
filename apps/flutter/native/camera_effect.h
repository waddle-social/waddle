#ifndef WADDLE_FLUTTER_CAMERA_EFFECT_H_
#define WADDLE_FLUTTER_CAMERA_EFFECT_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct WaddleCameraEffect WaddleCameraEffect;

typedef struct {
  const uint8_t* data;
  size_t length;
  size_t stride;
} WaddleCameraInputPlane;

typedef struct {
  uint8_t* data;
  size_t length;
  size_t stride;
} WaddleCameraOutputPlane;

// Pass NULL, 0, 0, 0 for blur. Otherwise provide an upright RGBA8888 image.
// Creation copies the image. A NULL handle indicates invalid input or model load failure.
WaddleCameraEffect* waddle_camera_effect_create(const uint8_t* rgba, size_t length,
                                                uint32_t width, uint32_t height);

// The adapter owns three disjoint, correctly sized Y/U/V destination planes.
// Supply clockwise display rotation in degrees (0, 90, 180, 270). Output retains
// input dimensions and orientation. On false, suppress the frame or send neutral pixels.
bool waddle_camera_effect_process(WaddleCameraEffect* effect, uint32_t width, uint32_t height,
                                  uint32_t rotation, const WaddleCameraInputPlane input[3],
                                  const WaddleCameraOutputPlane output[3]);

// Detach the capture callback and wait for its return before destroying the handle.
void waddle_camera_effect_destroy(WaddleCameraEffect* effect);

#ifdef __cplusplus
}
#endif
#endif
