/**
 * Tile-grid layout picker for the in-call participant grid.
 *
 * Mirrors LiveKit's reference `GridLayout` algorithm
 * (https://github.com/livekit/components-js — `packages/core/src/
 * helper/grid-layouts.ts`): a small ordered table of `(cols, rows,
 * minWidth?, minHeight?, orientation?)` definitions; pick the smallest
 * entry whose tile capacity `cols*rows >= n` that the container can
 * accommodate. If the container is too small, walk down the table.
 *
 * Why a table, not "sweep cols 1..N and maximize area" (Jitsi's
 * pattern): the table is deterministic, easy to tune by eye, and
 * automatically handles the "1-tile self-view shouldn't become a
 * billboard" case via the `min*` floors. The sweep variant tends to
 * pick wide-aspect layouts (e.g. 2×5 for 9 tiles in a square) that
 * look mathematically optimal but feel unbalanced.
 *
 * The result also exposes `maxTiles` so callers can paginate or
 * scroll when the participant count exceeds the largest layout's
 * capacity (currently 25 = 5×5).
 */

type GridLayoutDef = {
  cols: number;
  rows: number;
  /** Container must be at least this wide (CSS px) to use this layout. */
  minWidth?: number;
  /** Container must be at least this tall (CSS px) to use this layout. */
  minHeight?: number;
  /** Optional bias: prefer this entry when the container's own
   *  orientation matches. */
  orientation?: "landscape" | "portrait";
};

/**
 * The size floors here are tuned for the chat call surface, where
 * the splitter can shrink the call region vertically. They keep a
 * single tile readable (>= 200 px tall, >= ~355 px wide) before the
 * algorithm falls back to a smaller layout.
 */
const GRID_LAYOUTS: readonly GridLayoutDef[] = [
  { cols: 1, rows: 1 },
  { cols: 1, rows: 2, orientation: "portrait", minHeight: 360 },
  { cols: 2, rows: 1, orientation: "landscape", minWidth: 360 },
  { cols: 2, rows: 2, minWidth: 480, minHeight: 360 },
  { cols: 3, rows: 2, minWidth: 720, minHeight: 360, orientation: "landscape" },
  { cols: 3, rows: 3, minWidth: 720, minHeight: 540 },
  { cols: 4, rows: 3, minWidth: 960, minHeight: 540, orientation: "landscape" },
  { cols: 4, rows: 4, minWidth: 960, minHeight: 720 },
  { cols: 5, rows: 5, minWidth: 1200, minHeight: 900 },
];

type SelectedGridLayout = {
  cols: number;
  rows: number;
  /** `cols * rows`. When `n > maxTiles`, the caller should paginate
   *  or clip — the grid itself only renders `maxTiles` cells. */
  maxTiles: number;
};

function layoutFits(
  layout: GridLayoutDef,
  width: number,
  height: number,
): boolean {
  if (layout.minWidth !== undefined && width < layout.minWidth) return false;
  if (layout.minHeight !== undefined && height < layout.minHeight) return false;
  return true;
}

function preferredFor(
  orientation: "landscape" | "portrait",
  layout: GridLayoutDef,
): number {
  // 0 = best, 1 = neutral, 2 = wrong orientation.
  if (!layout.orientation) return 1;
  return layout.orientation === orientation ? 0 : 2;
}

/**
 * Pick the (cols, rows) for `n` tiles in a `(width, height)` container.
 *
 * The algorithm:
 *  1. Find layouts whose capacity `cols*rows >= n`. Sort by capacity
 *     ascending (so "just enough" wins over "much bigger"), with
 *     orientation match as a tiebreak when two layouts have the same
 *     capacity. Walk the sorted list; return the first whose `min*`
 *     floors fit the container.
 *  2. If nothing whose capacity covers `n` fits, fall back to the
 *     largest layout that DOES fit — overflow is clipped / paginated.
 *     Orientation is no longer a preference here: the user is in a
 *     small container and we want as many tiles visible as possible.
 *  3. Last resort: 1×1.
 */
export function selectGridLayout(
  n: number,
  width: number,
  height: number,
): SelectedGridLayout {
  const safeN = Math.max(1, Math.floor(n));
  const safeWidth = Math.max(0, width);
  const safeHeight = Math.max(0, height);
  const orientation: "landscape" | "portrait" =
    safeWidth >= safeHeight ? "landscape" : "portrait";

  const fitsAll = (l: GridLayoutDef) => layoutFits(l, safeWidth, safeHeight);

  // Phase 1 — pick the smallest layout that covers `n` and fits.
  const enoughCapacity = GRID_LAYOUTS.filter((l) => l.cols * l.rows >= safeN)
    .slice()
    .sort((a, b) => {
      const capDelta = a.cols * a.rows - b.cols * b.rows;
      if (capDelta !== 0) return capDelta;
      return preferredFor(orientation, a) - preferredFor(orientation, b);
    });
  for (const layout of enoughCapacity) {
    if (fitsAll(layout)) {
      return {
        cols: layout.cols,
        rows: layout.rows,
        maxTiles: layout.cols * layout.rows,
      };
    }
  }

  // Phase 2 — degraded mode: nothing covers `n`. Pick the LARGEST
  // layout that still fits the container, so we paint as many tiles
  // as possible (overflow is clipped). Capacity wins; orientation is
  // ignored because we're already compromising.
  const fallback = GRID_LAYOUTS.slice().sort(
    (a, b) => b.cols * b.rows - a.cols * a.rows,
  );
  for (const layout of fallback) {
    if (fitsAll(layout)) {
      return {
        cols: layout.cols,
        rows: layout.rows,
        maxTiles: layout.cols * layout.rows,
      };
    }
  }

  return { cols: 1, rows: 1, maxTiles: 1 };
}
