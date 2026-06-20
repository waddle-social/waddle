import { atom } from "nanostores";

/**
 * Self mic / mute state, owned by a leaf module that imports nothing from
 * the call surfaces — so the control bar (`call-controls.ts`) and the
 * call-presence broadcaster (`muc-call-actions.ts`) can both read it
 * without importing each other. Mirrors how `call-raised-hand.ts` owns the
 * self raised-hand state independently; keeping this here is what keeps
 * those two modules acyclic (the control bar calls the broadcaster, never
 * the reverse).
 *
 * `$callMicEnabled` mirrors the engine: when the user toggles mic via a
 * control bar, the engine `setMicEnabled(false)` and the atom flip
 * together; on failure the atom rolls back. It is the single source of
 * truth for self-mute (there is no separate mute-intent store).
 */
export const $callMicEnabled = atom<boolean>(true);

/**
 * This client's current self-mute, derived from the live mic toggle: muted
 * ⇔ the mic is disabled. Every MUC call-presence re-emit must carry the
 * CURRENT value so toggling the raised hand (or resuming) can't silently
 * drop the `<muted/>` marker — presence is last-writer-wins (#1030).
 */
export function selfCallMuted(): boolean {
  return !$callMicEnabled.get();
}
