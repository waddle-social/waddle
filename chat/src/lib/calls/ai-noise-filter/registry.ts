import type { NoiseModelId } from "./model-id";
import type { AudioNoiseProcessor } from "./processor";

/**
 * A loadable model backend: its id plus a factory for a fresh processor. Each
 * backend lives in its own module so its npm package and wasm/model assets are
 * pulled by exactly one dynamic `import()` — a user who never enables that
 * model (or picks a different one) downloads none of its bytes.
 */
export interface NoiseModelBackend {
  readonly id: NoiseModelId;
  createProcessor(): AudioNoiseProcessor;
}

/**
 * Per-model dynamic-import loaders. Only models with compliant, self-hosted
 * assets ship a backend. DeepFilterNet is intentionally ABSENT — it is a
 * deferred slot (see `support.ts`): its only ready-made package fetches its
 * model from a dead third-party CDN and is out-of-band besides.
 *
 * `Partial` makes the deferred slot explicit and type-safe: `makeNoiseProcessor`
 * must handle a missing backend, which can only be reached if support/UI gating
 * is ever bypassed.
 */
const NOISE_MODELS: Partial<Record<NoiseModelId, () => Promise<NoiseModelBackend>>> = {
  rnnoise: () => import("./models/rnnoise").then((m) => m.rnnoiseBackend),
  dtln: () => import("./models/dtln").then((m) => m.dtlnBackend),
};

/** Whether a model ships a self-hosted backend (vs. being a deferred slot). */
export function hasNoiseModelBackend(id: NoiseModelId): boolean {
  return Object.prototype.hasOwnProperty.call(NOISE_MODELS, id);
}

/**
 * Lazily load the backend for `id` and build a fresh processor. Rejects for a
 * deferred model with no backend — callers fail open to the raw mic.
 */
export async function makeNoiseProcessor(id: NoiseModelId): Promise<AudioNoiseProcessor> {
  const load = NOISE_MODELS[id];
  if (!load) throw new Error(`No self-hosted backend for noise model: ${id}`);
  const backend = await load();
  return backend.createProcessor();
}
