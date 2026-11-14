<script setup lang="ts">
import { useStore } from "@nanostores/vue";
import { VolumeX } from "lucide-vue-next";
import { css } from "styled-system/css";
import { btn } from "styled-system/recipes";
import { $callAudioPlaybackBlocked } from "@/lib/calls/call-audio-playback";
import { useCallAudioPlaybackPromptController } from "@/lib/calls/call-audio-playback-prompt-controller";
import { $callState } from "@/lib/calls/call-store";
import { useCallEngine } from "@/lib/calls/use-call-engine";

const blocked = useStore($callAudioPlaybackBlocked);
const callState = useStore($callState);
const { engine } = useCallEngine();
const {
  enableAudio,
  resumeFailed,
  resuming,
  visible,
} = useCallAudioPlaybackPromptController(blocked, callState, engine);

// HuddleStage "audio paused" strip: quiet surface, one primary action.
const rootClass = css({
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  flexWrap: "wrap",
  margin: "8px 16px",
  padding: "10px 12px",
  borderRadius: "12px",
  bg: "surface",
  border: "1px solid {colors.border}",
  color: "fg",
  fontSize: "13px",
  "& > svg": { width: "20px", height: "20px", flex: "none", color: "fg.muted" },
});
const messageClass = css({ flex: "1 1 240px", minWidth: 0 });
const buttonClass = btn({ variant: "primary", size: "sm" });
</script>

<template>
  <div
    v-if="visible"
    :class="rootClass"
    role="alert"
  >
    <VolumeX aria-hidden="true" />
    <span :class="messageClass">
      Audio is paused by your browser.
      <span v-if="resumeFailed">Try again from this button.</span>
    </span>
    <button
      :class="buttonClass"
      type="button"
      :disabled="resuming"
      @click="enableAudio"
    >
      {{ resuming ? "Enabling audio…" : "Tap to enable audio" }}
    </button>
  </div>
</template>
