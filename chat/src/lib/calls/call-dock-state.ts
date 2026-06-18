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

/** Which tab the dock currently shows. */
export type CallDockTab = "participants" | "chat";

/**
 * The selected dock tab. Like {@link $callDockOpen} it is module-scoped and
 * survives the split⟷expanded remount within a call, and resets to
 * `participants` once the call ends.
 */
export const $callDockTab = atom<CallDockTab>("participants");

export function openCallDock(): void {
  $callDockOpen.set(true);
}

export function closeCallDock(): void {
  $callDockOpen.set(false);
}

export function toggleCallDock(): void {
  $callDockOpen.set(!$callDockOpen.get());
}

export function setCallDockTab(tab: CallDockTab): void {
  $callDockTab.set(tab);
}

/**
 * Toggle the dock for a given tab from a control-bar button: if the dock is
 * already open on that tab, close it; otherwise select the tab and open it.
 * A click on the *other* tab's button just switches tabs without closing.
 */
function toggleCallDockTab(tab: CallDockTab): void {
  if ($callDockOpen.get() && $callDockTab.get() === tab) {
    closeCallDock();
    return;
  }
  $callDockTab.set(tab);
  openCallDock();
}

export function toggleCallParticipants(): void {
  toggleCallDockTab("participants");
}

export function toggleCallChat(): void {
  toggleCallDockTab("chat");
}

// Close the dock and reset the tab whenever the call leaves the active phase,
// so a stale "open" or tab can't carry into the next call. Subscribed once at
// module load.
$callState.subscribe((state) => {
  if (state.phase !== "active") {
    closeCallDock();
    $callDockTab.set("participants");
  }
});
