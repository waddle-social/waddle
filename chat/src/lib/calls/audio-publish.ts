/**
 * Pure publish-option builder for the local microphone track. Mirrors
 * `video-publish.ts`: it returns the typed LiveKit `TrackPublishOptions` the
 * engine forwards verbatim to `setMicrophoneEnabled`, keeping the engine a thin
 * applier. The default voice-clarity profile raises Opus to ~64 kbps mono.
 */

import type { TrackPublishOptions } from "livekit-client";

/**
 * ~64 kbps Opus, musicHighQuality-class voice clarity. We state it as an
 * explicit `AudioPreset` rather than reaching for a named `AudioPresets`
 * constant: `AudioPresets.musicStereo` is also 64k but forces stereo, and
 * `AudioPresets.musicHighQuality` is 96k — neither is "64k mono". A bare
 * `{ maxBitrate }` keeps the channel count governed by `forceStereo` below.
 */
const OPUS_VOICE_CLARITY_BITRATE = 64_000;

export function audioPublishOptions(): TrackPublishOptions {
  return {
    audioPreset: { maxBitrate: OPUS_VOICE_CLARITY_BITRATE },
    // RED (redundant audio) for packet-loss resilience and DTX (discontinuous
    // transmission) so silent participants cost ~nothing. Both default on for
    // mono tracks in LiveKit, but we set them explicitly so the contract is
    // locked and testable rather than relying on the SDK's implicit default.
    red: true,
    dtx: true,
    // Mono. The 64k target is mono, and DTX/RED above only auto-apply to mono
    // tracks; pinning this off stops a stereo capture device from doubling the
    // channel count (and the bitrate) behind our back.
    forceStereo: false,
  };
}
