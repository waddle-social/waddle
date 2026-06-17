import { atom } from "nanostores";
import type { CallViewMode } from "./view-mode";

/**
 * Per-call stage presentation: the chosen Gallery/Speaker view mode and the
 * locally pinned tile. Both are module-scoped atoms (not component refs) so the
 * choice survives the split⟷expanded surface switch — which remounts the tile
 * grid — and persists for the duration of the call.
 *
 * Both reset to their defaults at the start of every fresh call via
 * [`resetCallViewState`] so a previous call's Speaker view or pin never leaks
 * in. Distinct from `$callUiMode` (the localStorage-backed split/expanded
 * surface preference), which deliberately outlives individual calls.
 *
 * The pin is local-only — it affects only this client's view and is never put
 * on the wire (the signalling-propagated host "Spotlight" is separate, future
 * work; see ADR-0016).
 */
export const $callViewMode = atom<CallViewMode>("gallery");

/** The locally pinned tile key, or `null` when nothing is pinned. */
export const $callPinnedTileKey = atom<string | null>(null);

export function setCallViewMode(mode: CallViewMode): void {
  $callViewMode.set(mode);
}

/** Pin a tile, or unpin it if it is already the pinned one. */
export function toggleCallPin(tileKey: string): void {
  $callPinnedTileKey.set($callPinnedTileKey.get() === tileKey ? null : tileKey);
}

/** Reset stage presentation to its per-call defaults: Gallery, nothing pinned. */
export function resetCallViewState(): void {
  $callViewMode.set("gallery");
  $callPinnedTileKey.set(null);
}
