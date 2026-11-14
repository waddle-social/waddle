<script setup lang="ts">
import { ref, watch } from "vue";
import { Hand, Mic, MicOff, Video, VideoOff, Volume2, VolumeX } from "lucide-vue-next";
import {
  callVolumePercentToGain,
  type CallVolumeMixerRow,
} from "@/lib/calls/call-volume-mixer";
import type { CallRosterRow } from "@/lib/calls/call-roster";
import { css } from "styled-system/css";
import { btn, liveDot } from "styled-system/recipes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { barePeerJid } from "@/lib/xmpp/jid";

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

// Roster rows re-emit on EVERY attendee change — mic/cam toggles and
// active-speaker flips, not just volume — so a wholesale rebuild here would
// clobber an in-flight slider echo mid-drag and break the detent direction.
// Keep any value we've already emitted for a still-present slider; only adopt
// the prop level for sliders we haven't touched, and drop vanished keys.
function syncLastEmittedLevels(rows: readonly CallRosterRow[]): void {
  const previous = lastEmittedLevels.value;
  const next: Record<string, number> = {};
  for (const row of rows) {
    for (const volume of row.volumeRows) {
      next[volume.key] = previous[volume.key] ?? volume.level;
    }
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

function avatarJid(identity: string): string | null {
  return identity.includes("@") ? barePeerJid(identity) : null;
}

// HuddleStage board dock roster: 28px avatar · name · hand/mic/cam, then
// the volume slider(s) with a % readout under the name.
const rootClass = css({ display: "flex", flexDirection: "column", minHeight: 0, height: "100%" });
const listClass = css({ flex: "1 1 auto", minHeight: 0, overflowY: "auto", listStyle: "none", margin: 0, padding: "6px", display: "flex", flexDirection: "column", gap: "0.5" });
const rowClass = css({
  display: "grid",
  gridTemplateColumns: "28px minmax(0, 1fr) auto",
  alignItems: "center",
  gap: "4px 10px",
  padding: "8px 10px",
  borderRadius: "control",
  _hover: { bg: "state.hover" },
});
const nameCellClass = css({ display: "flex", alignItems: "center", gap: "1.5", minWidth: 0 });
const nameClass = css({ minWidth: 0, font: "700 14px/1.3 {fonts.body}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const mediaClass = css({ display: "inline-flex", alignItems: "center", gap: "1.5", color: "fg.muted", "& svg": { width: "16px", height: "16px", flex: "none" } });
const handClass = css({ color: "gold.text" });
const offClass = css({ color: "danger.text" });
const controlClass = css({
  gridColumn: "2 / -1",
  display: "flex",
  alignItems: "center",
  gap: "2",
  minWidth: 0,
  fontSize: "12px",
  color: "fg.muted",
  "&[data-disabled]": { opacity: 0.58 },
  "& > svg": { width: "16px", height: "16px", flex: "none" },
});
const sliderWrapClass = css({ position: "relative", flex: "1 1 auto", minWidth: "4rem", display: "flex", alignItems: "center" });
const sliderClass = css({ width: "100%", accentColor: "action", _disabled: { cursor: "not-allowed" } });
const tickClass = css({ position: "absolute", top: "50%", width: "1px", height: "14px", transform: "translateY(-50%)", bg: "border.strong", pointerEvents: "none" });
const percentClass = css({ flex: "0 0 36px", textAlign: "right", fontVariantNumeric: "tabular-nums" });
const footerClass = css({ flex: "none", borderTop: "1px solid {colors.border.soft}", padding: "3" });
</script>

<template>
  <div :class="rootClass" aria-label="Participants">
    <ul :class="listClass">
      <li
        v-for="row in rows"
        :key="row.key"
        :class="rowClass"
        :data-speaking="row.speaking || undefined"
      >
        <UserAvatar :name="row.label" :jid="avatarJid(row.identity)" size="sm" :speaking="row.speaking" />
        <span :class="nameCellClass">
          <span :class="nameClass">{{ row.label }}</span>
          <span v-if="row.speaking" :class="liveDot()" aria-label="Speaking" />
        </span>
        <span :class="mediaClass">
          <Hand
            v-if="row.raisedHand"
            :class="handClass"
            aria-label="Hand raised"
          />
          <component
            :is="row.micOn ? Mic : MicOff"
            :class="row.micOn ? undefined : offClass"
            :aria-label="row.micOn ? 'Microphone on' : 'Microphone off'"
          />
          <component
            :is="row.cameraOn ? Video : VideoOff"
            :aria-label="row.cameraOn ? 'Camera on' : 'Camera off'"
          />
        </span>

        <div
          v-for="volume in row.volumeRows"
          :key="volume.key"
          :class="controlClass"
          :data-disabled="volume.disabled || undefined"
        >
          <VolumeX
            v-if="volume.muted"
            aria-label="Muted"
          />
          <Volume2
            v-else
            aria-hidden="true"
          />
          <span v-if="volume.hint">{{ volume.hint }}</span>
          <div :class="sliderWrapClass">
            <input
              :class="sliderClass"
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
            <span :class="tickClass" aria-hidden="true" style="left:50%;" />
          </div>
          <span :class="percentClass">{{ Math.round(volume.level * 100) }}%</span>
        </div>
      </li>
    </ul>

    <footer :class="footerClass">
      <button
        type="button"
        :class="btn({ size: 'sm' })"
        @click="onResetAll"
      >
        Reset all
      </button>
    </footer>
  </div>
</template>
