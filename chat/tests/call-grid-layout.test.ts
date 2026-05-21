import { describe, expect, test } from "bun:test";
import { bestGridLayout } from "../src/lib/calls/grid-layout";

describe("bestGridLayout", () => {
  test("returns a 1x1 cell for zero participants without throwing", () => {
    const layout = bestGridLayout(0, 800, 600);
    expect(layout.cols).toBe(1);
    expect(layout.rows).toBe(1);
  });

  test("a single participant fills the container with one tile", () => {
    const layout = bestGridLayout(1, 800, 600);
    expect(layout.cols).toBe(1);
    expect(layout.rows).toBe(1);
  });

  test("a wide short container prefers more columns than rows", () => {
    // 1200x300 with 4 participants: 4x1 (cells 300x300, tile 300x169) beats
    // 2x2 (cells 600x150, tile 267x150) — pick 4x1.
    const layout = bestGridLayout(4, 1200, 300);
    expect(layout.cols).toBe(4);
    expect(layout.rows).toBe(1);
  });

  test("a tall narrow container prefers more rows than columns", () => {
    // 300x1200 with 4 participants: the symmetric case to above.
    const layout = bestGridLayout(4, 300, 1200);
    expect(layout.cols).toBe(1);
    expect(layout.rows).toBe(4);
  });

  test("a roughly square container prefers a square-ish grid", () => {
    // 800x800 with 4 participants: 2x2 wins on area.
    const layout = bestGridLayout(4, 800, 800);
    expect(layout.cols).toBe(2);
    expect(layout.rows).toBe(2);
  });

  test("grid grows when the container shrinks vertically (splitter drag)", () => {
    // Same 6 participants in a 1200-wide container: a tall container
    // prefers a 2x3 grid; a short container prefers a 6x1 grid.
    const tall = bestGridLayout(6, 1200, 800);
    const short = bestGridLayout(6, 1200, 250);
    expect(tall.rows).toBeGreaterThan(short.rows);
    expect(short.cols).toBeGreaterThanOrEqual(tall.cols);
  });

  test("9 participants in a square container picks the area-maximizing grid", () => {
    // Hand check for 16:9 tiles in a 900x900 container:
    //   1x9 → cell 900x100, tile 178x100, area = 17,800
    //   2x5 → cell 450x180, tile 320x180, area = 57,600  ← best
    //   3x3 → cell 300x300, tile 300x169, area = 50,700
    //   4x3 → cell 225x300, tile 225x127, area = 28,575
    // So the optimum is 2 columns × 5 rows. This looks "wrong" at
    // first glance (3x3 feels square-like), but for 16:9 tiles each
    // wide cell of 2x5 gives the largest fitted tile.
    const layout = bestGridLayout(9, 900, 900);
    expect(layout.cols).toBe(2);
    expect(layout.rows).toBe(5);
  });

  test("non-power participant counts pick a grid with at most one under-filled row", () => {
    // 5 participants: best is typically 3x2 (one empty cell) or 2x3 — the
    // algorithm picks whichever gives the larger tile area. For an 800x600
    // container, 3x2 cells = 267x300 → fit = 267x150 area=40050; 2x3 cells
    // = 400x200 → fit = 356x200 area=71200. 2x3 wins.
    const layout = bestGridLayout(5, 800, 600);
    expect(layout.cols * layout.rows).toBeGreaterThanOrEqual(5);
    expect(layout.cols).toBe(2);
    expect(layout.rows).toBe(3);
  });
});
