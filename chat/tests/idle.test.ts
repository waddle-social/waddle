import { describe, expect, test } from "bun:test";

import { computeAutoAway } from "../src/presence/idle";

const AWAY_MS = 10 * 60_000;
const XA_MS = 30 * 60_000;
const thresholds = { awayMs: AWAY_MS, xaMs: XA_MS };

describe("computeAutoAway", () => {
  test("recent interaction stays Available with no idle stamp", () => {
    const lastActive = 1_000_000;
    const now = lastActive + 60_000; // 1 min later
    expect(computeAutoAway({ now, lastActive, ...thresholds })).toEqual({
      show: "available",
      idleSince: null,
    });
  });

  test("crossing the Away threshold broadcasts away", () => {
    const lastActive = 1_000_000;
    const now = lastActive + AWAY_MS + 1;
    expect(computeAutoAway({ now, lastActive, ...thresholds }).show).toBe("away");
  });

  test("the idle stamp is when interaction stopped, not now", () => {
    const lastActive = 1_000_000;
    const now = lastActive + AWAY_MS + 5 * 60_000;
    // XEP-0319 `since` must be lastActive, never `now` — contacts render the
    // true idle age (acceptance criterion: timestamp is when interaction stopped).
    expect(computeAutoAway({ now, lastActive, ...thresholds }).idleSince).toBe(lastActive);
  });

  test("Away triggers exactly at the threshold (inclusive)", () => {
    const lastActive = 1_000_000;
    expect(computeAutoAway({ now: lastActive + AWAY_MS, lastActive, ...thresholds }).show).toBe(
      "away",
    );
  });

  test("one ms before the Away threshold is still Available", () => {
    const lastActive = 1_000_000;
    expect(
      computeAutoAway({ now: lastActive + AWAY_MS - 1, lastActive, ...thresholds }).show,
    ).toBe("available");
  });

  test("escalates to Extended Away past the xa threshold", () => {
    const lastActive = 1_000_000;
    const now = lastActive + XA_MS + 1;
    expect(computeAutoAway({ now, lastActive, ...thresholds })).toEqual({
      show: "xa",
      idleSince: lastActive,
    });
  });

  test("between the two thresholds stays Away, not yet Extended Away", () => {
    const lastActive = 1_000_000;
    const now = lastActive + XA_MS - 1;
    expect(computeAutoAway({ now, lastActive, ...thresholds }).show).toBe("away");
  });
});
