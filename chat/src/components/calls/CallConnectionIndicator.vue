<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callConnectionPhase,
  $callConnectionQuality,
  qualityToChip,
} from "@/lib/calls/connection-quality";

/**
 * Ambient self-connection indicator for the call bar. Reads the
 * engine-fed quality + transport-phase atoms and renders a three-bar
 * signal glyph that stays quiet when healthy (bars only, no text) and
 * escalates to an amber "Poor connection" / red "Reconnecting…" label as
 * the local connection degrades. Self-contained and store-connected so
 * the stateless `CallControls` bar can embed it without threading props.
 *
 * Hidden entirely (`v-if`) until the first real quality sample arrives,
 * so a fresh call doesn't flash an empty "no signal" glyph.
 */
const quality = useStore($callConnectionQuality);
const phase = useStore($callConnectionPhase);

const chip = computed(() => qualityToChip(quality.value, phase.value));

/** Three ascending bars; each is "filled" up to `chip.bars`. */
const bars = [1, 2, 3] as const;

const ariaLabel = computed(() => {
  const c = chip.value;
  if (!c) return "";
  if (c.label) return `Connection: ${c.label}`;
  return c.bars === 3 ? "Connection: excellent" : "Connection: good";
});
</script>

<template>
  <div
    v-if="chip"
    class="call-connection"
    :class="`call-connection--${chip.tone}`"
    role="status"
    aria-live="polite"
    :aria-label="ariaLabel"
    :title="ariaLabel"
  >
    <span class="call-connection__bars" aria-hidden="true">
      <span
        v-for="bar in bars"
        :key="bar"
        class="call-connection__bar"
        :class="bar <= chip.bars ? 'call-connection__bar--on' : 'call-connection__bar--off'"
        :style="{ height: `${bar * 33}%` }"
      />
    </span>
    <span v-if="chip.label" class="type-control call-connection__label">{{ chip.label }}</span>
  </div>
</template>

<style scoped>
.call-connection {
  display: inline-flex;
  align-items: center;
  gap: var(--space-xs);
  color: var(--muted-foreground);
}

/* Warn / danger tint the whole chip (bars + label) together. */
.call-connection--warn {
  color: var(--warning);
}

.call-connection--danger {
  color: var(--destructive);
}

.call-connection__bars {
  display: inline-flex;
  align-items: flex-end;
  gap: 2px;
  width: 1rem;
  height: 1rem;
}

.call-connection__bar {
  flex: 1;
  border-radius: 1px;
  align-self: flex-end;
}

/* Filled segments take the chip's current color. */
.call-connection__bar--on {
  background: currentColor;
}

/* Empty segments are a faint ghost of it, so the bar count reads at a
   glance without a hard background box. */
.call-connection__bar--off {
  background: color-mix(in oklab, currentColor 25%, transparent);
}

.call-connection__label {
  white-space: nowrap;
}
</style>
