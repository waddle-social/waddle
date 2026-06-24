// This device's own presence mode, as a nanostore so the account-menu
// picker can drive it directly without threading props through the shell.
// In-memory for now — cross-device sync + persistence is Phase 4 (ADR-010).

import { atom, computed } from "nanostores";

import { applyPick, resolveShow, type PresenceMode, type PresencePick } from "./effective-show";

/** The user's chosen presence mode on this device. Defaults to Automatic. */
export const $presenceMode = atom<PresenceMode>({ kind: "automatic" });

/** The Show this device currently broadcasts, derived from the mode. */
export const $selfShow = computed($presenceMode, (mode) => resolveShow(mode));

/** Apply a picker choice — Available / Away / Do Not Disturb, or `reset` to Automatic. */
export function pickPresence(pick: PresencePick): void {
  $presenceMode.set(applyPick(pick));
}

/**
 * Reset to Automatic. Call on logout: this store is module-global, so a
 * manual Away/DND chosen by one account would otherwise leak to the next
 * account signing in within the same tab.
 */
export function resetPresenceMode(): void {
  $presenceMode.set({ kind: "automatic" });
}
