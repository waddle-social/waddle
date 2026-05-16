<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { Freq, Rrule, Weekday } from "@/lib/xmpp-client";

type Mode = "none" | "daily" | "weekly" | "monthly" | "yearly";
type EndKind = "never" | "count" | "until";

interface RecurrencePickerProps {
  modelValue: Rrule | null;
}

const props = defineProps<RecurrencePickerProps>();
const emit = defineEmits<{
  "update:modelValue": [value: Rrule | null];
}>();

const mode = ref<Mode>(props.modelValue ? freqToMode(props.modelValue.freq) : "none");
const interval = ref<number>(props.modelValue?.interval ?? 1);
const byDay = ref<Set<Weekday>>(new Set(props.modelValue?.byDay ?? []));
const endKind = ref<EndKind>(deriveEndKind(props.modelValue));
const countValue = ref<number>(props.modelValue?.count ?? 10);
const untilValue = ref<string>(
  typeof props.modelValue?.untilMs === "number"
    ? new Date(props.modelValue.untilMs).toISOString().slice(0, 10)
    : "",
);

function freqToMode(freq: Freq): Mode {
  switch (freq) {
    case "DAILY":
      return "daily";
    case "WEEKLY":
      return "weekly";
    case "MONTHLY":
      return "monthly";
    case "YEARLY":
      return "yearly";
  }
}

function modeToFreq(m: Mode): Freq | null {
  switch (m) {
    case "daily":
      return "DAILY";
    case "weekly":
      return "WEEKLY";
    case "monthly":
      return "MONTHLY";
    case "yearly":
      return "YEARLY";
    case "none":
      return null;
  }
}

function deriveEndKind(rrule: Rrule | null): EndKind {
  if (!rrule) return "never";
  if (typeof rrule.count === "number") return "count";
  if (typeof rrule.untilMs === "number") return "until";
  return "never";
}

const weekdays: { code: Weekday; label: string }[] = [
  { code: "MO", label: "M" },
  { code: "TU", label: "T" },
  { code: "WE", label: "W" },
  { code: "TH", label: "T" },
  { code: "FR", label: "F" },
  { code: "SA", label: "S" },
  { code: "SU", label: "S" },
];

const intervalLabel = computed(() => {
  switch (mode.value) {
    case "daily":
      return interval.value === 1 ? "Every day" : `Every ${interval.value} days`;
    case "weekly":
      return interval.value === 1 ? "Every week" : `Every ${interval.value} weeks`;
    case "monthly":
      return interval.value === 1 ? "Every month" : `Every ${interval.value} months`;
    case "yearly":
      return interval.value === 1 ? "Every year" : `Every ${interval.value} years`;
    default:
      return "";
  }
});

function toggleWeekday(code: Weekday) {
  if (byDay.value.has(code)) {
    byDay.value.delete(code);
  } else {
    byDay.value.add(code);
  }
  byDay.value = new Set(byDay.value);
}

function buildRrule(): Rrule | null {
  const freq = modeToFreq(mode.value);
  if (!freq) return null;
  const rule: Rrule = {
    freq,
    ...(interval.value > 1 ? { interval: interval.value } : {}),
  };
  if (mode.value === "weekly" && byDay.value.size > 0) {
    rule.byDay = Array.from(byDay.value);
  }
  if (endKind.value === "count" && countValue.value > 0) {
    rule.count = countValue.value;
  } else if (endKind.value === "until" && untilValue.value) {
    const untilMs = Date.parse(`${untilValue.value}T23:59:59Z`);
    if (Number.isFinite(untilMs)) rule.untilMs = untilMs;
  }
  return rule;
}

watch(
  [mode, interval, byDay, endKind, countValue, untilValue],
  () => {
    emit("update:modelValue", buildRrule());
  },
  { deep: true },
);
</script>

<template>
  <div class="grid gap-3 rounded-lg border border-border bg-background p-3">
    <div class="flex items-center gap-2">
      <label class="type-caption text-muted-foreground">Repeats</label>
      <select
        v-model="mode"
        class="rounded-md border border-input bg-background px-2 py-1 text-xs"
        aria-label="Recurrence frequency"
      >
        <option value="none">Doesn't repeat</option>
        <option value="daily">Daily</option>
        <option value="weekly">Weekly</option>
        <option value="monthly">Monthly</option>
        <option value="yearly">Yearly</option>
      </select>
    </div>

    <template v-if="mode !== 'none'">
      <div class="flex items-center gap-2">
        <label class="type-caption text-muted-foreground" :for="`rec-interval-${$.uid}`">
          {{ intervalLabel }} — interval
        </label>
        <input
          :id="`rec-interval-${$.uid}`"
          v-model.number="interval"
          type="number"
          min="1"
          max="99"
          class="w-16 rounded-md border border-input bg-background px-2 py-1 text-xs"
        />
      </div>

      <div v-if="mode === 'weekly'" class="grid gap-1">
        <span class="type-caption text-muted-foreground">On weekdays</span>
        <div class="flex flex-wrap gap-1">
          <button
            v-for="day in weekdays"
            :key="day.code"
            type="button"
            class="inline-flex h-7 min-w-[1.75rem] items-center justify-center rounded-md border px-2 text-xs"
            :class="byDay.has(day.code)
              ? 'border-primary bg-primary/10 text-primary'
              : 'border-input text-muted-foreground hover:bg-muted/50'"
            :aria-pressed="byDay.has(day.code)"
            @click="toggleWeekday(day.code)"
          >
            {{ day.label }}
          </button>
        </div>
      </div>

      <div class="grid gap-1">
        <span class="type-caption text-muted-foreground">Ends</span>
        <div class="flex flex-wrap items-center gap-2">
          <label class="inline-flex items-center gap-1 text-xs">
            <input v-model="endKind" type="radio" value="never" />
            Never
          </label>
          <label class="inline-flex items-center gap-1 text-xs">
            <input v-model="endKind" type="radio" value="count" />
            After
            <input
              v-model.number="countValue"
              type="number"
              min="1"
              max="999"
              class="w-16 rounded-md border border-input bg-background px-2 py-1 text-xs"
              :disabled="endKind !== 'count'"
            />
            occurrences
          </label>
          <label class="inline-flex items-center gap-1 text-xs">
            <input v-model="endKind" type="radio" value="until" />
            On
            <input
              v-model="untilValue"
              type="date"
              class="rounded-md border border-input bg-background px-2 py-1 text-xs"
              :disabled="endKind !== 'until'"
            />
          </label>
        </div>
      </div>
    </template>
  </div>
</template>
