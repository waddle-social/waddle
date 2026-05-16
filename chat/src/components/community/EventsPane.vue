<script setup lang="ts">
import { computed, ref } from "vue";
import { CalendarDays, Plus, RefreshCw, Repeat, X } from "lucide-vue-next";
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
}>();

const selfBareJid = computed(() => {
  const raw = props.selfJid;
  return raw ? raw.split("/")[0] ?? raw : null;
});

const selfAttendeeUri = computed(() => {
  const bare = selfBareJid.value;
  return bare ? `xmpp:${bare}` : null;
});

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

const composerOpen = ref(false);
const summary = ref("");
const description = ref("");
const location = ref("");
const dtstart = ref("");
const dtend = ref("");
const rrule = ref<Rrule | null>(null);

const weekdayLabels: Record<Weekday, string> = {
  SU: "Sun",
  MO: "Mon",
  TU: "Tue",
  WE: "Wed",
  TH: "Thu",
  FR: "Fri",
  SA: "Sat",
};

const upcoming = computed(() => props.events.filter((e) => !isPast(e)));
const past = computed(() => props.events.filter(isPast));

function isPast(event: CommunityEvent): boolean {
  return typeof event.dtstartMs === "number" && event.dtstartMs < Date.now();
}

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

const canSubmit = computed(() => {
  return props.canPost && !props.isPosting && summary.value.trim().length > 0;
});

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
</script>

<template>
  <div class="chat-pane-scroll flex-1 min-h-0 bg-background px-[var(--chat-content-inline)] py-6">
    <div class="mx-auto grid w-full max-w-3xl gap-4">
      <header class="flex items-center gap-2">
        <CalendarDays class="h-5 w-5 text-primary" aria-hidden="true" />
        <h1 class="type-pane-title text-foreground">Community Events</h1>
        <button
          v-if="canPost"
          type="button"
          class="ml-auto inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground shadow-sm hover:bg-primary/90"
          @click="composerOpen = !composerOpen"
        >
          <Plus class="h-3.5 w-3.5" aria-hidden="true" />
          {{ composerOpen ? "Close" : "New event" }}
        </button>
        <button
          v-else
          type="button"
          class="ml-auto inline-flex items-center gap-1 rounded-md border border-transparent px-2 py-1 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
          :disabled="isLoading"
          @click="emit('refresh')"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': isLoading }" aria-hidden="true" />
          Refresh
        </button>
      </header>

      <div v-if="error" class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
        Couldn't load events: {{ error }}
      </div>

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

      <section class="grid gap-2">
        <h2 class="type-section-label text-muted-foreground">Upcoming</h2>
        <article
          v-for="event in upcoming"
          :key="event.id"
          class="grid gap-1.5 rounded-lg border border-border bg-card px-4 py-3"
        >
          <header class="flex items-baseline justify-between gap-3">
            <h3 class="type-control font-semibold text-foreground">{{ event.summary }}</h3>
            <span class="type-caption text-muted-foreground" :title="event.dtstartMs ? new Date(event.dtstartMs).toLocaleString() : ''">
              {{ formatStart(event) }}
            </span>
          </header>
          <p v-if="event.location" class="type-caption text-muted-foreground">📍 {{ event.location }}</p>
          <p v-if="event.description" class="whitespace-pre-wrap break-words text-sm text-foreground">{{ event.description }}</p>
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
        <p
          v-if="upcoming.length === 0 && !isLoading"
          class="type-caption rounded-lg border border-border px-4 py-6 text-center text-muted-foreground"
        >
          No upcoming events. {{ canPost ? "Schedule one to get the community together." : "Check back later." }}
        </p>
      </section>

      <section v-if="past.length > 0" class="grid gap-2">
        <h2 class="type-section-label text-muted-foreground">Past</h2>
        <article
          v-for="event in past"
          :key="event.id"
          class="grid gap-1.5 rounded-lg border border-border bg-card/60 px-4 py-3 opacity-75"
        >
          <header class="flex items-baseline justify-between gap-3">
            <h3 class="type-control text-foreground">{{ event.summary }}</h3>
            <span class="type-caption text-muted-foreground">{{ formatStart(event) }}</span>
          </header>
        </article>
      </section>
    </div>
  </div>
</template>
