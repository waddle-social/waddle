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

/**
 * Whether the local participant has hidden their own self-view tile from the
 * stage. Local-only and view-only: the outgoing camera keeps publishing — this
 * just removes the self tile from this client's grid (see `projectCallTiles`),
 * which also keeps the local participant off the large Speaker tile.
 */
export const $callSelfViewHidden = atom<boolean>(false);

export function setCallViewMode(mode: CallViewMode): void {
  $callViewMode.set(mode);
}

/** Pin a tile, or unpin it if it is already the pinned one. */
export function toggleCallPin(tileKey: string): void {
  $callPinnedTileKey.set($callPinnedTileKey.get() === tileKey ? null : tileKey);
}

/** Hide the self-view tile, or reveal it if it is already hidden. */
export function toggleCallSelfViewHidden(): void {
  $callSelfViewHidden.set(!$callSelfViewHidden.get());
}

/**
 * Reset stage presentation to its per-call defaults: Gallery, nothing pinned,
 * self-view visible.
 */
export function resetCallViewState(): void {
  $callViewMode.set("gallery");
  $callPinnedTileKey.set(null);
  $callSelfViewHidden.set(false);
}
