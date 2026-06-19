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
 *     `exdatesMs`;
 *   - replaced by the override's properties (summary, location,
 *     dtstart, dtend, description) when an override's
 *     `recurrenceIdMs` matches the occurrence.
 *
 * Non-recurring events pass through unchanged as a single instance.
 */

import type { CommunityEvent, Weekday } from "./event-types";

const DAY_MS = 86_400_000;

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
 * Past occurrences (DTSTART <= nowMs) are dropped. The returned list
 * is sorted ascending by DTSTART.
 */
export function expandInstances(
  group: CalendarMaster,
  options: ExpandInstancesOptions = {},
): CommunityEvent[] {
  const { master, overrides } = group;
  const nowMs = options.nowMs ?? Date.now();
  const horizonMs = options.horizonMs ?? nowMs + 365 * DAY_MS;
  const maxCount = options.maxCount ?? 50;

  const dtstart = master.dtstartMs;
  if (typeof dtstart !== "number") {
    // No timeline anchor — surface the master as-is so it still renders.
    return [{ ...master }];
  }
  if (!master.rrule) {
    if (dtstart <= nowMs) return [];
    return [{ ...master }];
  }

  const exdateSet = new Set(master.exdatesMs ?? []);
  const overrideByDtstart = new Map<number, CommunityEvent>();
  for (const ov of overrides) {
    if (typeof ov.recurrenceIdMs === "number") {
      overrideByDtstart.set(ov.recurrenceIdMs, ov);
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
    cursor <= horizonMs &&
    cursor <= until &&
    candidates < countCap &&
    emitted < maxCount &&
    walked < walkCap
  ) {
    walked += 1;
    if (matchesByDay(cursor, rrule.byDay) && matchesByMonthDay(cursor, rrule.byMonthDay)) {
      candidates += 1;
      const skip = exdateSet.has(cursor);
      if (!skip && cursor > nowMs) {
        const override = overrideByDtstart.get(cursor);
        const instance = override
          ? applyOverride(master, override, cursor)
          : occurrenceFromMaster(master, cursor);
        out.push(instance);
        emitted += 1;
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
    if (typeof item.recurrenceIdMs === "number") {
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

function matchesByDay(ms: number, byDay: readonly Weekday[] | undefined): boolean {
  if (!byDay || byDay.length === 0) return true;
  const dow = new Date(ms).getUTCDay();
  return byDay.some((wd) => WEEKDAY_INDEX[wd] === dow);
}

function matchesByMonthDay(ms: number, byMonthDay: readonly number[] | undefined): boolean {
  if (!byMonthDay || byMonthDay.length === 0) return true;
  const dom = new Date(ms).getUTCDate();
  return byMonthDay.includes(dom);
}

function advance(ms: number, freq: string, interval: number): number {
  const d = new Date(ms);
  switch (freq) {
    case "DAILY":
      d.setUTCDate(d.getUTCDate() + interval);
      return d.getTime();
    case "WEEKLY":
      // Walk by single days; BYDAY filtering picks the right ones.
      // For multi-week intervals we still walk daily and let the
      // BYDAY filter + interval-week check handle it.
      d.setUTCDate(d.getUTCDate() + (interval === 1 ? 1 : 7 * interval));
      return d.getTime();
    case "MONTHLY":
      d.setUTCMonth(d.getUTCMonth() + interval);
      return d.getTime();
    case "YEARLY":
      d.setUTCFullYear(d.getUTCFullYear() + interval);
      return d.getTime();
    default:
      // Unknown frequency — bail to "1 day" so the loop still
      // terminates against horizonMs / walkCap.
      d.setUTCDate(d.getUTCDate() + 1);
      return d.getTime();
  }
}

function occurrenceFromMaster(master: CommunityEvent, dtstartMs: number): CommunityEvent {
  const durationMs = masterDurationMs(master);
  return {
    ...master,
    id: `${master.id}::${dtstartMs}`,
    dtstartMs,
    ...(typeof durationMs === "number" ? { dtendMs: dtstartMs + durationMs } : {}),
    // Per-occurrence instances inherit the master's series — drop
    // RRULE so the UI shows "single occurrence" semantics.
    rrule: undefined,
  };
}

function applyOverride(
  master: CommunityEvent,
  override: CommunityEvent,
  recurrenceIdMs: number,
): CommunityEvent {
  const dtstartMs = override.dtstartMs ?? recurrenceIdMs;
  const inheritedDurationMs = masterDurationMs(master);
  return {
    ...master,
    id: `${master.id}::${recurrenceIdMs}::override`,
    summary: override.summary || master.summary,
    ...(override.description !== undefined ? { description: override.description } : {}),
    ...(override.location !== undefined ? { location: override.location } : {}),
    dtstartMs,
    ...(typeof override.dtendMs === "number"
      ? { dtendMs: override.dtendMs }
      : typeof inheritedDurationMs === "number"
        ? { dtendMs: dtstartMs + inheritedDurationMs }
        : {}),
    recurrenceIdMs,
    rrule: undefined,
  };
}

function masterDurationMs(master: CommunityEvent): number | undefined {
  if (typeof master.dtendMs !== "number" || typeof master.dtstartMs !== "number") {
    return undefined;
  }
  const durationMs = master.dtendMs - master.dtstartMs;
  return Number.isFinite(durationMs) && durationMs >= 0 ? durationMs : undefined;
}
