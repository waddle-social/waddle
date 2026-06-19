/**
 * Client-side instance expansion for recurring xCal events.
 *
 * Given a master `CommunityEvent` with an RRULE plus zero or more
 * RECURRENCE-ID overrides and EXDATE cancellations, produces a flat
 * list of upcoming `CommunityEvent` instances. The expander is
 * deliberately conservative: it walks DTSTART forward by FREQ +
 * INTERVAL with BYDAY / BYMONTHDAY filters, terminating at COUNT or
 * UNTIL (or a caller-supplied `horizonMs` / `maxCount` cap).
 *
 * For each candidate occurrence:
 *   - skipped if its DTSTART matches any value in the master's
 *     `exdates`;
 *   - replaced by the override's properties (summary, location,
 *     dtstart, dtend, description) when an override's
 *     `recurrenceId` matches the occurrence.
 *
 * Non-recurring events pass through unchanged as a single instance.
 */

import {
  addDaysToDateString,
  calendarDateKey,
  calendarDateStartMs,
  dateTimeValue,
  eventBounds,
} from "./event-calendar";
import type { CalendarDateValue, CommunityEvent, Weekday } from "./event-types";

const DAY_MS = 86_400_000;
type CalendarDateOnly = Extract<CalendarDateValue, { kind: "date" }>;

const WEEKDAY_INDEX: Record<Weekday, number> = {
  SU: 0,
  MO: 1,
  TU: 2,
  WE: 3,
  TH: 4,
  FR: 5,
  SA: 6,
};

export interface CalendarMaster {
  master: CommunityEvent;
  overrides: CommunityEvent[];
}

interface ExpandInstancesOptions {
  /**
   * Upper bound on the latest DTSTART the expander will emit.
   * Defaults to one year from `nowMs`.
   */
  horizonMs?: number;
  /** Hard cap on emitted instances. Defaults to 50. */
  maxCount?: number;
  /** Reference "now" for filtering past occurrences. Defaults to Date.now(). */
  nowMs?: number;
}

/**
 * Expand a CalendarMaster into upcoming `CommunityEvent` instances.
 * Occurrences whose effective end is not after `nowMs` are dropped,
 * so ongoing spanning events are retained. The returned list is
 * sorted ascending by DTSTART.
 */
export function expandInstances(
  group: CalendarMaster,
  options: ExpandInstancesOptions = {},
): CommunityEvent[] {
  const { master, overrides } = group;
  const nowMs = options.nowMs ?? Date.now();
  const horizonMs = options.horizonMs ?? nowMs + 365 * DAY_MS;
  const maxCount = options.maxCount ?? 50;

  const dtstart = master.dtstart;
  if (!dtstart) {
    // No timeline anchor — surface the master as-is so it still renders.
    return [{ ...master }];
  }
  if (!master.rrule) {
    if ((eventBounds(master)?.endMs ?? 0) <= nowMs) return [];
    return [{ ...master }];
  }

  const exdateSet = new Set((master.exdates ?? []).map(calendarDateKey));
  const overrideByDtstart = new Map<string, CommunityEvent>();
  for (const ov of overrides) {
    if (ov.recurrenceId) {
      overrideByDtstart.set(calendarDateKey(ov.recurrenceId), ov);
    }
  }

  const out: CommunityEvent[] = [];
  let cursor = dtstart;
  let emitted = 0;
  let candidates = 0;
  const rrule = master.rrule;
  const interval = rrule.interval ?? 1;
  const until = rrule.untilMs ?? Infinity;
  const countCap = rrule.count ?? Infinity;
  // Walk-cap is independent of `maxCount`; protects against pathological
  // BYDAY/BYMONTHDAY combos that emit no candidates per step.
  const walkCap = 3_650;
  let walked = 0;

  while (
    calendarDateStartMs(cursor) <= horizonMs &&
    calendarDateStartMs(cursor) <= until &&
    candidates < countCap &&
    emitted < maxCount &&
    walked < walkCap
  ) {
    walked += 1;
    if (matchesByDay(cursor, rrule.byDay) && matchesByMonthDay(cursor, rrule.byMonthDay)) {
      candidates += 1;
      const cursorKey = calendarDateKey(cursor);
      const skip = exdateSet.has(cursorKey);
      if (!skip) {
        const override = overrideByDtstart.get(cursorKey);
        const instance = override
          ? applyOverride(master, override, cursor)
          : occurrenceFromMaster(master, cursor);
        if ((eventBounds(instance)?.endMs ?? 0) > nowMs) {
          out.push(instance);
          emitted += 1;
        }
      }
    }
    cursor = advance(cursor, rrule.freq, interval);
  }
  return out;
}

/**
 * Group a flat item list into master / overrides keyed by UID. RSVP
 * items (item id contains `-rsvp-`) are intentionally NOT included
 * here — they are merged into the master's `attendees` upstream by
 * `groupEventsWithRsvps`.
 */
export function groupEventsWithOverrides(
  items: readonly CommunityEvent[],
): CalendarMaster[] {
  const masters = new Map<string, CommunityEvent>();
  const overrides = new Map<string, CommunityEvent[]>();
  for (const item of items) {
    if (item.recurrenceId) {
      const bucket = overrides.get(item.uid) ?? [];
      bucket.push(item);
      overrides.set(item.uid, bucket);
    } else {
      masters.set(item.uid, item);
    }
  }
  const out: CalendarMaster[] = [];
  for (const [uid, master] of masters) {
    out.push({ master, overrides: overrides.get(uid) ?? [] });
  }
  return out;
}

function matchesByDay(value: CalendarDateValue, byDay: readonly Weekday[] | undefined): boolean {
  if (!byDay || byDay.length === 0) return true;
  const dow = value.kind === "date"
    ? dateStringUtcDate(value.date).getUTCDay()
    : new Date(value.ms).getUTCDay();
  return byDay.some((wd) => WEEKDAY_INDEX[wd] === dow);
}

function matchesByMonthDay(value: CalendarDateValue, byMonthDay: readonly number[] | undefined): boolean {
  if (!byMonthDay || byMonthDay.length === 0) return true;
  const dom = value.kind === "date"
    ? dateStringUtcDate(value.date).getUTCDate()
    : new Date(value.ms).getUTCDate();
  return byMonthDay.includes(dom);
}

function advance(value: CalendarDateValue, freq: string, interval: number): CalendarDateValue {
  if (value.kind === "date") {
    return advanceDate(value, freq, interval);
  }
  const d = new Date(value.ms);
  switch (freq) {
    case "DAILY":
      d.setUTCDate(d.getUTCDate() + interval);
      return dateTimeValue(d.getTime());
    case "WEEKLY":
      // Walk by single days; BYDAY filtering picks the right ones.
      // For multi-week intervals we still walk daily and let the
      // BYDAY filter + interval-week check handle it.
      d.setUTCDate(d.getUTCDate() + (interval === 1 ? 1 : 7 * interval));
      return dateTimeValue(d.getTime());
    case "MONTHLY":
      d.setUTCMonth(d.getUTCMonth() + interval);
      return dateTimeValue(d.getTime());
    case "YEARLY":
      d.setUTCFullYear(d.getUTCFullYear() + interval);
      return dateTimeValue(d.getTime());
    default:
      // Unknown frequency — bail to "1 day" so the loop still
      // terminates against horizonMs / walkCap.
      d.setUTCDate(d.getUTCDate() + 1);
      return dateTimeValue(d.getTime());
  }
}

function advanceDate(value: CalendarDateOnly, freq: string, interval: number): CalendarDateValue {
  switch (freq) {
    case "DAILY":
      return { kind: "date", date: addDaysToDateString(value.date, interval) };
    case "WEEKLY":
      return { kind: "date", date: addDaysToDateString(value.date, interval === 1 ? 1 : 7 * interval) };
    case "MONTHLY": {
      const d = dateStringUtcDate(value.date);
      d.setUTCMonth(d.getUTCMonth() + interval);
      return { kind: "date", date: formatUtcDateString(d) };
    }
    case "YEARLY": {
      const d = dateStringUtcDate(value.date);
      d.setUTCFullYear(d.getUTCFullYear() + interval);
      return { kind: "date", date: formatUtcDateString(d) };
    }
    default:
      return { kind: "date", date: addDaysToDateString(value.date, 1) };
  }
}

function occurrenceFromMaster(master: CommunityEvent, dtstart: CalendarDateValue): CommunityEvent {
  const duration = masterDuration(master);
  const dtend = duration ? addDuration(dtstart, duration) : undefined;
  return {
    ...master,
    id: `${master.id}::${calendarDateKey(dtstart)}`,
    dtstart,
    ...(dtend ? { dtend } : {}),
    // Per-occurrence instances inherit the master's series — drop
    // RRULE so the UI shows "single occurrence" semantics.
    rrule: undefined,
  };
}

function applyOverride(
  master: CommunityEvent,
  override: CommunityEvent,
  recurrenceId: CalendarDateValue,
): CommunityEvent {
  const dtstart = override.dtstart ?? recurrenceId;
  const inheritedDuration = masterDuration(master);
  const inheritedEnd = inheritedDuration ? addDuration(dtstart, inheritedDuration) : undefined;
  return {
    ...master,
    id: `${master.id}::${calendarDateKey(recurrenceId)}::override`,
    summary: override.summary || master.summary,
    ...(override.description !== undefined ? { description: override.description } : {}),
    ...(override.location !== undefined ? { location: override.location } : {}),
    dtstart,
    ...(override.dtend ? { dtend: override.dtend } : inheritedEnd ? { dtend: inheritedEnd } : {}),
    recurrenceId,
    rrule: undefined,
  };
}

type MasterDuration =
  | { kind: "date-time"; ms: number }
  | { kind: "date"; days: number };

function masterDuration(master: CommunityEvent): MasterDuration | undefined {
  if (!master.dtstart || !master.dtend || master.dtstart.kind !== master.dtend.kind) {
    return undefined;
  }
  if (master.dtstart.kind === "date-time" && master.dtend.kind === "date-time") {
    const durationMs = master.dtend.ms - master.dtstart.ms;
    return Number.isFinite(durationMs) && durationMs >= 0
      ? { kind: "date-time", ms: durationMs }
      : undefined;
  }
  if (master.dtstart.kind === "date" && master.dtend.kind === "date") {
    const durationDays = Math.round(
      (calendarDateStartMs(master.dtend) - calendarDateStartMs(master.dtstart)) / DAY_MS,
    );
    return Number.isFinite(durationDays) && durationDays >= 0
      ? { kind: "date", days: durationDays }
      : undefined;
  }
  return undefined;
}

function addDuration(start: CalendarDateValue, duration: MasterDuration): CalendarDateValue | undefined {
  if (start.kind === "date-time" && duration.kind === "date-time") {
    return dateTimeValue(start.ms + duration.ms);
  }
  if (start.kind === "date" && duration.kind === "date") {
    return { kind: "date", date: addDaysToDateString(start.date, duration.days) };
  }
  return undefined;
}

function dateStringUtcDate(date: string): Date {
  const [year, month, day] = date.split("-").map(Number);
  return new Date(Date.UTC(year ?? 0, (month ?? 1) - 1, day ?? 1));
}

function formatUtcDateString(date: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}`;
}
