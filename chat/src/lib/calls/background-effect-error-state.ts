import { atom } from "nanostores";
import type { ActiveBackgroundEffect } from "./background-effect/effect-id";

/**
 * The effect whose processor most recently failed to attach on the active call,
 * or `null` when there is no outstanding failure. Drives the non-blocking
 * "couldn't start the background — using your raw camera" notice in the
 * call-settings dialog. The engine fails open (the raw camera keeps flowing);
 * this is purely the UI signal.
 */
export const $backgroundEffectError = atom<ActiveBackgroundEffect | null>(null);

export function setBackgroundEffectError(effect: ActiveBackgroundEffect): void {
  $backgroundEffectError.set(effect);
}

export function clearBackgroundEffectError(): void {
  if ($backgroundEffectError.get() !== null) $backgroundEffectError.set(null);
}
