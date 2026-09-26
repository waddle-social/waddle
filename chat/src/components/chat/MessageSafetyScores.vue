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
</script>

<template>
  <div v-if="indicator || questionScore" class="chat-safety-scores flex flex-col items-start gap-1">
    <button
      v-if="indicator"
      type="button"
      class="chat-safety-scores__toggle type-caption inline-flex h-6 items-center gap-1 rounded-md px-1.5 transition-colors hover:bg-muted/50"
      :class="indicator.textClass"
      :data-severity="indicator.severity"
      :aria-expanded="expanded"
      :aria-controls="panelId"
      :aria-label="`${expanded ? 'Hide' : 'Show'} ${indicator.label} scores`"
      :title="`${expanded ? 'Hide' : 'Show'} ${indicator.label} scores`"
      @click="expanded = !expanded"
    >
      <component :is="indicator.icon" class="h-3 w-3" aria-hidden="true" />
      <span>{{ indicator.label }}</span>
    </button>
    <button
      v-if="questionScore"
      type="button"
      class="chat-safety-scores__toggle type-caption inline-flex h-6 items-center gap-1 rounded-md px-1.5 transition-colors hover:bg-muted/50"
      :class="questionPresentation.textClass"
      :aria-expanded="expanded"
      :aria-controls="panelId"
      :aria-label="`${expanded ? 'Hide' : 'Show'} question score`"
      :title="`${expanded ? 'Hide' : 'Show'} question score`"
      @click="expanded = !expanded"
    >
      <component :is="questionPresentation.icon" class="h-3 w-3" aria-hidden="true" />
      <span>Question</span>
    </button>
    <div
      v-if="expanded"
      :id="panelId"
      class="chat-safety-scores__panel w-full max-w-xs rounded-md border border-border bg-muted/25 px-2.5 py-2"
    >
      <ul class="flex flex-col gap-1.5">
        <li
          v-for="score in rows"
          :key="score.category"
          class="grid grid-cols-[minmax(0,7.5rem)_1fr_2.5rem] items-center gap-2"
        >
          <span
            class="type-meta flex min-w-0 items-center gap-1"
            :class="safetyCategoryPresentation(score.category).textClass"
            :title="score.taxonomyVersion"
          >
            <component :is="safetyCategoryPresentation(score.category).icon" class="h-3 w-3 shrink-0" aria-hidden="true" />
            <span class="truncate">{{ safetyCategoryPresentation(score.category).label }}</span>
          </span>
          <span class="h-1 overflow-hidden rounded-full bg-muted" aria-hidden="true">
            <span
              class="block h-full rounded-full"
              :class="safetyCategoryPresentation(score.category).barClass"
              :style="{ width: safetyProbabilityWidth(score.probability) }"
            />
          </span>
          <span class="type-meta type-numeric text-right text-foreground/80">
            {{ formatSafetyProbability(score.probability) }}
          </span>
        </li>
      </ul>
      <p class="type-meta mt-1.5 truncate text-muted-foreground" :title="scores.modelVersion">
        Model {{ scores.modelVersion }}
      </p>
    </div>
  </div>
</template>
