/**
 * xCal calendar event shapes mirrored from the wasm bridge. Each
 * pubsub item carries a full VCALENDAR/VEVENT payload in the
 * `urn:ietf:params:xml:ns:xcal` namespace; recurrence flows through
 * an RRULE with FREQ + INTERVAL + BYDAY + BYMONTHDAY + COUNT|UNTIL
 * per RFC 5545.
 */

export type Freq = "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY";

/** ISO weekday two-letter codes used in BYDAY. */
export type Weekday = "SU" | "MO" | "TU" | "WE" | "TH" | "FR" | "SA";

export interface Rrule {
  freq: Freq;
  interval?: number;
  byDay?: Weekday[];
  byMonthDay?: number[];
  /** Mutually exclusive with `untilMs`. */
  count?: number;
  /** Mutually exclusive with `count`. Epoch ms. */
  untilMs?: number;
}

export interface CommunityEvent {
  /** Pubsub item id (UUID). */
  id: string;
  /** iCalendar UID. Usually identical to `id` for events we publish. */
  uid: string;
  summary: string;
  description?: string;
  location?: string;
  organizer?: string;
  /** Epoch ms. */
  dtstampMs?: number;
  /** Epoch ms. */
  dtstartMs?: number;
  /** Epoch ms. */
  dtendMs?: number;
  rrule?: Rrule;
}

export interface CommunityEventInput {
  summary: string;
  description?: string;
  location?: string;
  organizer?: string;
  /** Epoch ms. */
  dtstartMs?: number;
  /** Epoch ms. */
  dtendMs?: number;
  rrule?: Rrule;
}

// ── Wasm boundary ───────────────────────────────────────────────────

export interface WasmRrule {
  freq: string;
  interval?: number | null;
  by_day?: string[] | null;
  by_month_day?: number[] | null;
  count?: number | null;
  until?: string | null;
}

export interface WasmVEvent {
  id: string;
  uid: string;
  summary: string;
  description?: string | null;
  location?: string | null;
  organizer?: string | null;
  dtstamp?: string | null;
  dtstart?: string | null;
  dtend?: string | null;
  rrule?: WasmRrule | null;
}

function isFreq(value: string): value is Freq {
  return value === "DAILY" || value === "WEEKLY" || value === "MONTHLY" || value === "YEARLY";
}

function isWeekday(value: string): value is Weekday {
  return ["SU", "MO", "TU", "WE", "TH", "FR", "SA"].includes(value);
}

function rruleFromWasm(wasm: WasmRrule): Rrule | null {
  if (!isFreq(wasm.freq)) return null;
  const byDay = (wasm.by_day ?? []).filter(isWeekday);
  const byMonthDay = (wasm.by_month_day ?? []).filter((n) => Number.isInteger(n));
  const untilMs = wasm.until ? Date.parse(wasm.until) : undefined;
  return {
    freq: wasm.freq,
    ...(typeof wasm.interval === "number" ? { interval: wasm.interval } : {}),
    ...(byDay.length > 0 ? { byDay } : {}),
    ...(byMonthDay.length > 0 ? { byMonthDay } : {}),
    ...(typeof wasm.count === "number" ? { count: wasm.count } : {}),
    ...(typeof untilMs === "number" && Number.isFinite(untilMs) ? { untilMs } : {}),
  };
}

export function rruleToWasm(rrule: Rrule): WasmRrule {
  return {
    freq: rrule.freq,
    ...(typeof rrule.interval === "number" ? { interval: rrule.interval } : {}),
    ...(rrule.byDay && rrule.byDay.length > 0 ? { by_day: rrule.byDay } : {}),
    ...(rrule.byMonthDay && rrule.byMonthDay.length > 0 ? { by_month_day: rrule.byMonthDay } : {}),
    ...(typeof rrule.count === "number" ? { count: rrule.count } : {}),
    ...(typeof rrule.untilMs === "number" ? { until: new Date(rrule.untilMs).toISOString() } : {}),
  };
}

export function eventFromWasm(event: WasmVEvent): CommunityEvent {
  const dtstamp = event.dtstamp ? Date.parse(event.dtstamp) : undefined;
  const dtstart = event.dtstart ? Date.parse(event.dtstart) : undefined;
  const dtend = event.dtend ? Date.parse(event.dtend) : undefined;
  const rrule = event.rrule ? rruleFromWasm(event.rrule) ?? undefined : undefined;
  return {
    id: event.id,
    uid: event.uid,
    summary: event.summary,
    ...(event.description ? { description: event.description } : {}),
    ...(event.location ? { location: event.location } : {}),
    ...(event.organizer ? { organizer: event.organizer } : {}),
    ...(typeof dtstamp === "number" && Number.isFinite(dtstamp) ? { dtstampMs: dtstamp } : {}),
    ...(typeof dtstart === "number" && Number.isFinite(dtstart) ? { dtstartMs: dtstart } : {}),
    ...(typeof dtend === "number" && Number.isFinite(dtend) ? { dtendMs: dtend } : {}),
    ...(rrule ? { rrule } : {}),
  };
}

/** Sort events by upcoming-first, past events at the end newest-first. */
export function sortEventsUpcomingFirst(
  events: readonly CommunityEvent[],
  nowMs: number = Date.now(),
): CommunityEvent[] {
  const upcoming: CommunityEvent[] = [];
  const past: CommunityEvent[] = [];
  for (const event of events) {
    if (typeof event.dtstartMs === "number" && event.dtstartMs < nowMs) {
      past.push(event);
    } else {
      upcoming.push(event);
    }
  }
  upcoming.sort((a, b) => (a.dtstartMs ?? Infinity) - (b.dtstartMs ?? Infinity));
  past.sort((a, b) => (b.dtstartMs ?? 0) - (a.dtstartMs ?? 0));
  return [...upcoming, ...past];
}
