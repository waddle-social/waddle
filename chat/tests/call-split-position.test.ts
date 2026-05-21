import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  $callSplitPositions,
  SPLIT_DEFAULT_PERCENT,
  SPLIT_MAX_PERCENT,
  SPLIT_MIN_PERCENT,
  getSplitPercent,
  resetSplitPositionsForTests,
  setSplitPercent,
} from "../src/lib/calls/split-position";

const ROOM = "design@muc.waddle.test";

describe("call-split positions", () => {
  beforeEach(() => {
    resetSplitPositionsForTests();
  });

  afterEach(() => {
    resetSplitPositionsForTests();
  });

  test("getSplitPercent returns the default when no value is stored", () => {
    expect(getSplitPercent(ROOM)).toBe(SPLIT_DEFAULT_PERCENT);
  });

  test("setSplitPercent stores per-room and reads back via getSplitPercent", () => {
    setSplitPercent(ROOM, 40);
    expect(getSplitPercent(ROOM)).toBe(40);
    expect(getSplitPercent("other@muc.waddle.test")).toBe(SPLIT_DEFAULT_PERCENT);
  });

  test("clamps below the minimum", () => {
    setSplitPercent(ROOM, 5);
    expect(getSplitPercent(ROOM)).toBe(SPLIT_MIN_PERCENT);
  });

  test("clamps above the maximum", () => {
    setSplitPercent(ROOM, 95);
    expect(getSplitPercent(ROOM)).toBe(SPLIT_MAX_PERCENT);
  });

  test("treats non-finite values as the default", () => {
    setSplitPercent(ROOM, Number.NaN);
    expect(getSplitPercent(ROOM)).toBe(SPLIT_DEFAULT_PERCENT);
  });

  test("normalizes the room JID key so resource-qualified JIDs hit the same slot", () => {
    setSplitPercent(`${ROOM}/alice`, 35);
    expect(getSplitPercent(ROOM)).toBe(35);
  });

  test("ignores empty room JIDs without throwing", () => {
    setSplitPercent("", 60);
    expect($callSplitPositions.get()).toEqual({});
  });
});
