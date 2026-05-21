import { describe, expect, test } from "bun:test";
import { selectGridLayout } from "../src/lib/calls/grid-layout";

describe("selectGridLayout", () => {
  test("1 tile in any container fits a 1x1 layout", () => {
    expect(selectGridLayout(1, 1200, 800)).toEqual({ cols: 1, rows: 1, maxTiles: 1 });
    expect(selectGridLayout(1, 320, 200)).toEqual({ cols: 1, rows: 1, maxTiles: 1 });
  });

  test("2 tiles in a wide container pick the landscape 2x1", () => {
    const layout = selectGridLayout(2, 1200, 600);
    expect(layout).toEqual({ cols: 2, rows: 1, maxTiles: 2 });
  });

  test("2 tiles in a tall container pick the portrait 1x2", () => {
    const layout = selectGridLayout(2, 400, 900);
    expect(layout).toEqual({ cols: 1, rows: 2, maxTiles: 2 });
  });

  test("4 tiles in a comfortable container fit a 2x2", () => {
    const layout = selectGridLayout(4, 1000, 700);
    expect(layout).toEqual({ cols: 2, rows: 2, maxTiles: 4 });
  });

  test("6 tiles in a wide-ish container pick the landscape 3x2", () => {
    const layout = selectGridLayout(6, 1100, 500);
    expect(layout.cols).toBe(3);
    expect(layout.rows).toBe(2);
  });

  test("9 tiles in a roomy container fit a 3x3", () => {
    const layout = selectGridLayout(9, 1100, 800);
    expect(layout).toEqual({ cols: 3, rows: 3, maxTiles: 9 });
  });

  test("25 tiles in a large container fit a 5x5", () => {
    const layout = selectGridLayout(25, 1600, 1100);
    expect(layout).toEqual({ cols: 5, rows: 5, maxTiles: 25 });
  });

  test("more tiles than the largest layout supports clip to maxTiles", () => {
    // 30 participants, big container: largest layout is 5×5 = 25.
    const layout = selectGridLayout(30, 1600, 1100);
    expect(layout.maxTiles).toBe(25);
    expect(layout.cols).toBe(5);
    expect(layout.rows).toBe(5);
  });

  test("a shrunk container falls back to a smaller layout that fits", () => {
    // 9 participants, but only 600x400 — 3x3 needs >= 720x540, so we
    // walk down to a 2x2 (480x360 floor).
    const layout = selectGridLayout(9, 600, 400);
    expect(layout.cols).toBe(2);
    expect(layout.rows).toBe(2);
    expect(layout.maxTiles).toBe(4);
  });

  test("a tiny container collapses to 1x1 even with many participants", () => {
    const layout = selectGridLayout(8, 240, 180);
    expect(layout).toEqual({ cols: 1, rows: 1, maxTiles: 1 });
  });

  test("zero or negative participants are normalized to 1 tile", () => {
    expect(selectGridLayout(0, 800, 600).maxTiles).toBe(1);
    expect(selectGridLayout(-5, 800, 600).maxTiles).toBe(1);
  });

  test("the same participant count reflows when the container shrinks vertically", () => {
    // Same 6 participants, splitter drag from tall → short.
    const tall = selectGridLayout(6, 1100, 800);
    const short = selectGridLayout(6, 1100, 380);
    // Tall fits a 3x2 or 3x3; short can no longer fit 3-row layouts,
    // so it must pick something with fewer rows.
    expect(short.rows).toBeLessThanOrEqual(tall.rows);
  });
});
