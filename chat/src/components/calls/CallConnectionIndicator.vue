<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { css } from "styled-system/css";
import {
  $callConnectionPhase,
  $callConnectionQuality,
  qualityToChip,
} from "@/lib/calls/connection-quality";

/**
 * Ambient self-connection indicator for the call bar. Reads the
 * engine-fed quality + transport-phase atoms and renders a three-bar
 * signal glyph that stays quiet when healthy (bars only, no text) and
 * escalates to an amber "Poor connection" / red "Connection lost" /
 * "Reconnecting…" label as the local connection degrades. Self-contained
 * and store-connected so the stateless `CallControls` bar can embed it
 * without threading props.
 *
 * Always rendered during a call (a quiet, empty "measuring" glyph at the
 * `unknown` start state) so the centered control row keeps a stable
 * footprint instead of reflowing when the first sample lands.
 *
 * Accessibility: the visual glyph is `aria-hidden`; a separate
 * visually-hidden live region announces ONLY the degraded label, so
 * healthy↔healthy transitions (Excellent↔Good) stay silent for screen
 * readers and don't spam announcements.
 */
const quality = useStore($callConnectionQuality);
const phase = useStore($callConnectionPhase);

const chip = computed(() => qualityToChip(quality.value, phase.value));

/** Three ascending bars; each is "filled" up to `chip.bars`. */
const bars = [1, 2, 3] as const;

/** Full status, for the sighted hover tooltip (covers the no-label states). */
const statusText = computed(() => {
  const c = chip.value;
  if (c.label) return c.label;
  if (c.bars === 3) return "Excellent connection";
  if (c.bars === 2) return "Good connection";
  // 0 bars, no label → the quiet `unknown` start state before the first
  // quality sample. Don't claim "Good" — say what it's actually doing.
  return "Measuring…";
});

/** Spoken text: only the degraded label, empty (silent) when healthy. */
const announcement = computed(() => chip.value.label ?? "");

// HuddleStage connection chip: quiet bars while healthy, a tinted pill
// with a label once degraded.
const rootClass = css({
  display: "inline-flex",
  alignItems: "center",
  gap: "1.5",
  flex: "none",
  height: "28px",
  borderRadius: "pill",
  color: "presence.available",
  font: "600 13px/1 {fonts.body}",
  whiteSpace: "nowrap",
});
const TONE = {
  neutral: css({ color: "fg.muted" }),
  warn: css({ px: "2.5", color: "warning.text", bg: "color-mix(in srgb, {colors.presence.away} 14%, transparent)" }),
  danger: css({ px: "2.5", color: "danger.text", bg: "color-mix(in srgb, {colors.danger} 14%, transparent)" }),
};
const healthyClass = css({ color: "presence.available" });
const barsClass = css({ display: "inline-flex", alignItems: "flex-end", gap: "0.5", width: "16px", height: "16px" });
const barClass = css({ flex: 1, borderRadius: "1px", bg: "currentColor" });
const barOffClass = css({ opacity: 0.25 });
const srOnly = css({ srOnly: true });
</script>

<template>
  <div
    :class="['call-connection', rootClass, chip.bars > 0 && !chip.label ? healthyClass : TONE[chip.tone]]"
    :title="statusText"
  >
    <span :class="barsClass" aria-hidden="true">
      <span
        v-for="bar in bars"
        :key="bar"
        :class="[barClass, bar > chip.bars && barOffClass]"
        :style="{ height: `${bar * 33}%` }"
      />
    </span>
    <span v-if="chip.label">{{ chip.label }}</span>
    <span :class="srOnly" role="status" aria-live="polite">{{ announcement }}</span>
  </div>
</template>
