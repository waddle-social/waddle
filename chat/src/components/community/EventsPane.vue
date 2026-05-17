<script setup lang="ts">
import { computed, ref } from "vue";
import {
  CalendarDays,
  ChevronLeft,
  ChevronRight,
  List,
  Menu,
  Plus,
  RefreshCw,
  Repeat,
  X,
} from "lucide-vue-next";
import RecurrencePicker from "@/components/community/RecurrencePicker.vue";
import type {
  Attendee,
  CommunityEvent,
  CommunityEventInput,
  PartStat,
  Rrule,
  Weekday,
} from "@/lib/xmpp-client";

interface EventsPaneProps {
  events: readonly CommunityEvent[];
  isLoading: boolean;
  isPosting: boolean;
  error: string | null;
  canPost: boolean;
  selfJid: string | null;
}

const props = defineProps<EventsPaneProps>();
const emit = defineEmits<{
  refresh: [];
  post: [input: CommunityEventInput];
  rsvp: [event: CommunityEvent, partstat: PartStat];
  openNav: [];
}>();

// ─── Identity ────────────────────────────────────────────────────────────────

const selfBareJid = computed(() => {
  const raw = props.selfJid;
  return raw ? raw.split("/")[0] ?? raw : null;
});

const selfAttendeeUri = computed(() => {
  const bare = selfBareJid.value;
  return bare ? `xmpp:${bare}` : null;
});

// ─── View & filter state ──────────────────────────────────────────────────────

type ViewMode = "week" | "month";
type EventFilter = "all" | "attending";

const viewMode = ref<ViewMode>("week");
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

const SEVEN_DAYS_MS = 7 * 24 * 60 * 60 * 1000;

/** Events starting within the next 7 days, grouped by locale date string. */
const weekEventsByDay = computed<Map<string, CommunityEvent[]>>(() => {
  const now = Date.now();
  const horizon = now + SEVEN_DAYS_MS;
  const groups = new Map<string, CommunityEvent[]>();
  for (const e of filteredEvents.value) {
    const ms = e.dtstartMs;
    if (typeof ms !== "number" || ms < now || ms > horizon) continue;
    const key = new Date(ms).toLocaleDateString(undefined, {
      weekday: "long",
      month: "short",
      day: "numeric",
    });
    (groups.get(key) ?? (groups.set(key, []), groups.get(key)!)).push(e);
  }
  return groups;
});

// ─── Month calendar view ──────────────────────────────────────────────────────

const calYear = ref(new Date().getFullYear());
const calMonth = ref(new Date().getMonth()); // 0-indexed
const selectedDay = ref<number | null>(null); // day-of-month

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

interface CalCell {
  day: number | null;
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
    cells.push({ day: null, isToday: false, events: [] });
  }

  for (let d = 1; d <= daysInMonth; d++) {
    const cellDate = new Date(y, m, d);
    const dayStart = cellDate.getTime();
    const dayEnd = dayStart + 86_400_000;
    const events = filteredEvents.value.filter(
      (e) => typeof e.dtstartMs === "number" && e.dtstartMs >= dayStart && e.dtstartMs < dayEnd,
    );
    cells.push({
      day: d,
      isToday: cellDate.toDateString() === todayStr,
      events,
    });
  }

  // Trailing empty cells to complete last row
  while (cells.length % 7 !== 0) cells.push({ day: null, isToday: false, events: [] });

  const weeks: CalCell[][] = [];
  for (let i = 0; i < cells.length; i += 7) weeks.push(cells.slice(i, i + 7));
  return weeks;
});

const selectedDayEvents = computed<CommunityEvent[]>(() => {
  const d = selectedDay.value;
  if (d === null) return [];
  return calGrid.value.flat().find((c) => c.day === d)?.events ?? [];
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
  if (typeof event.dtstartMs !== "number") return "TBD";
  return new Date(event.dtstartMs).toLocaleString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function formatTime(event: CommunityEvent): string {
  if (typeof event.dtstartMs !== "number") return "TBD";
  return new Date(event.dtstartMs).toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });
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
  return stripped.split("@")[0] ?? stripped;
}

// ─── Composer ─────────────────────────────────────────────────────────────────

const composerOpen = ref(false);
const summary = ref("");
const description = ref("");
const location = ref("");
const dtstart = ref("");
const dtend = ref("");
const rrule = ref<Rrule | null>(null);

const canSubmit = computed(
  () => props.canPost && !props.isPosting && summary.value.trim().length > 0,
);

function submit() {
  const summaryValue = summary.value.trim();
  if (!summaryValue) return;
  const input: CommunityEventInput = {
    summary: summaryValue,
    ...(description.value.trim() ? { description: description.value.trim() } : {}),
    ...(location.value.trim() ? { location: location.value.trim() } : {}),
    ...(props.selfJid
      ? { organizer: `xmpp:${props.selfJid.split("/")[0] ?? props.selfJid}` }
      : {}),
    ...(dtstart.value ? { dtstartMs: Date.parse(dtstart.value) } : {}),
    ...(dtend.value ? { dtendMs: Date.parse(dtend.value) } : {}),
    ...(rrule.value ? { rrule: rrule.value } : {}),
  };
  emit("post", input);
  resetComposer();
}

function resetComposer() {
  composerOpen.value = false;
  summary.value = "";
  description.value = "";
  location.value = "";
  dtstart.value = "";
  dtend.value = "";
  rrule.value = null;
}

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

        <!-- New event / Refresh -->
        <button
          v-if="canPost"
          type="button"
          class="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90"
          @click="composerOpen = !composerOpen"
        >
          <Plus class="h-3.5 w-3.5" aria-hidden="true" />
          {{ composerOpen ? "Close" : "New event" }}
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
        <div class="grid gap-2 md:grid-cols-2">
          <label class="grid gap-1 text-xs">
            <span class="text-muted-foreground">Starts</span>
            <input
              v-model="dtstart"
              type="datetime-local"
              class="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
            />
          </label>
          <label class="grid gap-1 text-xs">
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
            {{ isPosting ? "Publishing…" : "Publish event" }}
          </button>
        </div>
      </form>

      <!-- ── 7-day list view ─────────────────────────────────────────────── -->
      <template v-if="viewMode === 'week'">
        <div v-if="weekEventsByDay.size > 0" class="grid gap-4">
          <section
            v-for="[dayLabel, dayEvents] in weekEventsByDay"
            :key="dayLabel"
            class="grid gap-2"
          >
            <h2 class="type-section-label text-muted-foreground">{{ dayLabel }}</h2>
            <article
              v-for="event in dayEvents"
              :key="event.id"
              class="grid gap-1.5 rounded-lg border border-border bg-card px-4 py-3"
            >
              <header class="flex items-baseline justify-between gap-3">
                <h3 class="type-control font-semibold text-foreground">{{ event.summary }}</h3>
                <span class="type-caption shrink-0 text-muted-foreground">
                  {{ formatTime(event) }}
                </span>
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
          <span class="type-control font-semibold text-foreground">{{ calTitle }}</span>
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
                  cell.day === selectedDay ? 'bg-primary/10' : '',
                ]"
                :disabled="cell.day === null"
                @click="cell.day !== null && (selectedDay = selectedDay === cell.day ? null : cell.day)"
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
                    {{ ev.summary }}
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
              <span class="type-caption shrink-0 text-muted-foreground">{{ formatStart(event) }}</span>
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
  </div>
</template>
