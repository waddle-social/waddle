<script setup lang="ts">
import {
  Maximize2,
  Mic,
  MicOff,
  Minimize2,
  MonitorUp,
  PhoneOff,
  Settings,
  Video,
  VideoOff,
  Volume2,
} from "lucide-vue-next";
import CallConnectionIndicator from "./CallConnectionIndicator.vue";

/**
 * Shared control bar for the in-call surfaces (split + expanded).
 * Stateless: every callback is invoked on the parent, which owns the
 * shared `call-controls` store (mic/cam atoms + toggle functions).
 *
 * The old multi-mode chrome (dock-toggle, minimize, PIP) is gone —
 * the only mode switch left is split ↔ expanded.
 */
defineProps<{
  micEnabled: boolean;
  camEnabled: boolean;
  screenShareEnabled: boolean;
  screenShareSupported: boolean;
  /** True when the parent surface is the expanded variant — flips
   *  the toggle button between "expand" and "collapse". */
  isExpanded: boolean;
  /** True while the volume mixer dialog is open — drives the speaker
   *  button's pressed/expanded state. The parent owns the dialog. */
  volumeOpen: boolean;
}>();

const emit = defineEmits<{
  toggleMic: [];
  toggleCam: [];
  toggleScreenShare: [];
  toggleExpanded: [];
  toggleVolume: [];
  openSettings: [];
  hangup: [];
}>();
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
    <button
      v-if="screenShareSupported"
      type="button"
      class="chat-action-button"
      :class="screenShareEnabled ? 'chat-action-button--primary' : 'chat-action-button--secondary'"
      :aria-pressed="screenShareEnabled"
      :title="screenShareEnabled ? 'Stop sharing' : 'Share screen'"
      @click="emit('toggleScreenShare')"
    >
      <MonitorUp class="w-4 h-4" />
      <span class="type-control sr-only sm:not-sr-only">{{ screenShareEnabled ? "Stop" : "Share" }}</span>
    </button>

    <!-- Ambient self-connection quality. Self-contained/store-connected,
         so it adds no props to this otherwise stateless bar. Hides itself
         until the first quality sample arrives. -->
    <CallConnectionIndicator />

    <span class="call-controls__divider" aria-hidden="true" />

    <button
      type="button"
      class="chat-icon-button chat-icon-button--md hover:bg-muted"
      :title="isExpanded ? 'Collapse call' : 'Expand call'"
      :aria-pressed="isExpanded"
      :aria-label="isExpanded ? 'Collapse call to split view' : 'Expand call to fill the chat pane'"
      @click="emit('toggleExpanded')"
    >
      <component :is="isExpanded ? Minimize2 : Maximize2" class="w-4 h-4" />
    </button>
    <button
      type="button"
      class="chat-icon-button chat-icon-button--md hover:bg-muted"
      :class="{ 'bg-muted text-foreground': volumeOpen }"
      :title="volumeOpen ? 'Close volume mixer' : 'Open volume mixer'"
      :aria-label="volumeOpen ? 'Close volume mixer' : 'Open volume mixer'"
      :aria-pressed="volumeOpen"
      :aria-expanded="volumeOpen"
      aria-haspopup="dialog"
      @click="emit('toggleVolume')"
    >
      <Volume2 class="w-4 h-4" />
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
