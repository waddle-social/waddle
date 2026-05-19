<script setup lang="ts">
import { computed } from "vue";
import {
  Mic,
  MicOff,
  Minimize2,
  PanelRight,
  PhoneOff,
  PictureInPicture,
  PictureInPicture2,
  Settings,
  Video,
  VideoOff,
} from "lucide-vue-next";
import type { CallUiMode } from "@/lib/calls/ui-mode";

/**
 * Shared control bar used by the docked and floating call surfaces.
 * Stateless: every callback is invoked on the parent, which owns
 * the engine + the UI mode store. That way the bar can be embedded
 * inside any container (docked panel, floating window, or — later —
 * a mobile drawer) without ferrying a copy of the engine through
 * the prop tree.
 */
const props = defineProps<{
  micEnabled: boolean;
  camEnabled: boolean;
  /** Whether the toolbar should render the PIP button. False on
   *  browsers that don't expose `HTMLVideoElement.requestPictureInPicture`. */
  pipSupported: boolean;
  mode: CallUiMode;
  /** Whether the cursor is over a video track that can be popped
   *  into PIP. When false we render a friendly disabled state. */
  hasVideoForPip: boolean;
}>();

const emit = defineEmits<{
  toggleMic: [];
  toggleCam: [];
  togglePip: [];
  setMode: [mode: CallUiMode];
  openSettings: [];
  hangup: [];
}>();

const isMinimized = computed(() => props.mode === "minimized");
const isFloating = computed(() => props.mode === "floating");

function dockToggle(): void {
  emit("setMode", isFloating.value ? "docked" : "floating");
}
</script>

<template>
  <div class="call-controls">
    <button
      type="button"
      class="chat-action-button"
      :class="micEnabled ? 'chat-action-button--secondary' : 'chat-action-button--primary'"
      :aria-pressed="!micEnabled"
      :title="micEnabled ? 'Mute' : 'Unmute'"
      @click="emit('toggleMic')"
    >
      <component :is="micEnabled ? Mic : MicOff" class="w-4 h-4" />
      <span class="type-control sr-only sm:not-sr-only">{{ micEnabled ? "Mute" : "Unmute" }}</span>
    </button>
    <button
      type="button"
      class="chat-action-button"
      :class="camEnabled ? 'chat-action-button--secondary' : 'chat-action-button--primary'"
      :aria-pressed="!camEnabled"
      :title="camEnabled ? 'Camera off' : 'Camera on'"
      @click="emit('toggleCam')"
    >
      <component :is="camEnabled ? Video : VideoOff" class="w-4 h-4" />
      <span class="type-control sr-only sm:not-sr-only">{{ camEnabled ? "Off" : "On" }}</span>
    </button>

    <span class="call-controls__divider" aria-hidden="true" />

    <button
      v-if="pipSupported"
      type="button"
      class="chat-icon-button chat-icon-button--md"
      :class="hasVideoForPip ? 'hover:bg-muted' : 'opacity-40 cursor-not-allowed'"
      :disabled="!hasVideoForPip"
      :aria-pressed="mode === 'pip'"
      :title="hasVideoForPip ? 'Picture-in-picture' : 'No video to pop out yet'"
      @click="emit('togglePip')"
    >
      <component :is="mode === 'pip' ? PictureInPicture2 : PictureInPicture" class="w-4 h-4" />
    </button>
    <button
      type="button"
      class="chat-icon-button chat-icon-button--md hover:bg-muted"
      :title="isFloating ? 'Dock to side' : 'Float window'"
      :aria-pressed="isFloating"
      @click="dockToggle"
    >
      <PanelRight class="w-4 h-4" />
    </button>
    <button
      type="button"
      class="chat-icon-button chat-icon-button--md hover:bg-muted"
      :title="isMinimized ? 'Restore' : 'Minimize'"
      :aria-pressed="isMinimized"
      @click="emit('setMode', isMinimized ? 'docked' : 'minimized')"
    >
      <Minimize2 class="w-4 h-4" />
    </button>
    <button
      type="button"
      class="chat-icon-button chat-icon-button--md hover:bg-muted"
      title="Call settings"
      @click="emit('openSettings')"
    >
      <Settings class="w-4 h-4" />
    </button>

    <span class="call-controls__divider" aria-hidden="true" />

    <button
      type="button"
      class="chat-action-button chat-action-button--destructive"
      title="Hang up"
      @click="emit('hangup')"
    >
      <PhoneOff class="w-4 h-4" />
      <span class="type-control sr-only sm:not-sr-only">Hang up</span>
    </button>
  </div>
</template>

<style scoped>
.call-controls {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-xs);
  flex-wrap: wrap;
}

.call-controls__divider {
  width: 1px;
  height: 1.5rem;
  background: var(--border);
  margin-inline: var(--space-xs);
}
</style>
