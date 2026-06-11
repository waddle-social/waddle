<script setup lang="ts">
import { useStore } from "@nanostores/vue";
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
</script>

<template>
  <div
    v-if="visible"
    class="call-audio-playback-prompt"
    role="alert"
  >
    <span class="call-audio-playback-prompt__message">
      Audio is paused by your browser.
      <span v-if="resumeFailed">Try again from this button.</span>
    </span>
    <button
      class="call-audio-playback-prompt__button"
      type="button"
      :disabled="resuming"
      @click="enableAudio"
    >
      {{ resuming ? "Enabling audio…" : "Tap to enable audio" }}
    </button>
  </div>
</template>

<style scoped>
.call-audio-playback-prompt {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--space-sm);
  border-bottom: 1px solid color-mix(in oklab, var(--accent) 35%, var(--border));
  background: color-mix(in oklab, var(--accent) 12%, var(--background));
  padding: 0.5rem 0.75rem;
  color: var(--foreground);
}

.call-audio-playback-prompt__message {
  min-width: 0;
}

.call-audio-playback-prompt__button {
  flex: 0 0 auto;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--background);
  padding: 0.35rem 0.65rem;
  font: inherit;
  color: var(--foreground);
}

.call-audio-playback-prompt__button:hover {
  background: var(--muted);
}

.call-audio-playback-prompt__button:disabled {
  cursor: wait;
  opacity: 0.72;
}
</style>
