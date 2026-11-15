<script setup lang="ts">
import { computed, onUnmounted, ref, useId } from "vue";
import {
  CalendarDays,
  ChevronLeft,
  ChevronRight,
  Clock3,
  Copy,
  List,
  MapPin,
  Menu,
  Pencil,
  Plus,
  RefreshCw,
  Repeat,
  Trash2,
} from "lucide-vue-next";
import EventPlanningPanel from "@/components/community/EventPlanningPanel.vue";
import { newPlanningItem } from "@/lib/xmpp/event-planning";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import CalendarFeedUrlPanel from "@/components/community/CalendarFeedUrlPanel.vue";
import RecurrencePicker from "@/components/community/RecurrencePicker.vue";
import type {
  Attendee,
  CalendarDateValue,
  CommunityEvent,
  CommunityEventInput,
  PartStat,
  Rrule,
  Weekday,
} from "@/lib/xmpp-client";
import {
  addDaysToDateString,
  calendarDateStartMs,
  dateTimeValue,
  dateValue,
  eventOverlapsRange,
  isEventUpcomingOrOngoing,
  localDateStringFromMs,
  localDayRange,
  sortEventsForDay,
} from "@/lib/xmpp-client";
import { useCalendarFeedCopy } from "@/lib/use-calendar-feed-copy";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";
import spot from "@waddle/design/assets/spot-stickers.svg?url";
import { css, cx } from "styled-system/css";
import { btn, chip, empty, iconbtn, input, kicker, scrim, seg } from "styled-system/recipes";

interface EventsPaneProps {
  client?: BrowserXmppClient | null;
  events: readonly CommunityEvent[];
  isLoading: boolean;
  isPosting: boolean;
  error: string | null;
  canPost: boolean;
  selfJid: string | null;
  communityJid: string | null;
  serverBaseUrl: string;
  sessionId: string | null;
  /**
   * Resolver from a UID back to the unexpanded master event so the
   * pane can edit / cancel the actual pubsub item rather than the
   * synthetic per-instance id produced by client-side expansion.
   */
  findMaster: (uid: string) => CommunityEvent | null;
}

const props = defineProps<EventsPaneProps>();
const emit = defineEmits<{
  refresh: [];
  post: [input: CommunityEventInput];
  edit: [itemId: string, input: CommunityEventInput];
  cancelSeries: [masterId: string];
  cancelInstance: [masterUid: string, instanceDtstart: CalendarDateValue];
  rsvp: [event: CommunityEvent, partstat: PartStat];
  openNav: [];
  openEventConversation: [roomJid: string];
}>();

/**
 * Strip `xmpp:` URI prefix and trim any resource so attendee /
 * organiser URIs compare cleanly against a session's bare JID.
 */
function bareFromUri(uri: string | undefined | null): string | null {
  if (!uri) return null;
  const stripped = uri.startsWith("xmpp:") ? uri.slice(5) : uri;
  return barePeerJid(stripped);
}

function isOrganiser(event: CommunityEvent): boolean {
  const self = selfBareJid.value;
  if (!self) return false;
  const eventOrganiser = bareFromUri(event.organizer);
  return !!eventOrganiser && eventOrganiser === self;
}

// ─── Identity ────────────────────────────────────────────────────────────────

const selfBareJid = computed(() => {
  const raw = props.selfJid;
  return raw ? barePeerJid(raw) : null;
});

const selfAttendeeUri = computed(() => {
  const bare = selfBareJid.value;
  return bare ? `xmpp:${bare}` : null;
});

// ─── Classes ──────────────────────────────────────────────────────────────────

const kickerClass = kicker();
const primaryButtonClass = btn({ variant: "primary" });
const primarySmButtonClass = btn({ variant: "primary", size: "sm" });
const quietButtonClass = btn({ size: "sm" });
const quietButtonLgClass = btn();
const segClass = seg();
const chipClass = chip();
const iconButtonClass = iconbtn();
const inputClass = input();
const spotUrl = spot;
const scrimClass = cx(scrim(), css({ position: "absolute", inset: 0, zIndex: "auto" }));
const formTitleId = useId();

const ui = {
  root: css({ display: "flex", flexDirection: "column", flex: "1 1 0%", minHeight: 0, bg: "bg", color: "fg" }),
  mobileBar: css({
    display: "flex",
    alignItems: "center",
    gap: "1.5",
    flex: "none",
    padding: "2",
    borderBottom: "1px solid {colors.border.soft}",
    bg: "bg.rail",
    color: "fg",
    md: { display: "none" },
  }),
  mobileTitle: css({ flex: 1, minWidth: 0, font: "700 18px/1.3 {fonts.display}" }),
  mobileNav: cx(iconbtn(), css({ width: "44px", height: "44px" })),
  scroll: css({ flex: "1 1 0%", minHeight: 0, overflowX: "hidden", overflowY: "auto", scrollbarGutter: "stable" }),
  content: css({
    display: "flex",
    flexDirection: "column",
    gap: "5",
    width: "100%",
    maxWidth: "1000px",
    boxSizing: "border-box",
    padding: "14px 12px 32px",
    md: { padding: "32px 40px" },
  }),
  head: css({ display: "none", alignItems: "center", gap: "3", flexWrap: "wrap", md: { display: "flex" } }),
  h1: css({ margin: 0, flex: 1, font: "700 30px/1.2 {fonts.display}", letterSpacing: "-0.01em" }),
  toolbar: css({ display: "flex", alignItems: "center", gap: "2.5", flexWrap: "wrap" }),
  feedTools: css({ display: "flex", alignItems: "center", gap: "2", ml: "auto" }),
  status: css({ fontSize: "xs", color: "fg.muted", md: { minWidth: "5.5rem" } }),
  spin: css({ _motionSafe: { animation: "spin 1s linear infinite" } }),
  error: css({
    padding: "10px 14px",
    borderRadius: "control",
    border: "1px solid {colors.danger}",
    bg: "danger.subtle",
    color: "danger.text",
    fontSize: "sm",
  }),
  // Form (Events-Form board)
  formTitle: css({ margin: 0, font: "700 24px/1.2 {fonts.display}", letterSpacing: "-0.01em" }),
  form: css({
    display: "flex",
    flexDirection: "column",
    gap: "4",
    maxWidth: "680px",
    padding: "5",
    borderRadius: "panel",
    bg: "surface",
    border: "1px solid {colors.border.soft}",
  }),
  titleInput: cx(input(), css({ width: "100%", height: "48px", font: "600 18px/1.3 {fonts.display}" })),
  field: css({ display: "flex", flexDirection: "column", gap: "1.5", minWidth: 0, fontSize: "13px", fontWeight: 600, color: "fg.muted" }),
  fieldInput: cx(input(), css({ width: "100%", minWidth: 0, fontWeight: 400 })),
  textarea: cx(input(), css({ width: "100%", height: "auto", minHeight: "88px", py: "2.5", resize: "vertical", font: "400 14px/1.5 {fonts.body}" })),
  pair: css({ display: "grid", gap: "3", md: { gridTemplateColumns: "1fr 1fr" } }),
  span2: css({ md: { gridColumn: "span 2" } }),
  allDay: css({
    alignSelf: "flex-start",
    display: "inline-flex",
    alignItems: "center",
    gap: "2",
    height: "chipH",
    px: "3",
    borderRadius: "pill",
    border: "1px solid {colors.border}",
    fontSize: "13px",
    fontWeight: 600,
    color: "fg",
    cursor: "pointer",
    "& input": { width: "16px", height: "16px", margin: 0, accentColor: "{colors.action}" },
    "&:has(input:checked)": { borderColor: "action", bg: "action.subtle" },
    "&:has(input:focus-visible)": { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
  }),
  alert: css({ display: "flex", alignItems: "center", gap: "1.5", margin: "-8px 0 0", fontSize: "13px", color: "danger.text", "& svg": { width: "16px", height: "16px", flex: "none" } }),
  formFoot: css({ display: "flex", justifyContent: "flex-end", gap: "2", pt: "1" }),
  // Month grid (Events-Calendar board)
  monthNav: css({ display: "flex", alignItems: "center", gap: "2", flexWrap: "wrap" }),
  monthTitle: css({ margin: 0, minWidth: "170px", textAlign: "center", font: "700 20px/1.25 {fonts.display}" }),
  todayButton: cx(btn({ size: "sm" }), css({ ml: "auto" })),
  grid: css({
    display: "grid",
    gridTemplateColumns: "repeat(7, minmax(0, 1fr))",
    gap: "1",
    padding: "2",
    borderRadius: "panel",
    bg: "surface",
    border: "1px solid {colors.border.soft}",
    _narrow: { gap: "0.5", padding: "1" },
  }),
  dow: cx(kicker(), css({ textAlign: "center", py: "1" })),
  cell: css({
    position: "relative",
    display: "flex",
    flexDirection: "column",
    alignItems: "stretch",
    gap: "3px",
    minWidth: 0,
    height: "92px",
    boxSizing: "border-box",
    padding: "6px",
    border: 0,
    borderRadius: "10px",
    bg: "transparent",
    color: "fg",
    font: "inherit",
    textAlign: "left",
    cursor: "pointer",
    overflow: "hidden",
    _hover: { bg: "state.hover" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
    _disabled: { cursor: "default", _hover: { bg: "transparent" } },
    "&[aria-pressed=true]": { bg: "surface.2", boxShadow: "inset 0 0 0 1px {colors.border.strong}" },
    "&[data-today]": { bg: "surface.2", boxShadow: "inset 0 0 0 2px {colors.gold}" },
    _narrow: { height: "56px", padding: "4px", alignItems: "center" },
  }),
  cellNum: css({
    fontSize: "13px",
    fontWeight: 500,
    fontVariantNumeric: "tabular-nums",
    "[data-today] > &": { fontWeight: 700, color: "gold.text" },
  }),
  pill: css({
    display: "block",
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
    padding: "2px 6px",
    borderRadius: "6px",
    fontSize: "12px",
    lineHeight: "16px",
    bg: "surface.2",
    color: "fg.muted",
    "&[data-mine]": { bg: "action.subtle", color: "action.text" },
    _narrow: { display: "none" },
  }),
  more: css({ px: "1", fontSize: "11px", color: "fg.muted", _narrow: { display: "none" } }),
  dot: css({ display: "none", width: "5px", height: "5px", borderRadius: "pill", bg: "action", _narrow: { display: "block" } }),
  // Day sections and the one event card (EventCard board)
  section: css({ display: "flex", flexDirection: "column", gap: "2.5" }),
  dayTitle: css({ margin: 0, font: "600 18px/1.3 {fonts.display}" }),
  card: css({
    display: "flex",
    flexDirection: "column",
    gap: "2",
    padding: "16px 20px",
    borderRadius: "panel",
    bg: "surface",
    border: "1px solid {colors.border.soft}",
    color: "fg",
    "&[data-live]": { borderColor: "live" },
    _narrow: { padding: "14px 16px" },
  }),
  cardHead: css({ display: "flex", alignItems: "flex-start", gap: "2" }),
  cardMain: css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "1" }),
  cardKicker: cx(kicker(), css({ color: "action.text", "[data-live] &": { color: "live.text" } })),
  cardTitle: css({ margin: 0, font: "700 18px/1.3 {fonts.display}", letterSpacing: "-0.01em", overflowWrap: "anywhere" }),
  cardMeta: css({ display: "inline-flex", alignItems: "center", gap: "1", fontSize: "xs", color: "fg.muted", "& svg": { width: "14px", height: "14px", flex: "none" } }),
  cardDesc: css({ margin: 0, fontSize: "14px", lineHeight: "1.5", whiteSpace: "pre-wrap", overflowWrap: "anywhere" }),
  cardRepeat: css({ display: "inline-flex", alignItems: "center", gap: "1.5", fontSize: "13px", color: "action.text", "& svg": { width: "16px", height: "16px" } }),
  cardFoot: css({
    display: "flex",
    alignItems: "center",
    gap: "3",
    flexWrap: "wrap",
    mt: "1",
    pt: "1.5",
    borderTop: "1px solid {colors.border.soft}",
  }),
  counts: css({ fontSize: "xs", color: "fg.muted", fontVariantNumeric: "tabular-nums" }),
  dayEmpty: css({
    padding: "4",
    borderRadius: "panel",
    border: "1px dashed {colors.border}",
    fontSize: "sm",
    color: "fg.muted",
    textAlign: "center",
  }),
  empty: cx(empty(), css({ py: "16", "& h2": { margin: 0, font: "700 24px/1.25 {fonts.display}", color: "fg", textWrap: "balance" } })),
  // Cancel sheet (Events-Cancel / Mobile-Events boards)
  sheetPositioner: css({
    position: "fixed",
    inset: 0,
    zIndex: "dialog",
    display: "flex",
    alignItems: "flex-end",
    justifyContent: "center",
    md: { alignItems: "center", padding: "4" },
  }),
  sheet: css({
    position: "relative",
    display: "flex",
    flexDirection: "column",
    gap: "3",
    width: "100%",
    boxSizing: "border-box",
    padding: "10px 16px 30px",
    borderRadius: "24px 24px 0 0",
    bg: "surface",
    borderTop: "1px solid {colors.border}",
    boxShadow: "overlay",
    color: "fg",
    md: { width: "440px", padding: "6", borderRadius: "20px", border: "1px solid {colors.border}" },
  }),
  grabber: css({ alignSelf: "center", width: "40px", height: "4px", borderRadius: "pill", bg: "border", md: { display: "none" } }),
  sheetTitle: css({ margin: "4px 0 0", font: "700 19px/1.3 {fonts.display}", md: { margin: 0, fontSize: "20px" } }),
  sheetBody: css({ margin: 0, fontSize: "14px", color: "fg.muted" }),
  option: css({
    display: "flex",
    alignItems: "center",
    gap: "3",
    width: "100%",
    minHeight: "52px",
    padding: "0 16px",
    borderRadius: "12px",
    border: "1px solid {colors.border}",
    bg: "surface",
    color: "fg",
    font: "inherit",
    textAlign: "left",
    cursor: "pointer",
    _hover: { bg: "state.hover" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
    "& > :first-child": { flex: 1, fontWeight: 600 },
    "& > :last-child": { fontSize: "xs", color: "fg.muted" },
  }),
  optionDanger: css({ borderColor: "danger", color: "danger.text", "& > :first-child": { fontWeight: 700 }, "& > :last-child": { color: "inherit" } }),
  sheetFoot: css({ display: "flex", justifyContent: "flex-end", pt: "1", "& > button": { _narrow: { width: "100%", height: "48px", justifyContent: "center" } } }),
};

// ─── Live window ──────────────────────────────────────────────────────────────

const SOON_WINDOW_MS = 2 * 60 * 60 * 1000;

/**
 * An event is "live" on the board when it is ongoing or starts within the
 * next two hours: that card carries the ember border and the only glow on
 * the page.
 */
function isEventLive(event: CommunityEvent, nowMs = Date.now()): boolean {
  if (event.dtstart?.kind !== "date-time") return false;
  const startMs = event.dtstart.ms;
  const endMs = event.dtend?.kind === "date-time" ? event.dtend.ms : startMs + 60 * 60 * 1000;
  return startMs - nowMs <= SOON_WINDOW_MS && endMs > nowMs;
}

function eventKicker(event: CommunityEvent, dayStartMs: number | null): string {
  const time = formatVisibleTime(event, dayStartMs);
  if (event.dtstart?.kind !== "date-time") return time;
  const nowMs = Date.now();
  const startMs = event.dtstart.ms;
  const endMs = event.dtend?.kind === "date-time" ? event.dtend.ms : startMs + 60 * 60 * 1000;
  if (startMs <= nowMs && endMs > nowMs) return `Happening now · ${time}`;
  if (isEventLive(event, nowMs)) return `Starting soon · ${time}`;
  return time;
}

// ─── View & filter state ──────────────────────────────────────────────────────

type ViewMode = "week" | "month";
type EventFilter = "all" | "attending";

const viewMode = ref<ViewMode>("month");
const activeFilter = ref<EventFilter>("all");

// ─── RSVP helpers ─────────────────────────────────────────────────────────────

function attendeesByPartstat(event: CommunityEvent): Record<PartStat, Attendee[]> {
  const buckets: Record<PartStat, Attendee[]> = {
    "ACCEPTED": [],
    "DECLINED": [],
    "TENTATIVE": [],
    "NEEDS-ACTION": [],
  };
  for (const a of event.attendees ?? []) {
    (buckets[a.partstat] ??= []).push(a);
  }
  return buckets;
}

function myPartstat(event: CommunityEvent): PartStat | null {
  const uri = selfAttendeeUri.value;
  if (!uri) return null;
  return event.attendees?.find((a) => a.uri === uri)?.partstat ?? null;
}

function onRsvp(event: CommunityEvent, partstat: PartStat) {
  emit("rsvp", event, partstat);
}

// ─── Filter ───────────────────────────────────────────────────────────────────

const filteredEvents = computed(() => {
  if (activeFilter.value === "all") return props.events;
  const uri = selfAttendeeUri.value;
  if (!uri) return props.events;
  return props.events.filter((e) => {
    const a = e.attendees?.find((at) => at.uri === uri);
    return a?.partstat === "ACCEPTED" || a?.partstat === "TENTATIVE";
  });
});

// ─── 7-day list view ─────────────────────────────────────────────────────────

interface WeekEventDay {
  startMs: number;
  events: CommunityEvent[];
}

/** Future or ongoing events overlapping the next 7 local days, grouped by day. */
const weekEventsByDay = computed<Map<string, WeekEventDay>>(() => {
  const groups = new Map<string, WeekEventDay>();
  const now = new Date();
  const nowMs = now.getTime();
  for (let offset = 0; offset < 7; offset += 1) {
    const day = new Date(now.getFullYear(), now.getMonth(), now.getDate() + offset);
    const { startMs: dayStart, endMs: dayEnd } = localDayRange(
      day.getFullYear(),
      day.getMonth(),
      day.getDate(),
    );
    const dayEvents = sortEventsForDay(
      filteredEvents.value.filter((event) =>
        isEventUpcomingOrOngoing(event, nowMs) && eventOverlapsRange(event, dayStart, dayEnd),
      ),
      dayStart,
    );
    if (dayEvents.length === 0) continue;
    const key = day.toLocaleDateString(undefined, {
      weekday: "long",
      month: "short",
      day: "numeric",
    });
    groups.set(key, { startMs: dayStart, events: dayEvents });
  }
  return groups;
});

// ─── Month calendar view ──────────────────────────────────────────────────────

const calYear = ref(new Date().getFullYear());
const calMonth = ref(new Date().getMonth()); // 0-indexed
const selectedDay = ref<number | null>(new Date().getDate()); // day-of-month — preselect today

const calTitle = computed(() =>
  new Date(calYear.value, calMonth.value).toLocaleDateString(undefined, {
    month: "long",
    year: "numeric",
  }),
);

function prevMonth() {
  if (calMonth.value === 0) { calMonth.value = 11; calYear.value--; }
  else calMonth.value--;
  selectedDay.value = null;
}
function nextMonth() {
  if (calMonth.value === 11) { calMonth.value = 0; calYear.value++; }
  else calMonth.value++;
  selectedDay.value = null;
}

function goToToday() {
  const now = new Date();
  calYear.value = now.getFullYear();
  calMonth.value = now.getMonth();
  selectedDay.value = now.getDate();
}

interface CalCell {
  day: number | null;
  rangeStartMs: number | null;
  isToday: boolean;
  events: CommunityEvent[];
}

const calGrid = computed<CalCell[][]>(() => {
  const y = calYear.value;
  const m = calMonth.value;
  const todayStr = new Date().toDateString();
  const firstDow = (new Date(y, m, 1).getDay() + 6) % 7; // weeks start on Monday
  const daysInMonth = new Date(y, m + 1, 0).getDate();

  const cells: CalCell[] = [];

  // Leading empty cells
  for (let i = 0; i < firstDow; i++) {
    cells.push({ day: null, rangeStartMs: null, isToday: false, events: [] });
  }

  for (let d = 1; d <= daysInMonth; d++) {
    const cellDate = new Date(y, m, d);
    const { startMs: dayStart, endMs: dayEnd } = localDayRange(y, m, d);
    const events = sortEventsForDay(
      filteredEvents.value.filter((event) => eventOverlapsRange(event, dayStart, dayEnd)),
      dayStart,
    );
    cells.push({
      day: d,
      rangeStartMs: dayStart,
      isToday: cellDate.toDateString() === todayStr,
      events,
    });
  }

  // Trailing empty cells to complete last row
  while (cells.length % 7 !== 0) cells.push({ day: null, rangeStartMs: null, isToday: false, events: [] });

  const weeks: CalCell[][] = [];
  for (let i = 0; i < cells.length; i += 7) weeks.push(cells.slice(i, i + 7));
  return weeks;
});

const selectedDayEvents = computed<CommunityEvent[]>(() => {
  const d = selectedDay.value;
  if (d === null) return [];
  return calGrid.value.flat().find((c) => c.day === d)?.events ?? [];
});

const selectedDayStartMs = computed(() => {
  const d = selectedDay.value;
  if (d === null) return null;
  return localDayRange(calYear.value, calMonth.value, d).startMs;
});

const selectedDayLabel = computed(() => {
  const d = selectedDay.value;
  if (d === null) return "";
  return new Date(calYear.value, calMonth.value, d).toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });
});

/** The next upcoming or ongoing event, unless the selected day already shows it. */
const nextEvent = computed<CommunityEvent | null>(() => {
  const nowMs = Date.now();
  const next = filteredEvents.value.find((event) => isEventUpcomingOrOngoing(event, nowMs)) ?? null;
  if (!next) return null;
  return selectedDayEvents.value.some((event) => event.id === next.id) ? null : next;
});


// ─── Formatting ───────────────────────────────────────────────────────────────

const weekdayLabels: Record<Weekday, string> = {
  SU: "Sun",
  MO: "Mon",
  TU: "Tue",
  WE: "Wed",
  TH: "Thu",
  FR: "Fri",
  SA: "Sat",
};

function formatStart(event: CommunityEvent): string {
  if (!event.dtstart) return "TBD";
  if (event.dtstart.kind === "date") {
    const start = dateLabel(event.dtstart.date);
    const end = allDayInclusiveEnd(event);
    return end && end !== event.dtstart.date ? `${start} - ${dateLabel(end)} · all day` : `${start} · all day`;
  }
  const start = new Date(event.dtstart.ms).toLocaleString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    hourCycle: "h23",
    minute: "2-digit",
  });
  if (event.dtend?.kind !== "date-time") return start;
  return `${start} - ${new Date(event.dtend.ms).toLocaleTimeString(undefined, {
    hour: "2-digit",
    hourCycle: "h23",
    minute: "2-digit",
  })}`;
}

function formatTime(event: CommunityEvent): string {
  if (!event.dtstart) return "TBD";
  if (event.dtstart.kind === "date") return "All day";
  return new Date(event.dtstart.ms).toLocaleTimeString(undefined, {
    hour: "2-digit",
    hourCycle: "h23",
    minute: "2-digit",
  });
}

function formatVisibleTime(event: CommunityEvent, dayStartMs: number | null): string {
  if (!event.dtstart) return "TBD";
  if (event.dtstart.kind === "date") return "All day";
  if (dayStartMs === null) return formatTime(event);
  const startDay = localDateStringFromMs(event.dtstart.ms);
  const visibleDay = localDateStringFromMs(dayStartMs);
  if (startDay === visibleDay) return formatTime(event);
  if (event.dtend?.kind === "date-time" && localDateStringFromMs(event.dtend.ms) === visibleDay) {
    return `continues until ${new Date(event.dtend.ms).toLocaleTimeString(undefined, {
      hour: "2-digit",
    hourCycle: "h23",
      minute: "2-digit",
    })}`;
  }
  return "continues";
}

function dateLabel(date: string): string {
  return new Date(`${date}T00:00:00`).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}

function allDayInclusiveEnd(event: CommunityEvent): string | null {
  if (event.dtstart?.kind !== "date") return null;
  if (event.dtend?.kind !== "date") return event.dtstart.date;
  return addDaysToDateString(event.dtend.date, -1);
}

function monthPillLabel(event: CommunityEvent, dayStartMs: number | null): string {
  if (dayStartMs === null || event.dtstart?.kind !== "date-time") return event.summary;
  const startDay = localDateStringFromMs(event.dtstart.ms);
  const cellDay = localDateStringFromMs(dayStartMs);
  if (startDay !== cellDay) return `continues ${event.summary}`;
  return `${formatTime(event)} ${event.summary}`;
}

function summarizeRrule(rule: Rrule): string {
  const parts: string[] = [];
  const interval = rule.interval ?? 1;
  const freqLabel = ({
    DAILY: "day",
    WEEKLY: "week",
    MONTHLY: "month",
    YEARLY: "year",
  } as const)[rule.freq];
  parts.push(interval === 1 ? `Every ${freqLabel}` : `Every ${interval} ${freqLabel}s`);
  if (rule.byDay && rule.byDay.length > 0) {
    parts.push(`on ${rule.byDay.map((d) => weekdayLabels[d]).join(", ")}`);
  }
  if (typeof rule.count === "number") {
    parts.push(`${rule.count} times`);
  } else if (typeof rule.untilMs === "number") {
    parts.push(`until ${new Date(rule.untilMs).toLocaleDateString()}`);
  }
  return parts.join(" · ");
}

function authorLabel(jid: string | undefined): string {
  if (!jid) return "Unknown";
  const stripped = jid.startsWith("xmpp:") ? jid.slice(5) : jid;
  return jidLocalpart(stripped);
}

const planningRoomRequest = ref<{ jid: string; version: number } | null>(null);
const planningPanel = ref<HTMLElement | null>(null);
const pendingWorkspaceLinks = ref<Record<string, string>>({});
const planningError = ref<string | null>(null);
const planningBusy = ref(false);
function openPlanning(room: string) {
  planningRoomRequest.value = { jid: room, version: (planningRoomRequest.value?.version ?? 0) + 1 };
  planningPanel.value?.scrollIntoView({ behavior: "smooth", block: "start" });
}
async function createPlanning(event: CommunityEvent) {
  const client = props.client; const community = props.communityJid;
  if (!client || !community || planningBusy.value) return;
  planningBusy.value = true; planningError.value = null;
  const master = props.findMaster(event.uid) ?? event;
  try {
    let room = master.workspaceRoom || pendingWorkspaceLinks.value[master.uid];
    if (!room) {
      room = await client.createEventRoom(master.summary, true);
      pendingWorkspaceLinks.value[master.uid] = room;
    }
    await client.publishEventPlanning(room, {
      ...newPlanningItem("event", master.summary, "event"),
      description: master.description, location: master.location, timezone: "UTC",
      starts_at: master.dtstart?.kind === "date-time" ? new Date(master.dtstart.ms).toISOString() : master.dtstart?.kind === "date" ? `${master.dtstart.date}T00:00:00Z` : null,
      ends_at: master.dtend?.kind === "date-time" ? new Date(master.dtend.ms).toISOString() : master.dtend?.kind === "date" ? `${master.dtend.date}T00:00:00Z` : null,
      status: "proposed",
    });
    await client.updateCommunityEvent(community, master.id, {
      summary: master.summary, description: master.description, location: master.location,
      organizer: master.organizer, dtstart: master.dtstart, dtend: master.dtend,
      rrule: master.rrule, exdates: master.exdates, workspaceRoom: room,
    });
    openPlanning(room); emit("refresh");
  } catch (cause) { planningError.value = cause instanceof Error ? cause.message : String(cause); }
  finally { planningBusy.value = false; }
}

// ─── Composer ─────────────────────────────────────────────────────────────────

const composerOpen = ref(false);
const editingId = ref<string | null>(null);
const summary = ref("");
const description = ref("");
const location = ref("");
const workspaceRoom = ref<string | null>(null);
const dtstart = ref("");
const dtend = ref("");
const allDay = ref(false);
const allDayStart = ref("");
const allDayEnd = ref("");
const durationChoice = ref("30");
const rrule = ref<Rrule | null>(null);
const composerTouched = ref(false);
const composerSubmitted = ref(false);

const DURATION_OPTIONS = [
  { value: "30", label: "30 minutes" },
  { value: "60", label: "1 hour" },
  { value: "90", label: "90 minutes" },
  { value: "120", label: "2 hours" },
  { value: "180", label: "3 hours" },
  { value: "custom", label: "Custom" },
] as const;

const composerError = computed(() => {
  if (summary.value.trim().length === 0) return "Add an event title.";
  if (allDay.value) {
    if (!allDayStart.value) return "Choose a start date.";
    const visibleEnd = allDayEnd.value || allDayStart.value;
    const endExclusive = addDaysToDateString(visibleEnd, 1);
    if (calendarDateStartMs(dateValue(endExclusive)) <= calendarDateStartMs(dateValue(allDayStart.value))) {
      return "End date must be after the start date.";
    }
    if (typeof rrule.value?.untilMs === "number") {
      return "All-day repeats need a count instead of an until date.";
    }
    return null;
  }
  if (!dtstart.value) return "Choose a start time.";
  const startMs = Date.parse(dtstart.value);
  const endMs = timedEndMs();
  if (!Number.isFinite(startMs) || !Number.isFinite(endMs)) return "Choose a valid time.";
  if (endMs <= startMs) return "End time must be after the start time.";
  return null;
});

const canSubmit = computed(
  () => props.canPost && !props.isPosting && composerError.value === null,
);

const visibleComposerError = computed(() =>
  composerTouched.value || composerSubmitted.value ? composerError.value : null,
);

const calendarFeedCopy = useCalendarFeedCopy({
  communityJid: () => props.communityJid,
  serverBaseUrl: () => props.serverBaseUrl,
  sessionId: () => props.sessionId,
});
const canCopyFeedUrl = calendarFeedCopy.canCopy;
const feedCopyState = calendarFeedCopy.state;
const feedCopyStatusLabel = calendarFeedCopy.statusLabel;
const feedCopyUrl = calendarFeedCopy.url;

onUnmounted(calendarFeedCopy.dispose);

/** Format an epoch-ms timestamp for the `<input type="datetime-local">` widget. */
function toDatetimeLocal(ms: number | undefined): string {
  if (typeof ms !== "number") return "";
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function openComposer() {
  resetComposerFields();
  composerOpen.value = true;
}

function markComposerTouched() {
  composerTouched.value = true;
}

function timedEndMs(): number {
  const startMs = Date.parse(dtstart.value);
  if (durationChoice.value === "custom") return Date.parse(dtend.value);
  const durationMinutes = Number(durationChoice.value);
  return Number.isFinite(startMs) && Number.isFinite(durationMinutes)
    ? startMs + durationMinutes * 60_000
    : Number.NaN;
}

function selectDurationForBounds(start: CalendarDateValue | undefined, end: CalendarDateValue | undefined) {
  if (start?.kind !== "date-time" || end?.kind !== "date-time") {
    durationChoice.value = "30";
    return;
  }
  const minutes = Math.round((end.ms - start.ms) / 60_000);
  const match = DURATION_OPTIONS.find((option) => option.value === String(minutes));
  durationChoice.value = match ? match.value : "custom";
}

/**
 * Edits always target the unexpanded master so the RRULE / EXDATEs
 * are preserved and the resulting publish lands at the real pubsub
 * item id. An expanded instance has a synthetic id (`<master>::<ts>`)
 * which doesn't exist on the server.
 */
function startEdit(event: CommunityEvent) {
  const master = props.findMaster(event.uid) ?? event;
  editingId.value = master.id;
  summary.value = master.summary;
  description.value = master.description ?? "";
  location.value = master.location ?? "";
  workspaceRoom.value = master.workspaceRoom ?? null;
  if (master.dtstart?.kind === "date") {
    allDay.value = true;
    allDayStart.value = master.dtstart.date;
    allDayEnd.value = allDayInclusiveEnd(master) ?? master.dtstart.date;
    dtstart.value = "";
    dtend.value = "";
  } else {
    allDay.value = false;
    dtstart.value = toDatetimeLocal(master.dtstart?.kind === "date-time" ? master.dtstart.ms : undefined);
    dtend.value = toDatetimeLocal(master.dtend?.kind === "date-time" ? master.dtend.ms : undefined);
    selectDurationForBounds(master.dtstart, master.dtend);
    const allDayDefault = master.dtstart?.kind === "date-time"
      ? localDateStringFromMs(master.dtstart.ms)
      : todayDateString();
    allDayStart.value = allDayDefault;
    allDayEnd.value = allDayDefault;
  }
  rrule.value = master.rrule ?? null;
  composerTouched.value = false;
  composerSubmitted.value = false;
  composerOpen.value = true;
}

// ── Cancel action sheet ──────────────────────────────────────────────────────
// Tracks which event the user just clicked Trash on. For recurring
// events we pop a small action sheet to choose just-this vs entire-
// series; for one-off events a plain confirm() is enough.
const cancelTarget = ref<CommunityEvent | null>(null);

function onCancelEvent(event: CommunityEvent) {
  const master = props.findMaster(event.uid);
  if (master?.rrule) {
    cancelTarget.value = event;
    return;
  }
  const ok = window.confirm(
    `Cancel "${event.summary}"? This removes the event for everyone.`,
  );
  if (!ok) return;
  emit("cancelSeries", (master ?? event).id);
}

function confirmCancelInstance() {
  const event = cancelTarget.value;
  if (!event || !event.dtstart) return;
  emit("cancelInstance", event.uid, event.dtstart);
  cancelTarget.value = null;
}

function confirmCancelSeries() {
  const event = cancelTarget.value;
  if (!event) return;
  const master = props.findMaster(event.uid) ?? event;
  emit("cancelSeries", master.id);
  cancelTarget.value = null;
}

function dismissCancelSheet() {
  cancelTarget.value = null;
}

function submit() {
  composerSubmitted.value = true;
  const summaryValue = summary.value.trim();
  if (!summaryValue || composerError.value) return;
  const dateFields = buildDateInput();
  if (!dateFields) return;
  const input: CommunityEventInput = {
    summary: summaryValue,
    ...(workspaceRoom.value ? { workspaceRoom: workspaceRoom.value } : {}),
    ...(description.value.trim() ? { description: description.value.trim() } : {}),
    ...(location.value.trim() ? { location: location.value.trim() } : {}),
    ...(props.selfJid
      ? { organizer: `xmpp:${barePeerJid(props.selfJid)}` }
      : {}),
    ...dateFields,
    ...(rrule.value ? { rrule: rrule.value } : {}),
  };
  if (editingId.value) {
    emit("edit", editingId.value, input);
  } else {
    emit("post", input);
  }
  resetComposer();
}

function resetComposer() {
  composerOpen.value = false;
  editingId.value = null;
  resetComposerFields();
}

function resetComposerFields() {
  summary.value = "";
  description.value = "";
  location.value = "";
  workspaceRoom.value = null;
  dtstart.value = "";
  dtend.value = "";
  allDay.value = false;
  setDefaultAllDayDates();
  durationChoice.value = "30";
  rrule.value = null;
  composerTouched.value = false;
  composerSubmitted.value = false;
}

function setDefaultAllDayDates() {
  const today = todayDateString();
  allDayStart.value = today;
  allDayEnd.value = today;
}

function todayDateString(): string {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
}

function buildDateInput(): Pick<CommunityEventInput, "dtstart" | "dtend"> | null {
  if (allDay.value) {
    if (!allDayStart.value) return null;
    const visibleEnd = allDayEnd.value || allDayStart.value;
    return {
      dtstart: dateValue(allDayStart.value),
      dtend: dateValue(addDaysToDateString(visibleEnd, 1)),
    };
  }
  const startMs = Date.parse(dtstart.value);
  const endMs = timedEndMs();
  if (!Number.isFinite(startMs) || !Number.isFinite(endMs)) return null;
  return {
    dtstart: dateTimeValue(startMs),
    dtend: dateTimeValue(endMs),
  };
}

const copyCalendarFeedUrl = calendarFeedCopy.copy;

const DOW_LABELS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

// ─── Page sections ───────────────────────────────────────────────────────────
// Every event renders through the one card in the template; sections say
// which heading and kicker it gets. The month grid is a section too so the
// DOM order matches the visual order (next up, grid, selected day).

interface CardSection {
  kind: "cards";
  key: string;
  heading: string;
  headingClass: string;
  events: CommunityEvent[];
  kicker: (event: CommunityEvent) => string;
}
type PageSection = CardSection | { kind: "grid"; key: "grid" };

const sections = computed<PageSection[]>(() => {
  if (viewMode.value === "week") {
    return [...weekEventsByDay.value].map(([dayLabel, day]) => ({
      kind: "cards",
      key: dayLabel,
      heading: dayLabel,
      headingClass: kickerClass,
      events: day.events,
      kicker: (event) => eventKicker(event, day.startMs),
    }));
  }
  const list: PageSection[] = [];
  const next = nextEvent.value;
  if (next) {
    list.push({
      kind: "cards",
      key: "next",
      heading: "Next up",
      headingClass: kickerClass,
      events: [next],
      kicker: formatStart,
    });
  }
  list.push({ kind: "grid", key: "grid" });
  if (selectedDay.value !== null && selectedDayEvents.value.length > 0) {
    list.push({
      kind: "cards",
      key: "selected",
      heading: selectedDayLabel.value,
      headingClass: ui.dayTitle,
      events: selectedDayEvents.value,
      kicker: (event) => eventKicker(event, selectedDayStartMs.value),
    });
  }
  return list;
});

const RSVP_CHOICES: { partstat: PartStat; label: string }[] = [
  { partstat: "ACCEPTED", label: "Going" },
  { partstat: "TENTATIVE", label: "Maybe" },
  { partstat: "DECLINED", label: "Can't" },
];

function isMine(event: CommunityEvent): boolean {
  const mine = myPartstat(event);
  return mine === "ACCEPTED" || mine === "TENTATIVE";
}
</script>

<template>
  <div :class="ui.root">
    <!-- Phone header: the rail bar with the sidebar button (Mobile-Events board). -->
    <header :class="ui.mobileBar" data-theme="night">
      <button
        type="button"
        :class="ui.mobileNav"
        aria-label="Open navigation"
        @click="emit('openNav')"
      >
        <Menu aria-hidden="true" />
      </button>
      <b :class="ui.mobileTitle">Calendar</b>
      <button
        v-if="canPost"
        type="button"
        :class="primarySmButtonClass"
        @click="composerOpen ? resetComposer() : openComposer()"
      >
        {{ composerOpen ? (editingId ? "Cancel edit" : "Close") : "New event" }}
      </button>
      <button
        v-else
        type="button"
        :class="iconButtonClass"
        aria-label="Refresh"
        :disabled="isLoading"
        @click="emit('refresh')"
      >
        <RefreshCw :class="{ [ui.spin]: isLoading }" aria-hidden="true" />
      </button>
    </header>

    <div :class="ui.scroll">
      <div :class="ui.content">
        <div v-if="client" ref="planningPanel"><EventPlanningPanel :client="client" :self-jid="selfJid" :requested-room="planningRoomRequest" @open-conversation="(roomJid) => emit('openEventConversation', roomJid)" /></div>
        <p v-if="planningError" role="alert">{{ planningError }}</p>
        <div :class="ui.head">
          <h1 :class="ui.h1">Calendar</h1>
          <button
            v-if="canPost"
            type="button"
            :class="primaryButtonClass"
            @click="composerOpen ? resetComposer() : openComposer()"
          >
            <Plus v-if="!composerOpen" aria-hidden="true" />
            {{ composerOpen ? (editingId ? "Cancel edit" : "Close") : "New event" }}
          </button>
          <button
            v-else
            type="button"
            :class="quietButtonLgClass"
            :disabled="isLoading"
            @click="emit('refresh')"
          >
            <RefreshCw :class="{ [ui.spin]: isLoading }" aria-hidden="true" />
            Refresh
          </button>
        </div>

        <div v-if="!composerOpen" :class="ui.toolbar">
          <div :class="segClass" role="group" aria-label="Calendar view">
            <button
              type="button"
              :aria-pressed="viewMode === 'week' ? 'true' : 'false'"
              @click="viewMode = 'week'"
            >
              <List aria-hidden="true" />
              7 days
            </button>
            <button
              type="button"
              :aria-pressed="viewMode === 'month' ? 'true' : 'false'"
              @click="viewMode = 'month'"
            >
              <CalendarDays aria-hidden="true" />
              Month
            </button>
          </div>

          <button
            v-if="selfBareJid"
            type="button"
            :class="chipClass"
            :aria-pressed="activeFilter === 'attending' ? 'true' : 'false'"
            @click="activeFilter = activeFilter === 'attending' ? 'all' : 'attending'"
          >
            Going or maybe
          </button>

          <div :class="ui.feedTools">
            <button
              type="button"
              :class="quietButtonClass"
              :disabled="!canCopyFeedUrl || feedCopyState === 'loading'"
              @click="copyCalendarFeedUrl"
            >
              <Copy aria-hidden="true" />
              Copy feed URL
            </button>
            <span :class="ui.status" role="status" aria-live="polite">
              {{ feedCopyStatusLabel }}
            </span>
          </div>
        </div>

        <CalendarFeedUrlPanel v-if="feedCopyUrl && !composerOpen" :url="feedCopyUrl" />

        <div v-if="error" :class="ui.error" role="alert">
          Couldn't load events: {{ error }}
        </div>

        <!-- Composer (Events-Form board): replaces the calendar while open. -->
        <template v-if="composerOpen">
          <h2 :id="formTitleId" :class="ui.formTitle">{{ editingId ? "Edit event" : "New event" }}</h2>
          <form
            :class="ui.form"
            :aria-labelledby="formTitleId"
            @input="markComposerTouched"
            @submit.prevent="submit"
          >
            <input
              v-model="summary"
              type="text"
              :class="ui.titleInput"
              placeholder="Event title"
              required
              aria-label="Event title"
              :aria-invalid="visibleComposerError === 'Add an event title.' ? 'true' : undefined"
            />
            <label :class="ui.allDay">
              <input v-model="allDay" type="checkbox" />
              All day
            </label>
            <div v-if="allDay" :class="ui.pair">
              <label :class="ui.field">
                Start date
                <input v-model="allDayStart" type="date" :class="ui.fieldInput" />
              </label>
              <label :class="ui.field">
                End date
                <input v-model="allDayEnd" type="date" :class="ui.fieldInput" />
              </label>
            </div>
            <div v-else :class="ui.pair">
              <label :class="ui.field">
                Starts
                <input v-model="dtstart" type="datetime-local" :class="ui.fieldInput" />
              </label>
              <label :class="ui.field">
                Duration
                <select v-model="durationChoice" :class="ui.fieldInput">
                  <option
                    v-for="option in DURATION_OPTIONS"
                    :key="option.value"
                    :value="option.value"
                  >
                    {{ option.label }}
                  </option>
                </select>
              </label>
              <label v-if="durationChoice === 'custom'" :class="[ui.field, ui.span2]">
                Ends
                <input v-model="dtend" type="datetime-local" :class="ui.fieldInput" />
              </label>
            </div>
            <label :class="ui.field">
              Location (optional)
              <input
                v-model="location"
                type="text"
                :class="ui.fieldInput"
                placeholder="Room, address or link"
                aria-label="Event location"
              />
            </label>
            <label :class="ui.field">
              Description (optional)
              <textarea
                v-model="description"
                :class="ui.textarea"
                rows="3"
                aria-label="Event description"
              />
            </label>
            <RecurrencePicker v-model="rrule" />
            <p v-if="visibleComposerError" :class="ui.alert" role="alert">
              <Clock3 aria-hidden="true" />
              {{ visibleComposerError }}
            </p>
            <div :class="ui.formFoot">
              <button
                type="button"
                :class="quietButtonLgClass"
                :disabled="isPosting"
                @click="resetComposer"
              >
                Cancel
              </button>
              <button type="submit" :class="primaryButtonClass" :disabled="!canSubmit">
                {{ isPosting ? (editingId ? "Saving…" : "Publishing…") : (editingId ? "Save changes" : "Publish event") }}
              </button>
            </div>
          </form>
        </template>

        <template v-else>
          <template v-for="section in sections" :key="section.key">
            <!-- Month grid (Events-Calendar board) -->
            <section v-if="section.kind === 'grid'" :class="ui.section" :aria-label="calTitle">
              <div :class="ui.monthNav">
                <button
                  type="button"
                  :class="iconButtonClass"
                  aria-label="Previous month"
                  @click="prevMonth"
                >
                  <ChevronLeft aria-hidden="true" />
                </button>
                <h2 :class="ui.monthTitle">{{ calTitle }}</h2>
                <button
                  type="button"
                  :class="iconButtonClass"
                  aria-label="Next month"
                  @click="nextMonth"
                >
                  <ChevronRight aria-hidden="true" />
                </button>
                <button
                  type="button"
                  :class="ui.todayButton"
                  aria-label="Jump to today"
                  @click="goToToday"
                >
                  Today
                </button>
              </div>
              <div :class="ui.grid">
                <span v-for="label in DOW_LABELS" :key="label" :class="ui.dow">{{ label }}</span>
                <template v-for="(week, wi) in calGrid" :key="wi">
                  <button
                    v-for="(cell, di) in week"
                    :key="di"
                    type="button"
                    :class="ui.cell"
                    :data-today="cell.isToday ? '' : undefined"
                    :disabled="cell.day === null"
                    :aria-pressed="cell.day !== null ? (cell.day === selectedDay ? 'true' : 'false') : undefined"
                    :aria-label="cell.day !== null ? `${cell.day}${cell.isToday ? ', today' : ''}, ${cell.events.length} ${cell.events.length === 1 ? 'event' : 'events'}` : undefined"
                    @click="cell.day !== null && (selectedDay = cell.day)"
                  >
                    <span v-if="cell.day !== null" :class="ui.cellNum">{{ cell.day }}</span>
                    <span
                      v-for="ev in cell.events.slice(0, 3)"
                      :key="ev.id"
                      :class="ui.pill"
                      :data-mine="isMine(ev) ? '' : undefined"
                    >{{ monthPillLabel(ev, cell.rangeStartMs) }}</span>
                    <span v-if="cell.events.length > 3" :class="ui.more">
                      +{{ cell.events.length - 3 }} more
                    </span>
                    <span v-if="cell.events.length > 0" :class="ui.dot" aria-hidden="true" />
                  </button>
                </template>
              </div>
              <p
                v-if="selectedDay !== null && selectedDayEvents.length === 0"
                :class="ui.dayEmpty"
              >
                Nothing on this day. A good one to plan something.
              </p>
            </section>

            <!-- Day / next-up sections: the one event card (EventCard board) -->
            <section v-else :class="ui.section" :aria-label="section.heading">
              <h2 :class="section.headingClass">{{ section.heading }}</h2>
              <article
                v-for="event in section.events"
                :key="event.id"
                :class="ui.card"
                :data-live="isEventLive(event) ? '' : undefined"
                :aria-label="event.summary"
              >
                <div :class="ui.cardHead">
                  <div :class="ui.cardMain">
                    <p :class="ui.cardKicker">{{ section.kicker(event) }}</p>
                    <h3 :class="ui.cardTitle">{{ event.summary }}</h3>
                    <span v-if="event.location || event.organizer" :class="ui.cardMeta">
                      <MapPin v-if="event.location" aria-hidden="true" />
                      {{ [event.location, event.organizer ? `Hosted by ${authorLabel(event.organizer)}` : null].filter(Boolean).join(" · ") }}
                    </span>
                  </div>
                  <template v-if="isOrganiser(event)">
                    <button
                      type="button"
                      :class="iconButtonClass"
                      aria-label="Edit event"
                      @click="startEdit(event)"
                    >
                      <Pencil aria-hidden="true" />
                    </button>
                    <button
                      type="button"
                      :class="iconButtonClass"
                      aria-label="Cancel event"
                      @click="onCancelEvent(event)"
                    >
                      <Trash2 aria-hidden="true" />
                    </button>
                  </template>
                </div>
                <button v-if="client && event.workspaceRoom" type="button" :class="quietButtonClass" @click="openPlanning(event.workspaceRoom)">Open planning space</button>
                <button v-else-if="client && isOrganiser(event)" type="button" :class="quietButtonClass" :disabled="planningBusy" @click="createPlanning(event)">{{ planningBusy ? 'Creating…' : 'Add a private planning space' }}</button>
                <p v-if="event.description" :class="ui.cardDesc">{{ event.description }}</p>
                <span v-if="event.rrule" :class="ui.cardRepeat">
                  <Repeat aria-hidden="true" />
                  {{ summarizeRrule(event.rrule) }}
                </span>
                <div v-if="selfBareJid" :class="ui.cardFoot">
                  <div :class="segClass" role="group" aria-label="RSVP">
                    <button
                      v-for="choice in RSVP_CHOICES"
                      :key="choice.partstat"
                      type="button"
                      :aria-pressed="myPartstat(event) === choice.partstat ? 'true' : 'false'"
                      @click="onRsvp(event, choice.partstat)"
                    >{{ choice.label }}</button>
                  </div>
                  <span :class="ui.counts">
                    {{ attendeesByPartstat(event)["ACCEPTED"].length }} going ·
                    {{ attendeesByPartstat(event)["TENTATIVE"].length }} maybe ·
                    {{ attendeesByPartstat(event)["DECLINED"].length }} can't
                  </span>
                </div>
              </article>
            </section>
          </template>

          <div v-if="viewMode === 'week' && sections.length === 0 && !isLoading" :class="ui.empty">
            <img :src="spotUrl" alt="" />
            <h2>
              {{ activeFilter === 'attending' ? "Nothing you're going to this week." : "Nothing scheduled this week." }}
            </h2>
            <p>
              {{ activeFilter === 'attending'
                ? "Say yes to something, or schedule one yourself."
                : canPost ? "Schedule one to get the community together." : "Check back later." }}
            </p>
            <button v-if="canPost" type="button" :class="primaryButtonClass" @click="openComposer">
              New event
            </button>
          </div>
        </template>
      </div>
    </div>

    <!-- Cancel sheet (recurring events only): a bottom sheet on phones, a dialog from md. -->
    <div v-if="cancelTarget" :class="ui.sheetPositioner">
      <div :class="scrimClass" aria-hidden="true" @click="dismissCancelSheet" />
      <div
        :class="ui.sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="events-cancel-title"
      >
        <span :class="ui.grabber" aria-hidden="true" />
        <h2 id="events-cancel-title" :class="ui.sheetTitle">
          Cancel “{{ cancelTarget.summary }}”
        </h2>
        <p :class="ui.sheetBody">This event repeats. Choose what to cancel:</p>
        <button type="button" :class="ui.option" @click="confirmCancelInstance">
          <span>Just this occurrence</span>
          <span>{{ formatStart(cancelTarget) }}</span>
        </button>
        <button type="button" :class="[ui.option, ui.optionDanger]" @click="confirmCancelSeries">
          <span>The entire series</span>
          <span>All future occurrences</span>
        </button>
        <div :class="ui.sheetFoot">
          <button type="button" :class="quietButtonLgClass" @click="dismissCancelSheet">
            Keep event
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
