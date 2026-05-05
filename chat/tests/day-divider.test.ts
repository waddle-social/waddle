import { describe, expect, test } from "bun:test";
import { formatTimelineDayDivider, isSameTimelineDay } from "../src/channels/timeline";

function localIso(
  year: number,
  monthIndex: number,
  day: number,
  hour = 12,
  minute = 0,
): string {
  return new Date(year, monthIndex, day, hour, minute).toISOString();
}

describe("day divider labels", () => {
  test("labels today and yesterday by local calendar day", () => {
    const now = new Date(2026, 3, 27, 12);

    expect(formatTimelineDayDivider(localIso(2026, 3, 27, 0, 5), now)).toBe("Today");
    expect(formatTimelineDayDivider(localIso(2026, 3, 26, 23, 55), now)).toBe("Yesterday");
  });

  test("falls back to a dated divider for older messages", () => {
    const now = new Date(2026, 3, 27, 12);

    expect(formatTimelineDayDivider(localIso(2026, 3, 25), now)).toBe("Sat, Apr 25");
  });

  test("compares days by local calendar date", () => {
    expect(isSameTimelineDay(localIso(2026, 3, 27, 0, 5), localIso(2026, 3, 27, 23, 55))).toBe(true);
    expect(isSameTimelineDay(localIso(2026, 3, 26, 23, 55), localIso(2026, 3, 27, 0, 5))).toBe(false);
  });
});
