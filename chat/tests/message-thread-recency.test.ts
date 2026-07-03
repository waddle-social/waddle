import { describe, expect, test } from "bun:test";
import { formatThreadRecency } from "../src/components/chat/message-thread-recency";

function isoAgo(ms: number): string {
  return new Date(Date.now() - ms).toISOString();
}

describe("formatThreadRecency", () => {
  test("empty for missing, invalid, or future timestamps", () => {
    expect(formatThreadRecency(undefined)).toBe("");
    expect(formatThreadRecency("not-a-date")).toBe("");
    expect(formatThreadRecency(isoAgo(-60_000))).toBe("");
  });

  test("buckets recent activity", () => {
    expect(formatThreadRecency(isoAgo(10_000))).toBe("just now");
    expect(formatThreadRecency(isoAgo(90_000))).toBe("1 min ago");
    expect(formatThreadRecency(isoAgo(5 * 60_000))).toBe("5 min ago");
    expect(formatThreadRecency(isoAgo(90 * 60_000))).toBe("1 hour ago");
    expect(formatThreadRecency(isoAgo(5 * 3_600_000))).toBe("5 hours ago");
    expect(formatThreadRecency(isoAgo(30 * 3_600_000))).toBe("1 day ago");
    expect(formatThreadRecency(isoAgo(3 * 86_400_000))).toBe("3 days ago");
  });

  test("falls back to a short date after a week", () => {
    const iso = isoAgo(10 * 86_400_000);
    const expected = new Date(iso).toLocaleDateString(undefined, { month: "short", day: "numeric" });
    expect(formatThreadRecency(iso)).toBe(expected);
  });
});
