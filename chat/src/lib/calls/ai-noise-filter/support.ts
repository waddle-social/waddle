/**
 * Capability probe: which AI noise models this browser can actually run.
 *
 * The first of two layers (the second is the runtime fail-open guard in the
 * reconciler). It never offers a model that is guaranteed to fail:
 *  - RNNoise and DTLN need `AudioWorklet` (they run inside one) and are
 *    otherwise fully self-hosted, single-threaded, no cross-origin isolation.
 *  - DeepFilterNet is a deferred slot: its only ready-made browser package
 *    fetches its model from a (now-dead) third-party CDN and is out-of-band
 *    besides, so it ships disabled until we vendor compliant self-hosted
 *    assets. Surfaced disabled-with-reason rather than hidden so the UI can
 *    show it's coming.
 *
 * Pure over an injected env so it is unit-tested without touching globals.
 */

import { NOISE_MODEL_IDS, type NoiseModelId } from "./model-id";

export type NoiseModelSupportEnv = {
  /** `AudioWorklet` is usable (the worklet models run inside one). */
  hasAudioWorklet: boolean;
};

type NoiseModelAvailability = { available: true } | { available: false; reason: string };

export type NoiseModelSupport = Record<NoiseModelId, NoiseModelAvailability>;

const NO_AUDIO_WORKLET = "Your browser doesn't support AudioWorklet.";
const DEEPFILTERNET_DEFERRED = "Coming soon — self-hosted assets pending.";

/** Per-model availability for the current environment. */
export function noiseModelSupport(env: NoiseModelSupportEnv): NoiseModelSupport {
  const workletModel: NoiseModelAvailability = env.hasAudioWorklet
    ? { available: true }
    : { available: false, reason: NO_AUDIO_WORKLET };
  return {
    rnnoise: workletModel,
    dtln: workletModel,
    deepfilternet: { available: false, reason: DEEPFILTERNET_DEFERRED },
  };
}

/** True when at least one model can run — gates the whole settings control. */
export function anyNoiseModelAvailable(support: NoiseModelSupport): boolean {
  return NOISE_MODEL_IDS.some((id) => support[id].available);
}

/** Read the real environment. Thin; the decision logic lives in the pure probe. */
export function currentNoiseModelSupportEnv(): NoiseModelSupportEnv {
  return {
    hasAudioWorklet:
      typeof AudioWorkletNode !== "undefined" && typeof AudioContext !== "undefined",
  };
}
