import type { AudioProcessingPrefs } from "../device-prefs";
import type { NoiseModelId } from "./model-id";

/**
 * The browser-native audio-processing constraints to actually request,
 * given the user's stored prefs and which AI noise model (if any) is active.
 *
 * When a model runs it *is* the noise suppression, so the browser's own
 * `noiseSuppression` is forced off — two suppressors in series produce
 * artifacts and the browser stage distorts the input the model expects.
 * Echo cancellation and auto gain are different jobs (AEC needs the far-end
 * reference only WebRTC has; AGC is level control) that the models don't do,
 * so those stay exactly as the user set them.
 *
 * Pure and non-mutating: the stored `noiseSuppression` pref is never
 * clobbered, so it returns automatically when the model is set back to off.
 */
export function effectiveAudioProcessing(
  prefs: AudioProcessingPrefs,
  model: NoiseModelId | null,
): AudioProcessingPrefs {
  if (model === null) return { ...prefs };
  return { ...prefs, noiseSuppression: false };
}
