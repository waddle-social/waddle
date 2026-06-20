import type { CalendarDateValue, CommunityEvent } from "./event-types";

export interface CalendarEventBounds {
  startMs: number;
  endMs: number;
  isAllDay: boolean;
}

export interface LocalDayRange {
  startMs: number;
  endMs: number;
}

export function dateTimeValue(ms: number): CalendarDateValue {
  return { kind: "date-time", ms };
}

export function dateValue(date: string): CalendarDateValue {
  return { kind: "date", date };
}

export function calendarDateKey(value: CalendarDateValue): string {
  return value.kind === "date" ? `date:${value.date}` : `date-time:${value.ms}`;
}

export function localDateStringFromMs(ms: number): string {
  const date = new Date(ms);
  return localDateString(date.getFullYear(), date.getMonth(), date.getDate());
}

export function localDateString(year: number, monthIndex: number, day: number): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${year}-${pad(monthIndex + 1)}-${pad(day)}`;
}

export function addDaysToDateString(date: string, days: number): string {
  const parts = parseDateParts(date);
  if (!parts) return date;
  const shifted = new Date(Date.UTC(parts.year, parts.monthIndex, parts.day + days));
  return localDateString(shifted.getUTCFullYear(), shifted.getUTCMonth(), shifted.getUTCDate());
}

export function localDayRange(year: number, monthIndex: number, day: number): LocalDayRange {
  const startMs = new Date(year, monthIndex, day).getTime();
  const endMs = new Date(year, monthIndex, day + 1).getTime();
  return { startMs, endMs };
}

export function calendarDateStartMs(value: CalendarDateValue): number {
  if (value.kind === "date-time") return value.ms;
  const parts = parseDateParts(value.date);
  if (!parts) return Number.NaN;
  return new Date(parts.year, parts.monthIndex, parts.day).getTime();
}

export function eventBounds(event: CommunityEvent): CalendarEventBounds | null {
  if (!event.dtstart) return null;
  const startMs = calendarDateStartMs(event.dtstart);
  if (!Number.isFinite(startMs)) return null;
  const isAllDay = event.dtstart.kind === "date";
  const rawEndMs = event.dtend ? calendarDateStartMs(event.dtend) : Number.NaN;
  const fallbackEndMs =
    event.dtstart.kind === "date" ? fallbackAllDayEndMs(event.dtstart.date) : startMs + 1;
  const endMs = Number.isFinite(rawEndMs) && rawEndMs > startMs ? rawEndMs : fallbackEndMs;
  return { startMs, endMs, isAllDay };
}

export function eventOverlapsRange(
  event: CommunityEvent,
  rangeStartMs: number,
  rangeEndMs: number,
): boolean {
  const bounds = eventBounds(event);
  if (!bounds) return false;
  return bounds.startMs < rangeEndMs && bounds.endMs > rangeStartMs;
}

export function isEventUpcomingOrOngoing(event: CommunityEvent, nowMs: number = Date.now()): boolean {
  const bounds = eventBounds(event);
  if (!bounds) return false;
  return bounds.endMs > nowMs;
}

export function sortEventsUpcomingFirst(
  events: readonly CommunityEvent[],
  nowMs: number = Date.now(),
): CommunityEvent[] {
  const upcoming: CommunityEvent[] = [];
  const past: CommunityEvent[] = [];
  for (const event of events) {
    if (isEventUpcomingOrOngoing(event, nowMs)) upcoming.push(event);
    else past.push(event);
  }
  upcoming.sort((a, b) => compareEventsByBounds(a, b, true));
  past.sort((a, b) => compareEventsByBounds(b, a, true));
  return [...upcoming, ...past];
}

export function sortEventsForDay(
  events: readonly CommunityEvent[],
  dayStartMs: number,
): CommunityEvent[] {
  return [...events].sort((a, b) => {
    const aBounds = eventBounds(a);
    const bBounds = eventBounds(b);
    if (!aBounds && !bBounds) return a.summary.localeCompare(b.summary);
    if (!aBounds) return 1;
    if (!bBounds) return -1;
    if (aBounds.isAllDay !== bBounds.isAllDay) return aBounds.isAllDay ? -1 : 1;
    const aVisible = Math.max(aBounds.startMs, dayStartMs);
    const bVisible = Math.max(bBounds.startMs, dayStartMs);
    return aVisible - bVisible || aBounds.endMs - bBounds.endMs || a.summary.localeCompare(b.summary);
  });
}

export function compareCalendarDateValues(a: CalendarDateValue, b: CalendarDateValue): number {
  if (a.kind === "date" && b.kind === "date") return a.date.localeCompare(b.date);
  return calendarDateStartMs(a) - calendarDateStartMs(b);
}

function compareEventsByBounds(a: CommunityEvent, b: CommunityEvent, allDayFirst: boolean): number {
  const aBounds = eventBounds(a);
  const bBounds = eventBounds(b);
  if (!aBounds && !bBounds) return a.summary.localeCompare(b.summary);
  if (!aBounds) return 1;
  if (!bBounds) return -1;
  if (allDayFirst && aBounds.isAllDay !== bBounds.isAllDay) {
    return aBounds.isAllDay ? -1 : 1;
  }
  return aBounds.startMs - bBounds.startMs || a.summary.localeCompare(b.summary);
}

function fallbackAllDayEndMs(date: string): number {
  const parts = parseDateParts(addDaysToDateString(date, 1));
  if (!parts) return Number.NaN;
  return new Date(parts.year, parts.monthIndex, parts.day).getTime();
}

function parseDateParts(date: string):
  | { year: number; monthIndex: number; day: number }
  | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!match) return null;
  return {
    year: Number(match[1]),
    monthIndex: Number(match[2]) - 1,
    day: Number(match[3]),
  };
}
