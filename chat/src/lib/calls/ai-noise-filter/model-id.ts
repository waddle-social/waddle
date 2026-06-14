/**
 * Identity of a client-side AI noise-suppression model and the codec that
 * lets the call engine read back *which* model is live from a LiveKit
 * processor's `name`.
 *
 * The #911 verified indicator reads the capture source's `getSettings()`,
 * which cannot see a track processor at all. So the honest source of "is
 * the AI filter on, and which model?" is the attached processor itself:
 * every model's `TrackProcessor.name` is `processorName(id)`, and
 * `modelIdFromProcessorName` recovers the id from `getProcessor()?.name`.
 * Keeping the encode/decode pure and in one place means the engine never
 * hand-parses processor names at call sites.
 */

/** A supported in-browser WASM noise-suppression model, light-to-heavy. */
export type NoiseModelId = "rnnoise" | "dtln" | "deepfilternet";

/** Canonical ordered set — render order and iteration order for the UI. */
export const NOISE_MODEL_IDS = ["rnnoise", "dtln", "deepfilternet"] as const;

/**
 * Namespace for every Waddle AI-noise-filter processor name. Distinct from
 * LiveKit's own processor names (e.g. `lk-krisp-noise-filter`) so the
 * decoder never mistakes a third-party processor for ours.
 */
const PROCESSOR_NAME_PREFIX = "waddle-ai-noise-filter";

/** Encode a model id into the `TrackProcessor.name` we attach to the mic. */
export function processorName(id: NoiseModelId): string {
  return `${PROCESSOR_NAME_PREFIX}:${id}`;
}

/**
 * Recover the model id from a live processor's `name`, or `null` when the
 * name is absent (no processor attached), belongs to a different processor,
 * or carries a suffix we don't recognise.
 */
export function modelIdFromProcessorName(name: string | undefined): NoiseModelId | null {
  if (name === undefined) return null;
  const prefix = `${PROCESSOR_NAME_PREFIX}:`;
  if (!name.startsWith(prefix)) return null;
  const suffix = name.slice(prefix.length);
  return isNoiseModelId(suffix) ? suffix : null;
}

/** Narrow untrusted input (persisted prefs, decoded names) to a model id. */
export function isNoiseModelId(value: unknown): value is NoiseModelId {
  return (
    typeof value === "string" &&
    (NOISE_MODEL_IDS as readonly string[]).includes(value)
  );
}
