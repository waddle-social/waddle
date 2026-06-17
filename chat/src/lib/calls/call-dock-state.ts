import { atom } from "nanostores";
import { $callState } from "./call-store";

/**
 * Whether the in-call **Dock** (the Participants tab) is open. A plain
 * module-scoped atom, not persisted: the dock survives the split↔expanded
 * surface remounts within one call (both surfaces read this single store),
 * but each fresh call starts with the dock closed.
 *
 * Only meaningful in the Expanded surface, where the dock reflows the
 * stage. The Split surface has no room for it, so its Participants button
 * opens the dock and bumps the call to Expanded instead of toggling here.
 */
export const $callDockOpen = atom<boolean>(false);

export function openCallDock(): void {
  $callDockOpen.set(true);
}

export function closeCallDock(): void {
  $callDockOpen.set(false);
}

export function toggleCallDock(): void {
  $callDockOpen.set(!$callDockOpen.get());
}

// Close the dock whenever the call leaves the active phase, so a stale
// "open" can't carry into the next call. Subscribed once at module load.
$callState.subscribe((state) => {
  if (state.phase !== "active") closeCallDock();
});
