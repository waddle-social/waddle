import { atom } from "nanostores";
import type { NoiseModelId } from "./ai-noise-filter/model-id";

/**
 * The model whose filter most recently failed to attach on the active call,
 * or `null` when there is no outstanding failure. Drives the non-blocking
 * "couldn't start the {model} filter — using your raw mic" notice in the
 * call-settings dialog. The engine fails open (raw mic keeps flowing); this is
 * purely the UI signal.
 */
export const $aiNoiseFilterError = atom<NoiseModelId | null>(null);

export function setAiNoiseFilterError(model: NoiseModelId): void {
  $aiNoiseFilterError.set(model);
}

export function clearAiNoiseFilterError(): void {
  if ($aiNoiseFilterError.get() !== null) $aiNoiseFilterError.set(null);
}
