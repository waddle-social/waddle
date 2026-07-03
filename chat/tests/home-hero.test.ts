import { describe, expect, test } from "bun:test";
import {
  heroGreetingFor,
  heroQuietMessageFor,
  heroSummaryPartsFor,
  heroTimeOfDayFor,
  type HeroSummary,
} from "../src/components/chat/home-hero";

function at(hour: number): Date {
  const d = new Date();
  d.setHours(hour, 0, 0, 0);
  return d;
}

describe("heroTimeOfDayFor", () => {
  test("buckets the local hour", () => {
    expect(heroTimeOfDayFor(at(5))).toBe("morning");
    expect(heroTimeOfDayFor(at(10))).toBe("morning");
    expect(heroTimeOfDayFor(at(11))).toBe("day");
    expect(heroTimeOfDayFor(at(16))).toBe("day");
    expect(heroTimeOfDayFor(at(17))).toBe("evening");
    expect(heroTimeOfDayFor(at(21))).toBe("evening");
    expect(heroTimeOfDayFor(at(22))).toBe("night");
    expect(heroTimeOfDayFor(at(4))).toBe("night");
  });

  test("greeting and quiet message cover every bucket", () => {
    for (const tod of ["morning", "day", "evening", "night"] as const) {
      expect(heroGreetingFor(tod).length).toBeGreaterThan(0);
      expect(heroQuietMessageFor(tod).length).toBeGreaterThan(0);
    }
    expect(heroGreetingFor("night")).toBe("Late one tonight.");
  });
});

describe("heroSummaryPartsFor", () => {
  function summary(overrides: Partial<HeroSummary> = {}): HeroSummary {
    return {
      totalUnread: 0,
      totalMentions: 0,
      totalThreadUnread: 0,
      dmUnread: 0,
      activeCalls: 0,
      onlineFriends: 0,
      hasUnread: false,
      ...overrides,
    };
  }

  test("empty summary produces no parts", () => {
    expect(heroSummaryPartsFor(summary())).toEqual([]);
  });

  test("merges channel and dm unread, pluralizes, and orders parts", () => {
    const parts = heroSummaryPartsFor(summary({
      totalMentions: 1,
      totalUnread: 2,
      dmUnread: 3,
      totalThreadUnread: 1,
      activeCalls: 2,
      onlineFriends: 1,
    }));
    expect(parts).toEqual([
      { count: 1, label: "mention" },
      { count: 5, label: "unread messages" },
      { count: 1, label: "thread reply" },
      { count: 2, label: "active calls" },
      { count: 1, label: "friend online" },
    ]);
  });
});
