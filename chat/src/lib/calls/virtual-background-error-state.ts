import { atom } from "nanostores";
import type { ActiveVirtualBackgroundEffect } from "./virtual-background/processor";

export const $virtualBackgroundError = atom<ActiveVirtualBackgroundEffect | null>(null);

export function setVirtualBackgroundError(effect: ActiveVirtualBackgroundEffect): void {
  $virtualBackgroundError.set(effect);
}

export function clearVirtualBackgroundError(): void {
  if ($virtualBackgroundError.get() !== null) $virtualBackgroundError.set(null);
}
