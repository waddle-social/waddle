/**
 * Compute the best `(cols, rows)` grid for `n` tiles in a container of
 * `(width, height)` such that every tile, sized to the cell and
 * constrained to `aspect`, has the largest possible visible area.
 *
 * This is the same algorithm LiveKit's reference `GridLayout` uses:
 * sweep every column count from 1 to `n`, compute the cell size each
 * implies, fit a tile of the target aspect ratio inside the cell, and
 * pick the column count whose total tile area is biggest.
 *
 * - Pure — no DOM access, easy to unit-test.
 * - `aspect` defaults to 16:9 (standard webcam framing).
 * - When `n === 0`, returns a degenerate 1×1 cell so callers don't
 *   have to special-case empty grids.
 * - The chosen grid may have an under-filled last row (e.g. 3 tiles
 *   in a 2×2). Callers can either accept that or pass through a
 *   "fill last row" tweak — we don't try to be clever here.
 */
type GridLayout = {
  cols: number;
  rows: number;
  /** Cell width in CSS pixels — what `grid-template-columns: repeat(cols, 1fr)`
   *  resolves to in a container of `width` minus inter-tile gaps. */
  cellWidth: number;
  /** Cell height in CSS pixels — symmetric with `cellWidth`. */
  cellHeight: number;
  /** Tile width after aspect-ratio fit inside the cell. */
  tileWidth: number;
  /** Tile height after aspect-ratio fit inside the cell. */
  tileHeight: number;
};

export const DEFAULT_TILE_ASPECT = 16 / 9;

export function bestGridLayout(
  n: number,
  width: number,
  height: number,
  aspect = DEFAULT_TILE_ASPECT,
): GridLayout {
  if (n <= 0 || width <= 0 || height <= 0 || aspect <= 0) {
    return {
      cols: 1,
      rows: 1,
      cellWidth: Math.max(0, width),
      cellHeight: Math.max(0, height),
      tileWidth: 0,
      tileHeight: 0,
    };
  }

  let best: GridLayout = {
    cols: 1,
    rows: n,
    cellWidth: width,
    cellHeight: height / n,
    tileWidth: 0,
    tileHeight: 0,
  };
  let bestArea = -1;

  for (let cols = 1; cols <= n; cols += 1) {
    const rows = Math.ceil(n / cols);
    const cellWidth = width / cols;
    const cellHeight = height / rows;
    // Fit a tile of the target aspect inside the cell.
    const tileWidth = Math.min(cellWidth, cellHeight * aspect);
    const tileHeight = Math.min(cellHeight, cellWidth / aspect);
    const area = tileWidth * tileHeight;
    if (area > bestArea) {
      bestArea = area;
      best = { cols, rows, cellWidth, cellHeight, tileWidth, tileHeight };
    }
  }

  return best;
}
