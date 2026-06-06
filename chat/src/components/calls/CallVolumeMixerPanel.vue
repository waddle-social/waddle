<script setup lang="ts">
import { Volume2, VolumeX } from "lucide-vue-next";
import {
  callVolumePercentToGain,
  type CallVolumeMixerRow,
} from "@/lib/calls/call-volume-mixer";

defineProps<{
  rows: readonly CallVolumeMixerRow[];
}>();

const emit = defineEmits<{
  setVolume: [row: CallVolumeMixerRow, level: number];
  resetAll: [];
}>();

function onInput(row: CallVolumeMixerRow, event: Event): void {
  const target = event.target;
  if (!(target instanceof HTMLInputElement)) return;
  emit("setVolume", row, callVolumePercentToGain(Number(target.value), row.level));
}
</script>

<template>
  <aside class="call-volume-mixer" aria-label="Call volume mixer">
    <div class="call-volume-mixer__header">
      <h2 class="type-control">Who you hear</h2>
    </div>

    <ul v-if="rows.length > 0" class="call-volume-mixer__rows">
      <li
        v-for="row in rows"
        :key="row.key"
        class="call-volume-mixer__row"
        :class="{ 'call-volume-mixer__row--disabled': row.disabled }"
      >
        <div class="call-volume-mixer__meta">
          <span class="type-caption truncate">{{ row.label }}</span>
          <span v-if="row.hint" class="call-volume-mixer__hint">{{ row.hint }}</span>
        </div>
        <div class="call-volume-mixer__control">
          <VolumeX
            v-if="row.muted"
            class="call-volume-mixer__muted"
            aria-label="Muted"
          />
          <Volume2
            v-else
            class="call-volume-mixer__speaker"
            aria-hidden="true"
          />
          <div class="call-volume-mixer__slider-wrap">
            <input
              class="call-volume-mixer__slider"
              type="range"
              min="0"
              max="200"
              step="1"
              :value="Math.round(row.level * 100)"
              :disabled="row.disabled"
              :aria-label="row.ariaLabel"
              :aria-valuetext="row.ariaValueText"
              @input="onInput(row, $event)"
            >
            <span class="call-volume-mixer__tick" aria-hidden="true" style="left:50%;" />
          </div>
          <span class="call-volume-mixer__percent">{{ Math.round(row.level * 100) }}%</span>
        </div>
      </li>
    </ul>
    <div v-else class="call-volume-mixer__empty type-caption">
      No remote audio
    </div>

    <footer class="call-volume-mixer__footer">
      <button
        type="button"
        class="chat-action-button chat-action-button--secondary"
        @click="emit('resetAll')"
      >
        Reset all
      </button>
    </footer>
  </aside>
</template>

<style scoped>
.call-volume-mixer {
  width: min(18rem, 32vw);
  min-width: 14rem;
  display: flex;
  flex-direction: column;
  border-left: 1px solid var(--border);
  background: color-mix(in oklab, var(--muted) 20%, var(--background));
}

.call-volume-mixer__header,
.call-volume-mixer__footer {
  flex: 0 0 auto;
  padding: 0.75rem;
}

.call-volume-mixer__header {
  border-bottom: 1px solid var(--border);
}

.call-volume-mixer__rows {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 0.625rem;
  overflow-y: auto;
  padding: 0.75rem;
}

.call-volume-mixer__row {
  display: flex;
  flex-direction: column;
  gap: 0.375rem;
}

.call-volume-mixer__row--disabled {
  opacity: 0.58;
}

.call-volume-mixer__meta,
.call-volume-mixer__control {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  min-width: 0;
}

.call-volume-mixer__hint {
  flex: 0 0 auto;
  color: var(--muted-foreground);
  font-size: 0.75rem;
}

.call-volume-mixer__muted,
.call-volume-mixer__speaker {
  flex: 0 0 auto;
  width: 1rem;
  height: 1rem;
  color: var(--muted-foreground);
}

.call-volume-mixer__slider-wrap {
  position: relative;
  flex: 1 1 auto;
  min-width: 4rem;
  display: flex;
  align-items: center;
}

.call-volume-mixer__slider {
  width: 100%;
  accent-color: var(--primary);
}

.call-volume-mixer__tick {
  position: absolute;
  top: 50%;
  width: 1px;
  height: 0.875rem;
  transform: translateY(-50%);
  background: color-mix(in oklab, var(--foreground) 28%, transparent);
  pointer-events: none;
}

.call-volume-mixer__percent {
  flex: 0 0 2.25rem;
  color: var(--muted-foreground);
  font-size: 0.75rem;
  text-align: right;
}

.call-volume-mixer__empty {
  flex: 1 1 auto;
  padding: 0.75rem;
  color: var(--muted-foreground);
}

.call-volume-mixer__footer {
  border-top: 1px solid var(--border);
}

@media (max-width: 760px) {
  .call-volume-mixer {
    width: 100%;
    max-height: 16rem;
    border-left: 0;
    border-top: 1px solid var(--border);
  }
}
</style>
