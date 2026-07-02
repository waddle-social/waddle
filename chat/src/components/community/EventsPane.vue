<script setup lang="ts">
import { computed, onUnmounted, ref } from "vue";
import {
  CalendarDays,
  ChevronLeft,
  ChevronRight,
  Clock3,
  Copy,
  List,
  Menu,
  Pencil,
  Plus,
  RefreshCw,
  Repeat,
  Trash2,
  X,
} from "lucide-vue-next";
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

interface EventsPaneProps {
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
  const firstDow = new Date(y, m, 1).getDay(); // 0 = Sun
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
    hour: "numeric",
    minute: "2-digit",
  });
  if (event.dtend?.kind !== "date-time") return start;
  return `${start} - ${new Date(event.dtend.ms).toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  })}`;
}

function formatTime(event: CommunityEvent): string {
  if (!event.dtstart) return "TBD";
  if (event.dtstart.kind === "date") return "All day";
  return new Date(event.dtstart.ms).toLocaleTimeString(undefined, {
    hour: "numeric",
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
      hour: "numeric",
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

// ─── Composer ─────────────────────────────────────────────────────────────────

const composerOpen = ref(false);
const editingId = ref<string | null>(null);
const summary = ref("");
const description = ref("");
const location = ref("");
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

const DOW_LABELS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
</script>

<template>
  <div class="chat-pane-scroll flex-1 min-h-0 bg-background px-[var(--chat-content-inline)] py-6">
    <div class="mx-auto grid w-full max-w-3xl gap-4">

      <!-- Header -->
      <header class="flex flex-wrap items-center gap-2">
        <button
          type="button"
          class="md:hidden inline-flex h-8 w-8 items-center justify-center rounded-md border border-transparent text-muted-foreground hover:bg-muted/50 hover:text-foreground"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Menu class="h-4 w-4" aria-hidden="true" />
        </button>

        <CalendarDays class="h-5 w-5 text-primary" aria-hidden="true" />
        <h1 class="type-pane-title text-foreground">Community Events</h1>

        <!-- View mode toggle -->
        <div class="ml-auto flex items-center rounded-md border border-border bg-muted/40 p-0.5 text-xs">
          <button
            type="button"
            class="inline-flex items-center gap-1 rounded px-2.5 py-1 font-medium transition-colors"
            :class="viewMode === 'week'
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground'"
            @click="viewMode = 'week'"
          >
            <List class="h-3.5 w-3.5" aria-hidden="true" />
            7 days
          </button>
          <button
            type="button"
            class="inline-flex items-center gap-1 rounded px-2.5 py-1 font-medium transition-colors"
            :class="viewMode === 'month'
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground'"
            @click="viewMode = 'month'"
          >
            <CalendarDays class="h-3.5 w-3.5" aria-hidden="true" />
            Month
          </button>
        </div>

        <!-- Attending filter -->
        <button
          v-if="selfBareJid"
          type="button"
          class="inline-flex items-center rounded-full border px-2.5 py-1 text-xs font-medium transition-colors"
          :class="activeFilter === 'attending'
            ? 'border-primary bg-primary/10 text-primary'
            : 'border-border text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
          @click="activeFilter = activeFilter === 'attending' ? 'all' : 'attending'"
        >
          {{ activeFilter === 'attending' ? 'Going / Maybe' : 'All events' }}
        </button>

        <!-- Calendar feed -->
        <button
          type="button"
          class="inline-flex min-h-11 min-w-[7.5rem] items-center justify-center gap-1 rounded-md border border-transparent px-3 py-2 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-muted-foreground sm:px-2 sm:py-1"
          :disabled="!canCopyFeedUrl || feedCopyState === 'loading'"
          @click="copyCalendarFeedUrl"
        >
          <Copy class="h-3.5 w-3.5" aria-hidden="true" />
          Copy feed URL
        </button>
        <span
          class="min-w-[5.5rem] text-xs text-muted-foreground"
          role="status"
          aria-live="polite"
        >
          {{ feedCopyStatusLabel }}
        </span>

        <!-- New event / Refresh -->
        <button
          v-if="canPost"
          type="button"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90"
          @click="composerOpen ? resetComposer() : openComposer()"
        >
          <Plus class="h-3.5 w-3.5" aria-hidden="true" />
          {{ composerOpen ? (editingId ? "Cancel edit" : "Close") : "New event" }}
        </button>
        <button
          v-else
          type="button"
          class="inline-flex items-center gap-1 rounded-md border border-transparent px-2 py-1 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
          :disabled="isLoading"
          @click="emit('refresh')"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': isLoading }" aria-hidden="true" />
          Refresh
        </button>
      </header>

      <CalendarFeedUrlPanel v-if="feedCopyUrl" :url="feedCopyUrl" />

      <!-- Error -->
      <div
        v-if="error"
        class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
      >
        Couldn't load events: {{ error }}
      </div>

      <!-- Composer -->
      <form
        v-if="composerOpen"
        class="grid gap-2 rounded-lg border border-border bg-card p-3"
        @input="markComposerTouched"
        @submit.prevent="submit"
      >
        <input
          v-model="summary"
          type="text"
          class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          placeholder="Event title"
          required
          aria-label="Event title"
        />
        <label class="inline-flex w-fit items-center gap-2 rounded-md border border-input bg-background px-3 py-2 text-xs text-foreground">
          <input
            v-model="allDay"
            type="checkbox"
            class="h-4 w-4 rounded border-input"
          />
          All day
        </label>
        <div v-if="allDay" class="grid gap-2 md:grid-cols-2">
          <label class="grid gap-1 text-xs">
            <span class="text-muted-foreground">Start date</span>
            <input
              v-model="allDayStart"
              type="date"
              class="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
            />
          </label>
          <label class="grid gap-1 text-xs">
            <span class="text-muted-foreground">End date</span>
            <input
              v-model="allDayEnd"
              type="date"
              class="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
            />
          </label>
        </div>
        <div v-else class="grid gap-2 md:grid-cols-2">
          <label class="grid gap-1 text-xs">
            <span class="text-muted-foreground">Starts</span>
            <input
              v-model="dtstart"
              type="datetime-local"
              class="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
            />
          </label>
          <label class="grid gap-1 text-xs">
            <span class="text-muted-foreground">Duration</span>
            <select
              v-model="durationChoice"
              class="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
            >
              <option
                v-for="option in DURATION_OPTIONS"
                :key="option.value"
                :value="option.value"
              >
                {{ option.label }}
              </option>
            </select>
          </label>
          <label v-if="durationChoice === 'custom'" class="grid gap-1 text-xs md:col-span-2">
            <span class="text-muted-foreground">Ends</span>
            <input
              v-model="dtend"
              type="datetime-local"
              class="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
            />
          </label>
        </div>
        <input
          v-model="location"
          type="text"
          class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          placeholder="Location (optional)"
          aria-label="Event location"
        />
        <textarea
          v-model="description"
          class="min-h-[3rem] w-full resize-y rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          placeholder="Description (optional)"
          aria-label="Event description"
        />
        <RecurrencePicker v-model="rrule" />
        <p
          v-if="visibleComposerError"
          class="inline-flex items-center gap-1 text-xs text-destructive"
          role="alert"
        >
          <Clock3 class="h-3.5 w-3.5" aria-hidden="true" />
          {{ visibleComposerError }}
        </p>
        <div class="flex items-center justify-end gap-2">
          <button
            type="button"
            class="inline-flex items-center gap-1 rounded-md border border-input px-3 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
            :disabled="isPosting"
            @click="resetComposer"
          >
            <X class="h-3.5 w-3.5" aria-hidden="true" />
            Cancel
          </button>
          <button
            type="submit"
            class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
            :disabled="!canSubmit"
          >
            {{ isPosting ? (editingId ? "Saving…" : "Publishing…") : (editingId ? "Save changes" : "Publish event") }}
          </button>
        </div>
      </form>

      <!-- ── 7-day list view ─────────────────────────────────────────────── -->
      <template v-if="viewMode === 'week'">
        <div v-if="weekEventsByDay.size > 0" class="grid gap-4">
          <section
            v-for="[dayLabel, day] in weekEventsByDay"
            :key="dayLabel"
            class="grid gap-2"
          >
            <h2 class="type-section-label text-muted-foreground">{{ dayLabel }}</h2>
            <article
              v-for="event in day.events"
              :key="event.id"
              class="grid gap-1.5 rounded-lg border border-border bg-card px-4 py-3"
            >
              <header class="flex items-baseline justify-between gap-3">
                <h3 class="type-control font-semibold text-foreground">{{ event.summary }}</h3>
                <div class="flex shrink-0 items-center gap-1">
                  <span class="type-caption text-muted-foreground">
                    {{ formatVisibleTime(event, day.startMs) }}
                  </span>
                  <template v-if="isOrganiser(event)">
                    <button
                      type="button"
                      class="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted/50 hover:text-foreground"
                      aria-label="Edit event"
                      @click="startEdit(event)"
                    >
                      <Pencil class="h-3.5 w-3.5" aria-hidden="true" />
                    </button>
                    <button
                      type="button"
                      class="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                      aria-label="Cancel event"
                      @click="onCancelEvent(event)"
                    >
                      <Trash2 class="h-3.5 w-3.5" aria-hidden="true" />
                    </button>
                  </template>
                </div>
              </header>
              <p v-if="event.location" class="type-caption text-muted-foreground">
                📍 {{ event.location }}
              </p>
              <p v-if="event.description" class="whitespace-pre-wrap break-words text-sm text-foreground">
                {{ event.description }}
              </p>
              <p v-if="event.rrule" class="type-caption inline-flex items-center gap-1 text-primary">
                <Repeat class="h-3 w-3" aria-hidden="true" />
                {{ summarizeRrule(event.rrule) }}
              </p>
              <p v-if="event.organizer" class="type-caption text-muted-foreground">
                Organised by {{ authorLabel(event.organizer) }}
              </p>
              <div v-if="selfBareJid" class="mt-1 flex flex-wrap items-center gap-1.5">
                <button
                  type="button"
                  class="inline-flex items-center rounded-full border px-2.5 py-1 text-xs font-medium transition-colors"
                  :class="myPartstat(event) === 'ACCEPTED'
                    ? 'border-primary bg-primary text-primary-foreground'
                    : 'border-border bg-background text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
                  @click="onRsvp(event, 'ACCEPTED')"
                >Going</button>
                <button
                  type="button"
                  class="inline-flex items-center rounded-full border px-2.5 py-1 text-xs font-medium transition-colors"
                  :class="myPartstat(event) === 'TENTATIVE'
                    ? 'border-primary bg-primary text-primary-foreground'
                    : 'border-border bg-background text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
                  @click="onRsvp(event, 'TENTATIVE')"
                >Maybe</button>
                <button
                  type="button"
                  class="inline-flex items-center rounded-full border px-2.5 py-1 text-xs font-medium transition-colors"
                  :class="myPartstat(event) === 'DECLINED'
                    ? 'border-primary bg-primary text-primary-foreground'
                    : 'border-border bg-background text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
                  @click="onRsvp(event, 'DECLINED')"
                >Not going</button>
                <span class="type-caption ml-1 text-muted-foreground">
                  {{ attendeesByPartstat(event)["ACCEPTED"].length }} going ·
                  {{ attendeesByPartstat(event)["TENTATIVE"].length }} maybe ·
                  {{ attendeesByPartstat(event)["DECLINED"].length }} not going
                </span>
              </div>
            </article>
          </section>
        </div>

        <p
          v-else-if="!isLoading"
          class="type-caption rounded-lg border border-border px-4 py-6 text-center text-muted-foreground"
        >
          <template v-if="activeFilter === 'attending'">
            No events you're attending in the next 7 days.
          </template>
          <template v-else>
            Nothing scheduled in the next 7 days.
            {{ canPost ? "Schedule one to get the community together." : "Check back later." }}
          </template>
        </p>
      </template>

      <!-- ── Month calendar view ────────────────────────────────────────────── -->
      <template v-else>
        <!-- Month navigation -->
        <div class="flex items-center justify-between">
          <button
            type="button"
            class="inline-flex h-8 w-8 items-center justify-center rounded-md border border-border text-muted-foreground hover:bg-muted/50 hover:text-foreground"
            :aria-label="`Previous month`"
            @click="prevMonth"
          >
            <ChevronLeft class="h-4 w-4" aria-hidden="true" />
          </button>
          <div class="flex items-center gap-2">
            <span class="type-control font-semibold text-foreground">{{ calTitle }}</span>
            <button
              type="button"
              class="inline-flex items-center rounded-md border border-border bg-background px-2 py-1 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground"
              aria-label="Jump to today"
              @click="goToToday"
            >
              Today
            </button>
          </div>
          <button
            type="button"
            class="inline-flex h-8 w-8 items-center justify-center rounded-md border border-border text-muted-foreground hover:bg-muted/50 hover:text-foreground"
            :aria-label="`Next month`"
            @click="nextMonth"
          >
            <ChevronRight class="h-4 w-4" aria-hidden="true" />
          </button>
        </div>

        <!-- Calendar grid -->
        <div class="rounded-lg border border-border bg-card overflow-hidden">
          <!-- Day-of-week headers -->
          <div class="grid grid-cols-7 border-b border-border">
            <div
              v-for="label in DOW_LABELS"
              :key="label"
              class="py-1.5 text-center type-caption text-muted-foreground font-medium"
            >
              {{ label }}
            </div>
          </div>

          <!-- Weeks -->
          <div class="grid">
            <div
              v-for="(week, wi) in calGrid"
              :key="wi"
              class="grid grid-cols-7 divide-x divide-border"
              :class="{ 'border-t border-border': wi > 0 }"
            >
              <button
                v-for="(cell, di) in week"
                :key="di"
                type="button"
                class="min-h-[4rem] p-1.5 text-left align-top transition-colors"
                :class="[
                  cell.day === null ? 'bg-muted/20 cursor-default' : 'hover:bg-muted/30 cursor-pointer',
                  cell.day !== null && cell.day === selectedDay ? 'bg-primary/10' : '',
                ]"
                :disabled="cell.day === null"
                @click="cell.day !== null && (selectedDay = cell.day)"
              >
                <span
                  v-if="cell.day !== null"
                  class="inline-flex h-5 w-5 items-center justify-center rounded-full text-xs font-medium"
                  :class="cell.isToday
                    ? 'bg-primary text-primary-foreground'
                    : 'text-foreground'"
                >{{ cell.day }}</span>

                <!-- Event pills (up to 3) -->
                <div class="mt-1 grid gap-px">
                  <div
                    v-for="(ev, ei) in cell.events.slice(0, 3)"
                    :key="ev.id"
                    class="truncate rounded px-1 py-px text-[0.65rem] leading-tight"
                    :class="myPartstat(ev) === 'ACCEPTED' || myPartstat(ev) === 'TENTATIVE'
                      ? 'bg-primary/20 text-primary'
                      : 'bg-muted/60 text-muted-foreground'"
                  >
                    {{ monthPillLabel(ev, cell.rangeStartMs) }}
                  </div>
                  <div
                    v-if="cell.events.length > 3"
                    class="px-1 text-[0.65rem] text-muted-foreground"
                  >
                    +{{ cell.events.length - 3 }} more
                  </div>
                </div>
              </button>
            </div>
          </div>
        </div>

        <!-- Selected day event list -->
        <section v-if="selectedDay !== null && selectedDayEvents.length > 0" class="grid gap-2">
          <h2 class="type-section-label text-muted-foreground">
            {{ new Date(calYear, calMonth, selectedDay).toLocaleDateString(undefined, { weekday: 'long', month: 'long', day: 'numeric' }) }}
          </h2>
          <article
            v-for="event in selectedDayEvents"
            :key="event.id"
            class="grid gap-1.5 rounded-lg border border-border bg-card px-4 py-3"
          >
            <header class="flex items-baseline justify-between gap-3">
              <h3 class="type-control font-semibold text-foreground">{{ event.summary }}</h3>
              <div class="flex shrink-0 items-center gap-1">
                <span class="type-caption text-muted-foreground">{{ formatVisibleTime(event, selectedDayStartMs) }}</span>
                <template v-if="isOrganiser(event)">
                  <button
                    type="button"
                    class="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted/50 hover:text-foreground"
                    aria-label="Edit event"
                    @click="startEdit(event)"
                  >
                    <Pencil class="h-3.5 w-3.5" aria-hidden="true" />
                  </button>
                  <button
                    type="button"
                    class="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                    aria-label="Cancel event"
                    @click="onCancelEvent(event)"
                  >
                    <Trash2 class="h-3.5 w-3.5" aria-hidden="true" />
                  </button>
                </template>
              </div>
            </header>
            <p v-if="event.location" class="type-caption text-muted-foreground">
              📍 {{ event.location }}
            </p>
            <p v-if="event.description" class="whitespace-pre-wrap break-words text-sm text-foreground">
              {{ event.description }}
            </p>
            <p v-if="event.rrule" class="type-caption inline-flex items-center gap-1 text-primary">
              <Repeat class="h-3 w-3" aria-hidden="true" />
              {{ summarizeRrule(event.rrule) }}
            </p>
            <p v-if="event.organizer" class="type-caption text-muted-foreground">
              Organised by {{ authorLabel(event.organizer) }}
            </p>
            <div v-if="selfBareJid" class="mt-1 flex flex-wrap items-center gap-1.5">
              <button
                type="button"
                class="inline-flex items-center rounded-full border px-2.5 py-1 text-xs font-medium transition-colors"
                :class="myPartstat(event) === 'ACCEPTED'
                  ? 'border-primary bg-primary text-primary-foreground'
                  : 'border-border bg-background text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
                @click="onRsvp(event, 'ACCEPTED')"
              >Going</button>
              <button
                type="button"
                class="inline-flex items-center rounded-full border px-2.5 py-1 text-xs font-medium transition-colors"
                :class="myPartstat(event) === 'TENTATIVE'
                  ? 'border-primary bg-primary text-primary-foreground'
                  : 'border-border bg-background text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
                @click="onRsvp(event, 'TENTATIVE')"
              >Maybe</button>
              <button
                type="button"
                class="inline-flex items-center rounded-full border px-2.5 py-1 text-xs font-medium transition-colors"
                :class="myPartstat(event) === 'DECLINED'
                  ? 'border-primary bg-primary text-primary-foreground'
                  : 'border-border bg-background text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
                @click="onRsvp(event, 'DECLINED')"
              >Not going</button>
              <span class="type-caption ml-1 text-muted-foreground">
                {{ attendeesByPartstat(event)["ACCEPTED"].length }} going ·
                {{ attendeesByPartstat(event)["TENTATIVE"].length }} maybe ·
                {{ attendeesByPartstat(event)["DECLINED"].length }} not going
              </span>
            </div>
          </article>
        </section>

        <p
          v-else-if="selectedDay !== null && selectedDayEvents.length === 0"
          class="type-caption rounded-lg border border-border px-4 py-4 text-center text-muted-foreground"
        >
          No events on this day.
        </p>
      </template>

    </div>

    <!-- Cancel action sheet (recurring events only) -->
    <div
      v-if="cancelTarget"
      class="fixed inset-0 z-50 flex items-end justify-center bg-black/40 p-4 sm:items-center"
      role="dialog"
      aria-modal="true"
      @click.self="dismissCancelSheet"
    >
      <div class="w-full max-w-sm rounded-lg border border-border bg-card p-4 shadow-lg">
        <h2 class="type-pane-title text-foreground">
          Cancel "{{ cancelTarget.summary }}"
        </h2>
        <p class="type-caption mt-1 text-muted-foreground">
          This event repeats. Choose what to cancel:
        </p>
        <div class="mt-3 grid gap-2">
          <button
            type="button"
            class="inline-flex items-center justify-between rounded-md border border-input px-3 py-2 text-sm text-foreground hover:bg-muted/50"
            @click="confirmCancelInstance"
          >
            <span>Just this occurrence</span>
            <span class="type-caption text-muted-foreground">
              {{ formatStart(cancelTarget) }}
            </span>
          </button>
          <button
            type="button"
            class="inline-flex items-center justify-between rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive hover:bg-destructive/10"
            @click="confirmCancelSeries"
          >
            <span>The entire series</span>
            <span class="type-caption">All future occurrences</span>
          </button>
        </div>
        <div class="mt-3 flex justify-end">
          <button
            type="button"
            class="inline-flex items-center gap-1 rounded-md px-3 py-1.5 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
            @click="dismissCancelSheet"
          >
            Keep event
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
