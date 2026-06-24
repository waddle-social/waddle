import { describe, expect, test } from "bun:test";

import { formatIdle } from "../src/presence/idle-duration";

const since = 1_000_000;

describe("formatIdle", () => {
  test("renders whole minutes", () => {
    expect(formatIdle(since, since + 20 * 60_000)).toBe("idle 20m");
  });

  test("rolls up to hours past 60 minutes", () => {
    expect(formatIdle(since, since + 2 * 3_600_000)).toBe("idle 2h");
  });

  test("rolls up to days past 24 hours", () => {
    expect(formatIdle(since, since + 3 * 86_400_000)).toBe("idle 3d");
  });

  test("sub-minute idle reads <1m", () => {
    expect(formatIdle(since, since + 30_000)).toBe("idle <1m");
  });

  test("a future 'since' (clock skew) clamps to <1m, never negative", () => {
    expect(formatIdle(since, since - 5_000)).toBe("idle <1m");
  });
});
