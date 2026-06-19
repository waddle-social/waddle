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

/** RFC 5545 §3.2.12 participation status. */
export type PartStat = "NEEDS-ACTION" | "ACCEPTED" | "DECLINED" | "TENTATIVE";

export interface Attendee {
  /** Typically `xmpp:user@example.com`. */
  uri: string;
  partstat: PartStat;
  role?: string;
  rsvp?: boolean;
}

export type CalendarDateValue =
  | { kind: "date"; date: string }
  | { kind: "date-time"; ms: number };

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
  dtstart?: CalendarDateValue;
  dtend?: CalendarDateValue;
  rrule?: Rrule;
  /**
   * RSVPs aggregated from sibling `<master-uid>-rsvp-<localpart>`
   * pubsub items. Empty for events with no RSVPs yet. Keyed by
   * attendee URI bare-JID at the chat layer to dedupe latest-wins.
   */
  attendees?: Attendee[];
  /**
   * RFC 5545 RECURRENCE-ID — set on override events that replace a
   * single occurrence of a recurring master. `undefined` on master
   * events.
   */
  recurrenceId?: CalendarDateValue;
  /**
   * RFC 5545 EXDATE — occurrence DTSTART values to skip on a
   * recurring master. Empty on overrides and on non-recurring events.
   */
  exdates?: CalendarDateValue[];
}

export interface CommunityEventInput {
  summary: string;
  description?: string;
  location?: string;
  organizer?: string;
  dtstart?: CalendarDateValue;
  dtend?: CalendarDateValue;
  rrule?: Rrule;
  /**
   * EXDATE list — occurrence DTSTART values to skip on a recurring
   * master. Only meaningful when `rrule` is set. Empty / absent
   * means no occurrences are excluded.
   */
  exdates?: CalendarDateValue[];
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

interface WasmAttendee {
  uri: string;
  partstat: string;
  role?: string | null;
  rsvp?: boolean | null;
}

export interface WasmVEvent {
  id: string;
  uid: string;
  summary: string;
  description?: string | null;
  location?: string | null;
  organizer?: string | null;
  dtstamp?: string | null;
  dtstart?: CalendarDateValue | null;
  dtend?: CalendarDateValue | null;
  rrule?: WasmRrule | null;
  attendees?: WasmAttendee[] | null;
  recurrence_id?: CalendarDateValue | null;
  exdates?: CalendarDateValue[] | null;
}

const PARTSTATS: ReadonlySet<PartStat> = new Set([
  "NEEDS-ACTION",
  "ACCEPTED",
  "DECLINED",
  "TENTATIVE",
]);

function isPartStat(value: string): value is PartStat {
  return PARTSTATS.has(value as PartStat);
}

function attendeeFromWasm(wasm: WasmAttendee): Attendee | null {
  if (!wasm.uri) return null;
  const partstat: PartStat = isPartStat(wasm.partstat) ? wasm.partstat : "NEEDS-ACTION";
  return {
    uri: wasm.uri,
    partstat,
    ...(wasm.role ? { role: wasm.role } : {}),
    ...(typeof wasm.rsvp === "boolean" ? { rsvp: wasm.rsvp } : {}),
  };
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
  const rrule = event.rrule ? rruleFromWasm(event.rrule) ?? undefined : undefined;
  const attendees = (event.attendees ?? [])
    .map(attendeeFromWasm)
    .filter((a): a is Attendee => a !== null);
  const dtstart = calendarDateFromWasm(event.dtstart);
  const dtend = calendarDateFromWasm(event.dtend);
  const recurrenceId = calendarDateFromWasm(event.recurrence_id);
  const exdates = (event.exdates ?? [])
    .map(calendarDateFromWasm)
    .filter((value): value is CalendarDateValue => value !== undefined);
  return {
    id: event.id,
    uid: event.uid,
    summary: event.summary,
    ...(event.description ? { description: event.description } : {}),
    ...(event.location ? { location: event.location } : {}),
    ...(event.organizer ? { organizer: event.organizer } : {}),
    ...(typeof dtstamp === "number" && Number.isFinite(dtstamp) ? { dtstampMs: dtstamp } : {}),
    ...(dtstart ? { dtstart } : {}),
    ...(dtend ? { dtend } : {}),
    ...(rrule ? { rrule } : {}),
    ...(attendees.length > 0 ? { attendees } : {}),
    ...(recurrenceId ? { recurrenceId } : {}),
    ...(exdates.length > 0 ? { exdates } : {}),
  };
}

function calendarDateFromWasm(value: CalendarDateValue | null | undefined): CalendarDateValue | undefined {
  if (!value) return undefined;
  if (value.kind === "date") {
    return /^\d{4}-\d{2}-\d{2}$/.test(value.date) ? value : undefined;
  }
  if (value.kind === "date-time") {
    return Number.isFinite(value.ms) ? value : undefined;
  }
  return undefined;
}

function parseRsvpItemId(itemId: string): { masterUid: string; localpart: string } | null {
  const idx = itemId.lastIndexOf("-rsvp-");
  if (idx <= 0) return null;
  const localpart = itemId.slice(idx + "-rsvp-".length);
  if (!localpart) return null;
  return { masterUid: itemId.slice(0, idx), localpart };
}

/**
 * Group a flat item list (from `xcal_items`) into master events
 * with their sibling RSVPs folded into the master's `attendees`.
 * RSVP items override the master's own attendees on a per-URI basis
 * (latest-wins by item dtstamp where available, else last seen).
 */
export function groupEventsWithRsvps(items: readonly CommunityEvent[]): CommunityEvent[] {
  const masters = new Map<string, CommunityEvent>();
  const rsvps = new Map<string, Map<string, Attendee>>();
  const overrides: CommunityEvent[] = [];

  for (const item of items) {
    const parsed = parseRsvpItemId(item.id);
    if (parsed) {
      const single = item.attendees?.[0];
      if (single) {
        const bucket = rsvps.get(parsed.masterUid) ?? new Map<string, Attendee>();
        bucket.set(single.uri, single);
        rsvps.set(parsed.masterUid, bucket);
      }
      continue;
    }
    if (item.recurrenceId) {
      overrides.push(item);
      continue;
    }
    masters.set(item.uid, item);
  }

  const out: CommunityEvent[] = [];
  for (const master of masters.values()) {
    const merged = new Map<string, Attendee>();
    for (const attendee of master.attendees ?? []) {
      merged.set(attendee.uri, attendee);
    }
    for (const [uri, attendee] of rsvps.get(master.uid) ?? new Map()) {
      merged.set(uri, attendee);
    }
    const attendees = [...merged.values()];
    out.push({
      ...master,
      ...(attendees.length > 0 ? { attendees } : {}),
    });
  }
  return [...out, ...overrides];
}
