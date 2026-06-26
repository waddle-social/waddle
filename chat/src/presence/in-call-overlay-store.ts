// Receiver-side state for the in-call presence overlay (ADR-010 Phase 3).
// Tracks which contacts are currently in a call, derived from their inbound
// XEP-0108 activity. Keyed by bare JID — XEP-0108 rides PEP, which is
// per-account, so unlike the per-resource presence dot there is nothing to
// aggregate. Absent = not in a call.

import { map } from "nanostores";
import { computed, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";

import { barePeerJid } from "@/lib/xmpp/jid";

// Module-private — reads go through `isInCall` / the composables, writes
// through `setInCallOverlay` / `clearInCallOverlay`, so the keying (bare JID)
// stays in one place.
const $inCallOverlays = map<Record<string, true>>({});

/** Set or clear a contact's in-call overlay. A full JID is collapsed to bare. */
export function setInCallOverlay(jid: string, inCall: boolean): void {
  const bare = barePeerJid(jid);
  if (!bare) return;
  const current = $inCallOverlays.get();
  if (inCall === (current[bare] === true)) return; // already in this state — no churn
  if (inCall) {
    $inCallOverlays.set({ ...current, [bare]: true });
  } else {
    const { [bare]: _gone, ...rest } = current;
    $inCallOverlays.set(rest);
  }
}

/**
 * Ghost-call cleanup: a contact whose presence goes `unavailable` can no longer
 * be in a call, so clear any overlay a crashed client left published (ADR-010
 * belt-and-suspenders for a never-retracted activity item).
 */
export function clearInCallOverlay(jid: string): void {
  setInCallOverlay(jid, false);
}

/** Forget every overlay. Call on logout / fresh reconnect. */
export function resetInCallOverlays(): void {
  $inCallOverlays.set({});
}

/** Whether a contact is in a call, against a snapshot (defaults to live state). */
export function isInCall(jid: string, snapshot: Record<string, true> = $inCallOverlays.get()): boolean {
  const bare = barePeerJid(jid);
  return bare.length > 0 && snapshot[bare] === true;
}

/** Reactive "is this contact in a call" for a single, known JID. */
export function useInCallOverlay(jid: () => string | null | undefined): ComputedRef<boolean> {
  const overlays = useStore($inCallOverlays);
  return computed(() => isInCall(jid() ?? "", overlays.value));
}

/**
 * Reactive per-contact lookup for list rows, where `v-for` can't call the
 * single-JID composable per row. Returns a `peerInCall(jid)` reader backed by
 * the live store, so a render reflects the current overlays.
 */
export function useInCallOverlays(): { peerInCall: (jid: string) => boolean } {
  const overlays = useStore($inCallOverlays);
  return { peerInCall: (jid: string) => isInCall(jid, overlays.value) };
}
