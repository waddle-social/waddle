<script setup lang="ts">
import { computed, ref } from "vue";
import type { SafetyScores } from "@/lib/safety-scores/types";
import {
  formatSafetyProbability,
  leadingSafetyScore,
  notableSafetyScores,
  safetyCategoryPresentation,
  safetyProbabilitySeverity,
  safetyProbabilityWidth,
  QUESTION_MARKER_THRESHOLD,
} from "@/components/chat/message-safety-scores";
import { css, cx } from "styled-system/css";

// Server judgments fastened to a message (XEP-0422,
// `urn:waddle:safety-scores:1`). Safety scores use the notice threshold;
// question signals get a separate blue marker at their own threshold. The
// breakdown expands inline, matching the reply chip's disclosure idiom.
const props = defineProps<{
  scores: SafetyScores;
  messageId: string;
}>();

const expanded = ref(false);
const panelId = computed(() => `safety-scores-${props.messageId}`);
const indicator = computed(() => {
  const score = leadingSafetyScore(props.scores);
  return score
    ? { ...safetyCategoryPresentation(score.category), severity: safetyProbabilitySeverity(score.probability) }
    : null;
});
const questionScore = computed(() => {
  return props.scores.scores.find(
    (score) => score.category === "is_question" && score.probability >= QUESTION_MARKER_THRESHOLD,
  ) ?? null;
});
const questionPresentation = computed(() => safetyCategoryPresentation("is_question"));
const rows = computed(() => notableSafetyScores(props.scores));

// `data-tone` colours a label or bar from its category's token family.
const tone = css({
  "&[data-tone=info]": { color: "info.text" },
  "&[data-tone=danger]": { color: "danger.text" },
  "&[data-tone=warning]": { color: "warning.text" },
  "&[data-tone=live]": { color: "live.text" },
  "&[data-tone=gold]": { color: "gold.text" },
  "&[data-tone=muted]": { color: "fg.muted" },
});
const c = {
  root: css({ display: "flex", flexDirection: "column", alignItems: "flex-start", gap: "1" }),
  toggle: cx(tone, css({
    display: "inline-flex",
    alignItems: "center",
    gap: "1",
    height: "24px",
    px: "1.5",
    borderRadius: "sm",
    textStyle: "caption",
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}",
    _hover: { bg: "state.hover" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
    "& svg": { width: "12px", height: "12px" },
  })),
  panel: css({ width: "100%", maxWidth: "320px", px: "2.5", py: "2", borderRadius: "sm", border: "1px solid {colors.border.soft}", bg: "surface" }),
  list: css({ display: "flex", flexDirection: "column", gap: "1.5" }),
  row: css({ display: "grid", gridTemplateColumns: "minmax(0, 7.5rem) 1fr 2.5rem", alignItems: "center", gap: "2" }),
  label: cx(tone, css({ display: "flex", minWidth: 0, alignItems: "center", gap: "1", textStyle: "caption", "& svg": { width: "12px", height: "12px", flex: "none" } })),
  name: css({ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  track: css({ height: "4px", overflow: "hidden", borderRadius: "pill", bg: "surface.3" }),
  bar: cx(tone, css({ display: "block", height: "100%", borderRadius: "pill", bg: "currentColor" })),
  value: css({ textStyle: "caption", textAlign: "right", color: "fg", fontVariantNumeric: "tabular-nums" }),
  model: css({ mt: "1.5", textStyle: "caption", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
};
</script>

<template>
  <div v-if="indicator || questionScore" :class="c.root">
    <button
      v-if="indicator"
      type="button"
      :class="c.toggle"
      :data-tone="indicator.tone"
      :data-severity="indicator.severity"
      :aria-expanded="expanded"
      :aria-controls="panelId"
      :aria-label="`${expanded ? 'Hide' : 'Show'} ${indicator.label} scores`"
      :title="`${expanded ? 'Hide' : 'Show'} ${indicator.label} scores`"
      @click="expanded = !expanded"
    >
      <component :is="indicator.icon" aria-hidden="true" />
      <span>{{ indicator.label }}</span>
    </button>
    <button
      v-if="questionScore"
      type="button"
      :class="c.toggle"
      :data-tone="questionPresentation.tone"
      :aria-expanded="expanded"
      :aria-controls="panelId"
      :aria-label="`${expanded ? 'Hide' : 'Show'} question score`"
      :title="`${expanded ? 'Hide' : 'Show'} question score`"
      @click="expanded = !expanded"
    >
      <component :is="questionPresentation.icon" aria-hidden="true" />
      <span>Question</span>
    </button>
    <div
      v-if="expanded"
      :id="panelId"
      :class="c.panel"
    >
      <ul :class="c.list">
        <li
          v-for="score in rows"
          :key="score.category"
          :class="c.row"
        >
          <span
            :class="c.label"
            :data-tone="safetyCategoryPresentation(score.category).tone"
            :title="score.taxonomyVersion"
          >
            <component :is="safetyCategoryPresentation(score.category).icon" aria-hidden="true" />
            <span :class="c.name">{{ safetyCategoryPresentation(score.category).label }}</span>
          </span>
          <span :class="c.track" aria-hidden="true">
            <span
              :class="c.bar"
              :data-tone="safetyCategoryPresentation(score.category).tone"
              :style="{ width: safetyProbabilityWidth(score.probability) }"
            />
          </span>
          <span :class="c.value">
            {{ formatSafetyProbability(score.probability) }}
          </span>
        </li>
      </ul>
      <p :class="c.model" :title="scores.modelVersion">
        Model {{ scores.modelVersion }}
      </p>
    </div>
  </div>
</template>
