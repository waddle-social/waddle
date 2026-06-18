import type { CallTileModel } from "./call-tiles";

export type StageOverflow = {
  /** Distinct participants who have no tile on the Stage. */
  hiddenCount: number;
};

export type StagePartition = {
  /** Tiles to render in the equal grid, in order. */
  tiles: CallTileModel[];
  /** The "+N more" Overflow affordance, or `null` when every tile fits. */
  overflow: StageOverflow | null;
};

/**
 * Partition the full Stage tile list into the tiles shown in the grid plus an
 * optional "+N more" Overflow tile.
 */
export function partitionStageTiles(
  tiles: readonly CallTileModel[],
  capacity: number,
): StagePartition {
  const cap = Math.max(1, Math.floor(capacity));
  if (tiles.length <= cap) return { tiles: [...tiles], overflow: null };

  if (cap <= 1) {
    // A 1-cell Stage has no room for the overflow tile; keep the single tile
    // rather than render an all-button, no-video Stage. Everyone else stays
    // reachable via the roster/participant count.
    return { tiles: tiles.slice(0, 1), overflow: null };
  }

  // The overflow tile takes the last cell, so the grid shows `cap - 1` real
  // tiles. The count is of *people* with no tile on the Stage, so a participant
  // whose camera is shown but whose extra (e.g. screen-share) tile spilled over
  // is not double-counted.
  const visible = tiles.slice(0, cap - 1);
  const visibleIdentities = new Set(visible.map((tile) => tile.identity));
  const hiddenIdentities = new Set<string>();
  for (const tile of tiles.slice(cap - 1)) {
    if (!visibleIdentities.has(tile.identity)) hiddenIdentities.add(tile.identity);
  }
  if (hiddenIdentities.size === 0) {
    // Everyone past the cut is already on the Stage (only their extra tiles
    // spilled over). Fill the last cell with a real tile rather than show a
    // meaningless "+0 more".
    return { tiles: tiles.slice(0, cap), overflow: null };
  }
  return { tiles: visible, overflow: { hiddenCount: hiddenIdentities.size } };
}
