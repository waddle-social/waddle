<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { css, cx } from "styled-system/css";
import { input, kicker } from "styled-system/recipes";
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

const weekdays: { code: Weekday; label: string; name: string }[] = [
  { code: "MO", label: "M", name: "Monday" },
  { code: "TU", label: "T", name: "Tuesday" },
  { code: "WE", label: "W", name: "Wednesday" },
  { code: "TH", label: "T", name: "Thursday" },
  { code: "FR", label: "F", name: "Friday" },
  { code: "SA", label: "S", name: "Saturday" },
  { code: "SU", label: "S", name: "Sunday" },
];

// Events-Form board: a "Repeats" fieldset inside the event form.
const ui = {
  root: css({
    margin: 0,
    minWidth: 0,
    display: "flex",
    flexDirection: "column",
    gap: "3.5",
    padding: "4",
    borderRadius: "12px",
    border: "1px solid {colors.border.soft}",
  }),
  legend: cx(kicker(), css({ px: "1.5" })),
  select: cx(input(), css({ width: "100%", maxWidth: "260px" })),
  label: css({ display: "flex", flexDirection: "column", gap: "1.5", fontSize: "13px", fontWeight: 600, color: "fg.muted" }),
  row: css({ display: "flex", alignItems: "center", gap: "2.5", flexWrap: "wrap", fontSize: "13px", fontWeight: 600, color: "fg.muted" }),
  number: cx(input(), css({ width: "72px" })),
  date: cx(input(), css({ width: "180px", maxWidth: "100%" })),
  days: css({ display: "flex", gap: "1.5", flexWrap: "wrap" }),
  day: css({
    width: "36px",
    height: "36px",
    borderRadius: "pill",
    border: "1px solid {colors.border}",
    bg: "transparent",
    color: "fg.muted",
    font: "600 13px/1 {fonts.body}",
    cursor: "pointer",
    _hover: { color: "fg", bg: "state.hover" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
    _pressed: { bg: "action", borderColor: "action", color: "action.fg", _hover: { bg: "action.hover" } },
    _coarse: { width: "44px", height: "44px" },
  }),
  ends: css({ display: "flex", flexDirection: "column" }),
  radio: css({
    display: "flex",
    alignItems: "center",
    gap: "2.5",
    flexWrap: "wrap",
    minHeight: "36px",
    py: "1",
    fontSize: "14px",
    color: "fg",
    "& input[type=radio]": { width: "16px", height: "16px", margin: 0, accentColor: "{colors.action}" },
  }),
};

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
  <fieldset :class="ui.root">
    <legend :class="ui.legend">Repeats</legend>
    <select
      v-model="mode"
      :class="ui.select"
      aria-label="Recurrence frequency"
    >
      <option value="none">Doesn't repeat</option>
      <option value="daily">Daily</option>
      <option value="weekly">Weekly</option>
      <option value="monthly">Monthly</option>
      <option value="yearly">Yearly</option>
    </select>

    <template v-if="mode !== 'none'">
      <div :class="ui.row">
        <label :for="`rec-interval-${$.uid}`">
          {{ intervalLabel }} — interval
        </label>
        <input
          :id="`rec-interval-${$.uid}`"
          v-model.number="interval"
          type="number"
          min="1"
          max="99"
          :class="ui.number"
        />
      </div>

      <div v-if="mode === 'weekly'" :class="ui.label">
        <span :id="`rec-days-${$.uid}`">On weekdays</span>
        <div :class="ui.days" role="group" :aria-labelledby="`rec-days-${$.uid}`">
          <button
            v-for="day in weekdays"
            :key="day.code"
            type="button"
            :class="ui.day"
            :aria-label="day.name"
            :aria-pressed="byDay.has(day.code)"
            @click="toggleWeekday(day.code)"
          >
            {{ day.label }}
          </button>
        </div>
      </div>

      <div :class="ui.ends" role="radiogroup" :aria-labelledby="`rec-ends-${$.uid}`">
        <span :id="`rec-ends-${$.uid}`" :class="ui.label">Ends</span>
        <label :class="ui.radio">
          <input v-model="endKind" type="radio" value="never" />
          Never
        </label>
        <label :class="ui.radio">
          <input v-model="endKind" type="radio" value="count" />
          After
          <input
            v-model.number="countValue"
            type="number"
            min="1"
            max="999"
            :class="ui.number"
            :disabled="endKind !== 'count'"
          />
          occurrences
        </label>
        <label :class="ui.radio">
          <input v-model="endKind" type="radio" value="until" />
          On
          <input
            v-model="untilValue"
            type="date"
            :class="ui.date"
            :disabled="endKind !== 'until'"
          />
        </label>
      </div>
    </template>
  </fieldset>
</template>
