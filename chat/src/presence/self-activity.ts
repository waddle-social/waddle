// The user's own manually-chosen XEP-0108 activity (set via Settings or the
// feed composer), or null. XEP-0108 has a single current activity item, and the
// in-call overlay (ADR-010 Phase 3) temporarily overrides what's published on
// the wire — so this store is the activity to restore when a call ends, rather
// than blanket-retracting and losing the user's status. The ActivityCoordinator
// is the sole writer of the node; everything else writes intent here.

import { atom } from "nanostores";

import type { ActivityPublication } from "@/lib/xmpp/pep-types";

export const $manualActivity = atom<ActivityPublication | null>(null);

/** Record the user's chosen activity (Settings / feed composer publish). */
export function setManualActivity(activity: ActivityPublication): void {
  $manualActivity.set(activity);
}

/** Clear the user's chosen activity (Settings / feed composer clear). */
export function clearManualActivity(): void {
  $manualActivity.set(null);
}

/** Forget the manual activity on logout so the next account starts clean. */
export function resetManualActivity(): void {
  $manualActivity.set(null);
}
