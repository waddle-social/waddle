<script setup lang="ts">
import { ref, watch } from "vue";
import { Mic, MicOff, Video, VideoOff, Volume2, VolumeX } from "lucide-vue-next";
import {
  callVolumePercentToGain,
  type CallVolumeMixerRow,
} from "@/lib/calls/call-volume-mixer";
import type { CallRosterRow } from "@/lib/calls/call-roster";

/**
 * The Participants roster shown inside the call Dock: one row per
 * attendee with live mic/camera state, a speaking highlight, and the
 * per-participant volume control(s) that subsume the old "Who you hear"
 * mixer. Stateless beyond the slider's local echo; the parent owns the
 * roster controller and applies the gain to LiveKit.
 */
const props = defineProps<{
  rows: readonly CallRosterRow[];
}>();

const emit = defineEmits<{
  setVolume: [row: CallVolumeMixerRow, level: number];
  resetAll: [];
}>();

// Echo the last gain we emitted per volume key so the snap-detent logic
// reads the value the user is dragging from, not the prop (which lags a
// tick behind the controller's apply path).
const lastEmittedLevels = ref<Record<string, number>>({});

function syncLastEmittedLevels(rows: readonly CallRosterRow[]): void {
  const next: Record<string, number> = {};
  for (const row of rows) {
    for (const volume of row.volumeRows) next[volume.key] = volume.level;
  }
  lastEmittedLevels.value = next;
}

watch(() => props.rows, syncLastEmittedLevels);

function onInput(volume: CallVolumeMixerRow, event: Event): void {
  const target = event.target;
  if (!(target instanceof HTMLInputElement)) return;
  const currentGain = lastEmittedLevels.value[volume.key] ?? volume.level;
  const nextGain = callVolumePercentToGain(Number(target.value), currentGain);
  lastEmittedLevels.value = {
    ...lastEmittedLevels.value,
    [volume.key]: nextGain,
  };
  emit("setVolume", volume, nextGain);
}

function onResetAll(): void {
  lastEmittedLevels.value = {};
  emit("resetAll");
}
</script>

<template>
  <div class="call-participants" aria-label="Participants">
    <ul class="call-participants__rows">
      <li
        v-for="row in rows"
        :key="row.key"
        class="call-participants__row"
        :class="{ 'call-participants__row--speaking': row.speaking }"
      >
        <div class="call-participants__person">
          <span
            v-if="row.speaking"
            class="call-participants__speaking"
            aria-label="Speaking"
          />
          <span class="call-participants__name type-control truncate">{{ row.label }}</span>
          <span class="call-participants__media">
            <component
              :is="row.micOn ? Mic : MicOff"
              class="call-participants__icon"
              :class="{ 'call-participants__icon--off': !row.micOn }"
              :aria-label="row.micOn ? 'Microphone on' : 'Microphone off'"
            />
            <component
              :is="row.cameraOn ? Video : VideoOff"
              class="call-participants__icon"
              :class="{ 'call-participants__icon--off': !row.cameraOn }"
              :aria-label="row.cameraOn ? 'Camera on' : 'Camera off'"
            />
          </span>
        </div>

        <div
          v-for="volume in row.volumeRows"
          :key="volume.key"
          class="call-participants__control"
          :class="{ 'call-participants__control--disabled': volume.disabled }"
        >
          <VolumeX
            v-if="volume.muted"
            class="call-participants__volume-icon"
            aria-label="Muted"
          />
          <Volume2
            v-else
            class="call-participants__volume-icon"
            aria-hidden="true"
          />
          <span v-if="volume.hint" class="call-participants__hint">{{ volume.hint }}</span>
          <div class="call-participants__slider-wrap">
            <input
              class="call-participants__slider"
              type="range"
              min="0"
              max="200"
              step="1"
              :value="Math.round(volume.level * 100)"
              :disabled="volume.disabled"
              :aria-label="volume.ariaLabel"
              :aria-valuetext="volume.ariaValueText"
              @input="onInput(volume, $event)"
            >
            <span class="call-participants__tick" aria-hidden="true" style="left:50%;" />
          </div>
          <span class="call-participants__percent">{{ Math.round(volume.level * 100) }}%</span>
        </div>
      </li>
    </ul>

    <footer class="call-participants__footer">
      <button
        type="button"
        class="chat-action-button chat-action-button--secondary"
        @click="onResetAll"
      >
        Reset all
      </button>
    </footer>
  </div>
</template>

<style scoped>
.call-participants {
  display: flex;
  flex-direction: column;
  min-height: 0;
  height: 100%;
}

.call-participants__rows {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  overflow-y: auto;
  padding: 0.75rem;
}

.call-participants__row {
  display: flex;
  flex-direction: column;
  gap: 0.375rem;
}

.call-participants__person {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  min-width: 0;
}

.call-participants__speaking {
  flex: 0 0 auto;
  width: 0.5rem;
  height: 0.5rem;
  border-radius: 9999px;
  background: oklch(0.7 0.18 145);
  box-shadow: 0 0 0 3px color-mix(in oklab, oklch(0.7 0.18 145) 25%, transparent);
}

.call-participants__name {
  flex: 1 1 auto;
  min-width: 0;
}

.call-participants__media {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  gap: 0.375rem;
}

.call-participants__icon {
  width: 1rem;
  height: 1rem;
  color: var(--foreground);
}

.call-participants__icon--off {
  color: var(--muted-foreground);
}

.call-participants__control {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  min-width: 0;
  padding-left: 0.25rem;
}

.call-participants__control--disabled {
  opacity: 0.58;
}

.call-participants__volume-icon {
  flex: 0 0 auto;
  width: 1rem;
  height: 1rem;
  color: var(--muted-foreground);
}

.call-participants__hint {
  flex: 0 0 auto;
  color: var(--muted-foreground);
  font-size: 0.75rem;
}

.call-participants__slider-wrap {
  position: relative;
  flex: 1 1 auto;
  min-width: 4rem;
  display: flex;
  align-items: center;
}

.call-participants__slider {
  width: 100%;
  accent-color: var(--primary);
}

.call-participants__tick {
  position: absolute;
  top: 50%;
  width: 1px;
  height: 0.875rem;
  transform: translateY(-50%);
  background: color-mix(in oklab, var(--foreground) 28%, transparent);
  pointer-events: none;
}

.call-participants__percent {
  flex: 0 0 2.25rem;
  color: var(--muted-foreground);
  font-size: 0.75rem;
  text-align: right;
}

.call-participants__footer {
  flex: 0 0 auto;
  border-top: 1px solid var(--border);
  padding: 0.75rem;
}
</style>
