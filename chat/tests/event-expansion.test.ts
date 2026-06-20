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
import {
  dateTimeValue,
  dateValue,
  eventOverlapsRange,
  localDayRange,
} from "../src/lib/xmpp/event-calendar";
import type { CalendarDateValue, CommunityEvent } from "../src/lib/xmpp/event-types";

const NOW = Date.parse("2026-06-01T00:00:00Z");
// 365 days from NOW — covers all the synthetic test events without
// piling up emissions beyond `maxCount`.
const HORIZON = NOW + 365 * 86_400_000;

function master(overrides: Partial<CommunityEvent> = {}): CommunityEvent {
  return {
    id: "evt-1",
    uid: "evt-1",
    summary: "Game Night",
    dtstart: dateTimeValue(Date.parse("2026-06-05T19:00:00Z")),
    dtend: dateTimeValue(Date.parse("2026-06-05T22:00:00Z")),
    ...overrides,
  };
}

function ms(value: CalendarDateValue | undefined): number | undefined {
  return value?.kind === "date-time" ? value.ms : undefined;
}

function date(value: CalendarDateValue | undefined): string | undefined {
  return value?.kind === "date" ? value.date : undefined;
}

describe("expandInstances", () => {
  test("non-recurring event returns itself when in the future", () => {
    const event = master();
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.length).toBe(1);
    expect(instances[0]?.id).toBe(event.id);
  });

  test("non-recurring event in the past is dropped", () => {
    const event = master({
      dtstart: dateTimeValue(NOW - 86_400_000),
      dtend: dateTimeValue(NOW - 86_400_000 + 60_000),
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.length).toBe(0);
  });

  test("non-recurring event that started earlier but is ongoing is kept", () => {
    const event = master({
      dtstart: dateTimeValue(NOW - 60 * 60 * 1000),
      dtend: dateTimeValue(NOW + 60 * 60 * 1000),
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.length).toBe(1);
    expect(instances[0]?.id).toBe(event.id);
  });

  test("weekly + BYDAY emits the right occurrences within COUNT", () => {
    const event = master({
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 4 },
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.length).toBe(4);
    expect(ms(instances[0]?.dtstart)).toBe(Date.parse("2026-06-05T19:00:00Z"));
    expect(ms(instances[3]?.dtstart)).toBe(Date.parse("2026-06-26T19:00:00Z"));
    // Per-occurrence emission carries DTEND adjusted by the master's duration.
    expect(ms(instances[0]?.dtend)).toBe(Date.parse("2026-06-05T22:00:00Z"));
    expect(ms(instances[3]?.dtend)).toBe(Date.parse("2026-06-26T22:00:00Z"));
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
      exdates: [dateTimeValue(Date.parse("2026-06-19T19:00:00Z"))],
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    const starts = instances.map((i) => ms(i.dtstart));
    expect(starts).not.toContain(Date.parse("2026-06-19T19:00:00Z"));
    expect(starts).toContain(Date.parse("2026-06-26T19:00:00Z"));
    // 4 in count - 1 EXDATE = 3 emitted
    expect(instances.length).toBe(3);
  });

  test("EXDATE that doesn't match any occurrence is a no-op", () => {
    const event = master({
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 3 },
      exdates: [dateTimeValue(Date.parse("2030-12-25T12:00:00Z"))],
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
      recurrenceId: dateTimeValue(overrideRecurrence),
      dtstart: dateTimeValue(Date.parse("2026-06-12T20:00:00Z")),
    };
    const instances = expandInstances(
      { master: event, overrides: [override] },
      { nowMs: NOW, horizonMs: HORIZON },
    );
    const replaced = instances.find((i) => ms(i.recurrenceId) === overrideRecurrence);
    expect(replaced).toBeDefined();
    expect(replaced?.summary).toBe("Special: Halo");
    expect(ms(replaced?.dtstart)).toBe(Date.parse("2026-06-12T20:00:00Z"));
    // Other 3 occurrences still come from the master.
    expect(instances.filter((i) => i.summary === "Game Night").length).toBe(3);
  });

  test("RECURRENCE-ID override moved into the future is filtered by effective bounds", () => {
    const event = master({
      dtstart: dateTimeValue(Date.parse("2026-05-01T19:00:00Z")),
      dtend: dateTimeValue(Date.parse("2026-05-01T20:00:00Z")),
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 2 },
    });
    const override: CommunityEvent = {
      id: "evt-1::override::future",
      uid: "evt-1",
      summary: "Moved forward",
      recurrenceId: dateTimeValue(Date.parse("2026-05-08T19:00:00Z")),
      dtstart: dateTimeValue(Date.parse("2026-06-12T20:00:00Z")),
      dtend: dateTimeValue(Date.parse("2026-06-12T21:00:00Z")),
    };

    const instances = expandInstances(
      { master: event, overrides: [override] },
      { nowMs: NOW, horizonMs: HORIZON },
    );

    expect(instances.map((instance) => instance.summary)).toEqual(["Moved forward"]);
  });

  test("RECURRENCE-ID override moved into the past is dropped by effective bounds", () => {
    const event = master({
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 2 },
    });
    const override: CommunityEvent = {
      id: "evt-1::override::past",
      uid: "evt-1",
      summary: "Moved backward",
      recurrenceId: dateTimeValue(Date.parse("2026-06-05T19:00:00Z")),
      dtstart: dateTimeValue(Date.parse("2026-05-01T20:00:00Z")),
      dtend: dateTimeValue(Date.parse("2026-05-01T21:00:00Z")),
    };

    const instances = expandInstances(
      { master: event, overrides: [override] },
      { nowMs: NOW, horizonMs: HORIZON },
    );

    expect(instances.map((instance) => instance.summary)).toEqual(["Game Night"]);
    expect(ms(instances[0]?.dtstart)).toBe(Date.parse("2026-06-12T19:00:00Z"));
  });

  test("monthly + BYMONTHDAY emits day-of-month matches", () => {
    const event = master({
      dtstart: dateTimeValue(Date.parse("2026-06-01T18:00:00Z")),
      dtend: dateTimeValue(Date.parse("2026-06-01T20:00:00Z")),
      rrule: { freq: "MONTHLY", byMonthDay: [1, 15], count: 6 },
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    const days = instances.map((i) => new Date(ms(i.dtstart) ?? 0).getUTCDate());
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

  test("all-day recurrence uses typed DATE DTSTART and DATE EXDATE", () => {
    const event = master({
      dtstart: dateValue("2026-06-05"),
      dtend: dateValue("2026-06-06"),
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 4 },
      exdates: [dateValue("2026-06-19")],
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(instances.map((instance) => date(instance.dtstart))).toEqual([
      "2026-06-05",
      "2026-06-12",
      "2026-06-26",
    ]);
    expect(instances.every((instance) => instance.dtstart?.kind === "date")).toBe(true);
  });

  test("all-day multi-day duration is inherited by recurring instances", () => {
    const event = master({
      dtstart: dateValue("2026-06-05"),
      dtend: dateValue("2026-06-08"),
      rrule: { freq: "WEEKLY", byDay: ["FR"], count: 2 },
    });
    const instances = expandInstances({ master: event, overrides: [] }, { nowMs: NOW, horizonMs: HORIZON });
    expect(date(instances[0]?.dtstart)).toBe("2026-06-05");
    expect(date(instances[0]?.dtend)).toBe("2026-06-08");
    expect(date(instances[1]?.dtstart)).toBe("2026-06-12");
    expect(date(instances[1]?.dtend)).toBe("2026-06-15");
  });

  test("day overlap helper includes every day touched by a multi-day event", () => {
    const event = master({
      dtstart: dateValue("2026-06-05"),
      dtend: dateValue("2026-06-08"),
    });
    const june5 = localDayRange(2026, 5, 5);
    const june6 = localDayRange(2026, 5, 6);
    const june7 = localDayRange(2026, 5, 7);
    const june8 = localDayRange(2026, 5, 8);
    expect(eventOverlapsRange(event, june5.startMs, june5.endMs)).toBe(true);
    expect(eventOverlapsRange(event, june6.startMs, june6.endMs)).toBe(true);
    expect(eventOverlapsRange(event, june7.startMs, june7.endMs)).toBe(true);
    expect(eventOverlapsRange(event, june8.startMs, june8.endMs)).toBe(false);
  });
});

describe("groupEventsWithOverrides", () => {
  test("buckets overrides by UID and surfaces masters as CalendarMaster", () => {
    const items: CommunityEvent[] = [
      { id: "evt-a", uid: "evt-a", summary: "A", dtstart: dateTimeValue(NOW + 1000) },
      {
        id: "evt-a::override::1",
        uid: "evt-a",
        summary: "A-override",
        recurrenceId: dateTimeValue(NOW + 2000),
      },
      { id: "evt-b", uid: "evt-b", summary: "B", dtstart: dateTimeValue(NOW + 3000) },
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
