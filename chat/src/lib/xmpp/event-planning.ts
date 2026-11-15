/** Shared event-workspace projection, backed by room-authorised XMPP PubSub. */
export type PlanningKind = "event" | "link" | "task" | "itinerary" | "decision" | "travel" | "expense" | "poll" | "vote" | "attendance";
export type AttendanceStatus = "invited" | "interested" | "going" | "declined";
export interface PlanningItem {
  id: string;
  kind: PlanningKind;
  author?: string | null;
  title: string;
  description?: string | null;
  url?: string | null;
  category?: string | null;
  location?: string | null;
  timezone?: string | null;
  starts_at?: string | null;
  ends_at?: string | null;
  assignee?: string | null;
  completed: boolean;
  pinned: boolean;
  amount_minor?: number | null;
  currency?: string | null;
  participants: string[];
  options: { id: string; label: string }[];
  selections: string[];
  subject?: string | null;
  attendance?: AttendanceStatus | null;
  status?: "proposed" | "confirmed" | "cancelled" | null;
}

export type EventPlanningWasmClient = {
  event_planning_items?: (room: string) => Promise<PlanningItem[]>;
  event_planning_publish?: (room: string, input: PlanningItem) => Promise<PlanningItem>;
  event_planning_retract?: (room: string, id: string) => Promise<unknown>;
  event_room_create?: (name: string, privateRoom: boolean) => Promise<string>;
};

export function newPlanningItem(kind: PlanningKind, title: string, id: string = crypto.randomUUID()): PlanningItem {
  return { id, kind, title, completed: false, pinned: false, participants: [], options: [], selections: [] };
}

/** Resolve a wall-clock time in an IANA zone, rejecting skipped or ambiguous times. */
export function planningInstant(local: string, timezone: string): string | undefined {
  if (!local) return undefined;
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(local)) throw new Error("Choose a valid date and time.");
  let formatter: Intl.DateTimeFormat;
  try {
    formatter = new Intl.DateTimeFormat("en-CA", { timeZone: timezone, year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
  } catch {
    throw new Error("Choose a valid time zone, such as Europe/London.");
  }
  const assumed = Date.parse(`${local}:00Z`);
  const candidates: number[] = [];
  for (let offset = -14 * 60; offset <= 14 * 60; offset += 15) {
    const instant = assumed + offset * 60_000;
    const parts = Object.fromEntries(formatter.formatToParts(instant).map((part) => [part.type, part.value]));
    const wall = `${parts.year}-${parts.month}-${parts.day}T${parts.hour}:${parts.minute}`;
    if (wall === local) candidates.push(instant);
  }
  if (candidates.length !== 1) throw new Error(candidates.length ? "This time occurs twice when clocks change. Choose another time." : "This time does not exist in the selected time zone.");
  return new Date(candidates[0]!).toISOString();
}

export function planningLocal(instant: string | null | undefined, timezone: string): string {
  if (!instant) return "";
  const parts = Object.fromEntries(new Intl.DateTimeFormat("en-CA", { timeZone: timezone, year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hourCycle: "h23" }).formatToParts(new Date(instant)).map((part) => [part.type, part.value]));
  return `${parts.year}-${parts.month}-${parts.day}T${parts.hour}:${parts.minute}`;
}

export function safePlanningUrl(value: string | null | undefined): string | null {
  if (!value) return null;
  try { const url = new URL(value); return ["http:", "https:"].includes(url.protocol) && !url.username && !url.password ? url.href : null; } catch { return null; }
}

function currencyFractionDigits(currency: string): number {
  return new Intl.NumberFormat("en", { style: "currency", currency }).resolvedOptions().maximumFractionDigits ?? 2;
}

/** Parse major-unit decimal text without binary floating-point rounding. */
export function parsePlanningAmount(text: string, currency: string): number {
  const match = /^(\d*)(?:\.(\d*))?$/.exec(text.trim());
  if (!match || (!match[1] && !match[2])) throw new Error("Enter a non-negative decimal amount.");
  const digits = currencyFractionDigits(currency);
  const fraction = (match[2] ?? "").replace(/0+$/, "");
  if (fraction.length > digits) throw new Error(`${currency} amounts allow at most ${digits} decimal places.`);
  const whole = (match[1] || "0").replace(/^0+(?=\d)/, "");
  if (whole.length > 16) throw new Error("This amount is too large.");
  const minor = BigInt(whole) * 10n ** BigInt(digits) + BigInt(fraction.padEnd(digits, "0") || "0");
  if (minor > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("This amount is too large.");
  return Number(minor);
}

/** Preserve every minor unit when populating an expense editor. */
export function formatPlanningAmount(minor: number, currency: string): string {
  if (!Number.isSafeInteger(minor) || minor < 0) throw new Error("Enter a valid minor-unit amount.");
  const digits = currencyFractionDigits(currency);
  const factor = 10n ** BigInt(digits);
  const amount = BigInt(minor);
  const whole = String(amount / factor);
  return digits ? `${whole}.${String(amount % factor).padStart(digits, "0")}` : whole;
}

function calendarText(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/\r?\n/g, "\\n").replace(/;/g, "\\;").replace(/,/g, "\\,");
}

/** A local export never publishes private workspace details to the community feed. */
export function eventPlanningCalendar(items: readonly PlanningItem[], room: string, now = new Date()): string {
  const stamp = (value: string) => new Date(value).toISOString().replace(/[-:]/g, "").replace(/\.\d{3}Z$/, "Z");
  const lines = ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//Waddle//Event Planning//EN", "CALSCALE:GREGORIAN"];
  for (const item of items.filter((entry) => ["event", "itinerary"].includes(entry.kind) && entry.starts_at)) {
    lines.push("BEGIN:VEVENT", `UID:${calendarText(`${item.id}@${room}`)}`, `DTSTAMP:${stamp(now.toISOString())}`, `DTSTART:${stamp(item.starts_at!)}`, `SUMMARY:${calendarText(item.title)}`);
    if (item.ends_at) lines.push(`DTEND:${stamp(item.ends_at)}`);
    if (item.description) lines.push(`DESCRIPTION:${calendarText(item.description)}`);
    if (item.location) lines.push(`LOCATION:${calendarText(item.location)}`);
    if (item.status === "cancelled") lines.push("STATUS:CANCELLED");
    lines.push("END:VEVENT");
  }
  lines.push("END:VCALENDAR");
  // Fold UTF-8 content lines at 75 octets (RFC 5545 §3.1).
  return lines.flatMap((line) => {
    const folded: string[] = []; let current = ""; let bytes = 0;
    for (const char of line) {
      const size = new TextEncoder().encode(char).length;
      if (bytes + size > 75) { folded.push(current); current = " "; bytes = 1; }
      current += char; bytes += size;
    }
    folded.push(current); return folded;
  }).join("\r\n") + "\r\n";
}
