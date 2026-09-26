<script setup lang="ts">
import { computed, ref } from "vue";
import { Gauge } from "lucide-vue-next";
import type { SafetyScores } from "@/lib/safety-scores/types";
import {
  formatSafetyProbability,
  notableSafetyScores,
  safetyCategoryLabel,
  safetyProbabilityBarClass,
  safetyProbabilityWidth,
  safetyScoresSeverity,
  safetyScoresToggleLabel,
  safetySeverityTextClass,
} from "@/components/chat/message-safety-scores";

// Server judgments fastened to a message (XEP-0422,
// `urn:waddle:safety-scores:1`). The parent only mounts this once a
// content-safety category has crossed the notice threshold, so the chip is
// always amber or red, never neutral. Collapsed by default to a quiet chip
// in the reactions row; the breakdown expands inline, matching the reply
// chip's disclosure idiom, so it works identically on touch and desktop.
const props = defineProps<{
  scores: SafetyScores;
  messageId: string;
}>();

const expanded = ref(false);
const panelId = computed(() => `safety-scores-${props.messageId}`);
const severity = computed(() => safetyScoresSeverity(props.scores) ?? "notice");
const severityClass = computed(() => safetySeverityTextClass(severity.value));
const toggleLabel = computed(() => safetyScoresToggleLabel(expanded.value, severity.value));
const rows = computed(() => notableSafetyScores(props.scores));
</script>

<template>
  <div class="chat-safety-scores flex flex-col items-start gap-1">
    <button
      type="button"
      class="chat-safety-scores__toggle type-caption inline-flex h-6 items-center gap-1 rounded-md px-1.5 transition-colors hover:bg-muted/50"
      :class="severityClass"
      :data-severity="severity"
      :aria-expanded="expanded"
      :aria-controls="panelId"
      :aria-label="toggleLabel"
      :title="toggleLabel"
      @click="expanded = !expanded"
    >
      <Gauge class="h-3 w-3" aria-hidden="true" />
      <span>Scores</span>
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
          <span class="type-meta truncate text-muted-foreground" :title="score.taxonomyVersion">
            {{ safetyCategoryLabel(score.category) }}
          </span>
          <span class="h-1 overflow-hidden rounded-full bg-muted" aria-hidden="true">
            <span
              class="block h-full rounded-full"
              :class="safetyProbabilityBarClass(score.probability)"
              :style="{ width: safetyProbabilityWidth(score.probability) }"
            />
          </span>
          <span class="type-meta type-numeric text-right text-foreground/80">
            {{ formatSafetyProbability(score.probability) }}
          </span>
        </li>
      </ul>
      <p class="type-meta mt-1.5 truncate text-muted-foreground/60" :title="scores.modelVersion">
        Model {{ scores.modelVersion }}
      </p>
    </div>
  </div>
</template>
