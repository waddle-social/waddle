// Unit tests for the client-side xCal instance expander. Covers
// weekly + BYDAY, monthly + BYMONTHDAY, COUNT vs UNTIL termination,
// EXDATE skipping (including no-op EXDATE on a non-existent
// occurrence), and RECURRENCE-ID override substitution.

import { describe, expect, test } from "bun:test";
import {
  expandInstances,
  groupEventsWithOverrides,
  type CalendarMaster,
} from "../src/lib/xmpp/event-expansion";
import type { CommunityEvent } from "../src/lib/xmpp/event-types";

const NOW = Date.parse("2026-06-01T00:00:00Z");
// 365 days from NOW — covers all the synthetic test events without
// piling up emissions beyond `maxCount`.
const HORIZON = NOW + 365 * 86_400_000;

function master(overrides: Partial<CommunityEvent> = {}): CommunityEvent {
  return {
    id: "evt-1",
    uid: "evt-1",
    summary: "Game Night",
    dtstartMs: Date.parse("2026-06-05T19:00:00Z"),
    dtendMs: Date.parse("2026-06-05T22:00:00Z"),
    ...overrides,
  };
}

describe("expandInstances", () => {
  test("non-recurring event returns itself when in the future", () => {
    const event = master();
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.length).toBe(1);
    expect(instances[0]?.id).toBe(event.id);
  });

  test("non-recurring event in the past is dropped", () => {
    const event = master({ dtstartMs: NOW - 86_400_000 });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.length).toBe(0);
  });

  test("weekly + BYDAY emits the right occurrences within COUNT", () => {
    const event = master({
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 4 },
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.length).toBe(4);
    expect(instances[0]?.dtstartMs).toBe(Date.parse("2026-06-05T19:00:00Z"));
    expect(instances[3]?.dtstartMs).toBe(Date.parse("2026-06-26T19:00:00Z"));
    // Per-occurrence emission carries DTEND adjusted by the master's duration.
    expect(instances[0]?.dtendMs).toBe(Date.parse("2026-06-05T22:00:00Z"));
    expect(instances[3]?.dtendMs).toBe(Date.parse("2026-06-26T22:00:00Z"));
  });

  test("UNTIL terminates the series even without COUNT", () => {
    const event = master({
      rrule: {
        freq: "WEEKLY",
        byDay: ["FR"],
        untilMs: Date.parse("2026-06-26T23:59:00Z"),
      },
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.length).toBe(4); // Jun 5, 12, 19, 26
  });

  test("EXDATE skips the matching occurrence", () => {
    const event = master({
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 4 },
      exdatesMs: [Date.parse("2026-06-19T19:00:00Z")],
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    const starts = instances.map((i) => i.dtstartMs);
    expect(starts).not.toContain(Date.parse("2026-06-19T19:00:00Z"));
    expect(starts).toContain(Date.parse("2026-06-26T19:00:00Z"));
    // 4 in count - 1 EXDATE = 3 emitted
    expect(instances.length).toBe(3);
  });

  test("EXDATE that doesn't match any occurrence is a no-op", () => {
    const event = master({
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 3 },
      exdatesMs: [Date.parse("2030-12-25T12:00:00Z")],
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.length).toBe(3);
  });

  test("RECURRENCE-ID override replaces a single occurrence", () => {
    const event = master({
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 4 },
    });
    const overrideRecurrence = Date.parse("2026-06-12T19:00:00Z");
    const override: CommunityEvent = {
      id: "evt-1::override::1",
      uid: "evt-1",
      summary: "Special: Halo",
      recurrenceIdMs: overrideRecurrence,
      dtstartMs: Date.parse("2026-06-12T20:00:00Z"),
    };
    const instances = expandInstances(
      { master: event, overrides: [override] },
      { nowMs: NOW, horizonMs: HORIZON },
    );
    const replaced = instances.find((i) => i.recurrenceIdMs === overrideRecurrence);
    expect(replaced).toBeDefined();
    expect(replaced?.summary).toBe("Special: Halo");
    expect(replaced?.dtstartMs).toBe(Date.parse("2026-06-12T20:00:00Z"));
    // Other 3 occurrences still come from the master.
    expect(instances.filter((i) => i.summary === "Game Night").length).toBe(3);
  });

  test("monthly + BYMONTHDAY emits day-of-month matches", () => {
    const event = master({
      dtstartMs: Date.parse("2026-06-01T18:00:00Z"),
      dtendMs: Date.parse("2026-06-01T20:00:00Z"),
      rrule: { freq: "MONTHLY", byMonthDay: [1, 15], count: 6 },
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    const days = instances.map((i) => new Date(i.dtstartMs ?? 0).getUTCDate());
    expect(days.every((d) => d === 1 || d === 15)).toBe(true);
    expect(instances.length).toBe(6);
  });

  test("maxCount caps the emitted list even when COUNT is larger", () => {
    const event = master({
      rrule: { freq: "DAILY", count: 1000 },
    });
    const instances = expandInstances(
      { master: event, overrides: [] },
      { nowMs: NOW, horizonMs: HORIZON, maxCount: 7 },
    );
    expect(instances.length).toBe(7);
  });
});

describe("groupEventsWithOverrides", () => {
  test("buckets overrides by UID and surfaces masters as CalendarMaster", () => {
    const items: CommunityEvent[] = [
      { id: "evt-a", uid: "evt-a", summary: "A", dtstartMs: NOW + 1000 },
      {
        id: "evt-a::override::1",
        uid: "evt-a",
        summary: "A-override",
        recurrenceIdMs: NOW + 2000,
      },
      { id: "evt-b", uid: "evt-b", summary: "B", dtstartMs: NOW + 3000 },
    ];
    const groups = groupEventsWithOverrides(items);
    expect(groups.length).toBe(2);
    const a = groups.find((g: CalendarMaster) => g.master.uid === "evt-a");
    const b = groups.find((g: CalendarMaster) => g.master.uid === "evt-b");
    expect(a?.overrides.length).toBe(1);
    expect(a?.overrides[0]?.summary).toBe("A-override");
    expect(b?.overrides.length).toBe(0);
  });
});
