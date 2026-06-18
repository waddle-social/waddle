<script setup lang="ts">
import { computed } from "vue";
import {
  LayoutGrid,
  Maximize2,
  MessageSquare,
  Mic,
  MicOff,
  Minimize2,
  MonitorUp,
  PhoneOff,
  Settings,
  SquareUser,
  Users,
  Video,
  VideoOff,
} from "lucide-vue-next";
import type { CallViewMode } from "@/lib/calls/view-mode";
import CallConnectionIndicator from "./CallConnectionIndicator.vue";

/**
 * Shared control bar for the in-call surfaces (split + expanded).
 * Stateless: every callback is invoked on the parent, which owns the
 * shared `call-controls` store (mic/cam atoms + toggle functions).
 *
 * The old multi-mode chrome (dock-toggle, minimize, PIP) is gone —
 * the only mode switch left is split ↔ expanded.
 */
const props = defineProps<{
  micEnabled: boolean;
  camEnabled: boolean;
  screenShareEnabled: boolean;
  screenShareSupported: boolean;
  /** True when the parent surface is the expanded variant — flips
   *  the toggle button between "expand" and "collapse". */
  isExpanded: boolean;
  /** True while the Participants dock is open — drives the participants
   *  button's pressed/expanded state. The parent owns the dock store. */
  participantsOpen: boolean;
  /** Attendee count shown as a badge on the participants button. */
  participantCount: number;
  /** True while the dock is open on the Chat tab — drives the chat
   *  button's pressed/expanded state. The parent owns the dock store. */
  chatOpen: boolean;
  /** Unread call-chat messages shown as a badge on the chat button. */
  chatUnread: number;
  /** The chosen stage layout — drives the view switcher's pressed state. */
  viewMode: CallViewMode;
}>();

const emit = defineEmits<{
  toggleMic: [];
  toggleCam: [];
  toggleScreenShare: [];
  toggleExpanded: [];
  toggleParticipants: [];
  toggleChat: [];
  openSettings: [];
  setViewMode: [mode: CallViewMode];
  hangup: [];
}>();

// The button's own `aria-label` overrides the accessible-name calculation, so a
// nested badge (even with its own label) is never announced. Fold the unread
// count into the label and mark the visual badge `aria-hidden`.
const chatButtonLabel = computed(() => {
  const base = props.chatOpen ? "Close chat" : "Open chat";
  if (props.chatUnread <= 0) return base;
  const plural = props.chatUnread === 1 ? "" : "s";
  return `${base}, ${props.chatUnread} unread message${plural}`;
});

/**
 * Arrow-key navigation for the two-option Gallery/Speaker radiogroup, per the
 * WAI-ARIA radio-group pattern: an arrow moves selection to the other option
 * (wrapping at both ends) and focus follows the selection (roving tabindex).
 * With two options every arrow flips the mode, so no redundant emit fires.
 */
function onViewKeydown(event: KeyboardEvent): void {
  const isArrow =
    event.key === "ArrowRight" ||
    event.key === "ArrowDown" ||
    event.key === "ArrowLeft" ||
    event.key === "ArrowUp";
  if (!isArrow) return;
  event.preventDefault();
  const next: CallViewMode = props.viewMode === "gallery" ? "speaker" : "gallery";
  emit("setViewMode", next);
  const current = event.currentTarget as HTMLElement;
  const other = current.nextElementSibling ?? current.previousElementSibling;
  if (other instanceof HTMLElement) other.focus();
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

    <!-- Gallery ⟷ Speaker view switcher. Sticky for the call; a pin or an
         incoming screen share still overrides the chosen layout's large tile.
         A radiogroup (single-choice) rather than two independent toggles. -->
    <div class="call-controls__view-switch" role="radiogroup" aria-label="Stage view">
      <button
        type="button"
        role="radio"
        class="chat-icon-button chat-icon-button--md hover:bg-muted"
        :class="{ 'bg-muted text-foreground': viewMode === 'gallery' }"
        title="Gallery view"
        aria-label="Gallery view"
        :aria-checked="viewMode === 'gallery'"
        :tabindex="viewMode === 'gallery' ? 0 : -1"
        @click="emit('setViewMode', 'gallery')"
        @keydown="onViewKeydown"
      >
        <LayoutGrid class="w-4 h-4" />
      </button>
      <button
        type="button"
        role="radio"
        class="chat-icon-button chat-icon-button--md hover:bg-muted"
        :class="{ 'bg-muted text-foreground': viewMode === 'speaker' }"
        title="Speaker view"
        aria-label="Speaker view"
        :aria-checked="viewMode === 'speaker'"
        :tabindex="viewMode === 'speaker' ? 0 : -1"
        @click="emit('setViewMode', 'speaker')"
        @keydown="onViewKeydown"
      >
        <SquareUser class="w-4 h-4" />
      </button>
    </div>

    <!-- Ambient self-connection quality. Self-contained/store-connected,
         so it adds no props to this otherwise stateless bar. Shows quiet
         measuring bars until the first quality sample arrives. -->
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
      class="call-controls__participants chat-icon-button chat-icon-button--md hover:bg-muted"
      :class="{ 'bg-muted text-foreground': participantsOpen }"
      :title="participantsOpen ? 'Close participants' : 'Open participants'"
      :aria-label="participantsOpen ? 'Close participants' : 'Open participants'"
      :aria-pressed="participantsOpen"
      :aria-expanded="participantsOpen"
      @click="emit('toggleParticipants')"
    >
      <Users class="w-4 h-4" />
      <span
        v-if="participantCount > 0"
        class="call-controls__count"
        aria-hidden="true"
      >{{ participantCount }}</span>
    </button>
    <button
      type="button"
      class="call-controls__chat chat-icon-button chat-icon-button--md hover:bg-muted"
      :class="{ 'bg-muted text-foreground': chatOpen }"
      :title="chatOpen ? 'Close chat' : 'Open chat'"
      :aria-label="chatButtonLabel"
      :aria-pressed="chatOpen"
      :aria-expanded="chatOpen"
      @click="emit('toggleChat')"
    >
      <MessageSquare class="w-4 h-4" />
      <span
        v-if="chatUnread > 0"
        class="call-controls__unread"
        aria-hidden="true"
      >{{ chatUnread }}</span>
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

.call-controls__view-switch {
  display: flex;
  align-items: center;
  gap: 2px;
}

/* The participants toggle carries a live attendee count, so it widens
 * from the square icon-button into a pill with the badge beside the icon. */
.call-controls__participants {
  width: auto;
  gap: 0.375rem;
  padding-inline: 0.5rem;
}

.call-controls__count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 1.125rem;
  height: 1.125rem;
  padding: 0 0.3125rem;
  border-radius: 9999px;
  background: color-mix(in oklab, var(--foreground) 14%, transparent);
  font-size: 0.6875rem;
  font-variant-numeric: tabular-nums;
}

/* The chat toggle widens into a pill when an unread badge rides on it,
 * mirroring the participants toggle. */
.call-controls__chat {
  width: auto;
  gap: 0.375rem;
  padding-inline: 0.5rem;
}

.call-controls__unread {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 1.125rem;
  height: 1.125rem;
  padding: 0 0.3125rem;
  border-radius: 9999px;
  background: var(--primary);
  color: var(--primary-foreground);
  font-size: 0.6875rem;
  font-variant-numeric: tabular-nums;
}
</style>
