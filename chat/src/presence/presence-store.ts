// This device's own presence mode, as a nanostore so the account-menu
// picker can drive it directly without threading props through the shell.
// In-memory for now — cross-device sync + persistence is Phase 4 (ADR-010).

import { atom, computed } from "nanostores";

import { applyPick, resolveShow, type BroadcastShow, type PresenceMode, type PresencePick } from "./effective-show";
import { presenceSuppressesOwnNotifications } from "./own-notification-gate";

/** The user's chosen presence mode on this device. Defaults to Automatic. */
export const $presenceMode = atom<PresenceMode>({ kind: "automatic" });

/** The Show this device currently broadcasts, derived from the mode. */
export const $selfShow = computed($presenceMode, (mode) => resolveShow(mode));

// What the idle tracker last decided to broadcast, auto-away included.
const $selfBroadcastShow = atom<BroadcastShow | null>(null);

/** Record the Show this device broadcasts (the idle tracker's every emit). */
export function setSelfBroadcastShow(show: BroadcastShow): void {
  $selfBroadcastShow.set(show);
}

/** The user's own presence as others see it: the broadcast Show, or the
 * chosen mode's Show before the first broadcast. */
export const $selfPresenceShow = computed(
  [$selfBroadcastShow, $selfShow],
  (broadcast, chosen): BroadcastShow => broadcast ?? chosen,
);

/**
 * Whether this device should silence its OWN message banners/sounds because
 * the Effective Show is Do Not Disturb (ADR-010 Phase 5a). Badge / unread
 * counts are unaffected. Orthogonal to the server-side `urn:waddle:dnd:0`
 * quiet-hours schedule; both can suppress, for different reasons.
 */
export const $ownNotificationsSuppressed = computed($selfShow, (show) =>
  presenceSuppressesOwnNotifications(show)
);

/** Apply a picker choice — Available / Free to chat / Away / Do Not Disturb, or `reset` to Automatic. */
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
  $selfBroadcastShow.set(null);
}
