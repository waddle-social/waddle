#ifndef WADDLE_FLUTTER_AUDIO_FILTER_H_
#define WADDLE_FLUTTER_AUDIO_FILTER_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct WaddleAudioFilter WaddleAudioFilter;

enum WaddleAudioFilterMode {
  WADDLE_AUDIO_BYPASS = 0,
  WADDLE_AUDIO_RNNOISE = 1,
  WADDLE_AUDIO_DTLN = 2,
};

// Prepare outside the capture callback. Accepts 8,000–96,000 Hz and one or two channels. Each handle owns
// one capture's filter history. Replace it when the input rate or layout changes.
WaddleAudioFilter* waddle_audio_filter_create(uint32_t mode, uint32_t sample_rate,
                                              uint32_t channels);

// In-place, interleaved signed PCM16. Supply whole channel frames, up to 100 ms per call.
// Count includes every channel. A false result
// leaves the supplied samples untouched. Calls on one handle must be serialized.
bool waddle_audio_filter_process(WaddleAudioFilter* filter, int16_t* interleaved,
                                 size_t sample_count);

// Discard all pending audio and recurrent history before resuming a muted stream.
void waddle_audio_filter_reset(WaddleAudioFilter* filter);

// Detach and wait for in-flight capture callbacks before destroying the handle.
void waddle_audio_filter_destroy(WaddleAudioFilter* filter);

#ifdef __cplusplus
}
#endif
#endif
