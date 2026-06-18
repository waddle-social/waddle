<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import {
  Check,
  LayoutGrid,
  Maximize2,
  MessageSquare,
  Mic,
  MicOff,
  Minimize2,
  MonitorUp,
  MoreHorizontal,
  PhoneOff,
  ScanFace,
  Settings,
  SquareUser,
  Users,
  Video,
  VideoOff,
} from "lucide-vue-next";
import type { CallViewMode } from "@/lib/calls/view-mode";

/**
 * Shared control bar for the in-call surfaces (split + expanded).
 * Holds every call action; the only local state is the open/closed flag
 * of the More ▸ overflow menu — call state stays in the parent's
 * `call-controls` store (mic/cam atoms + toggle functions).
 *
 * Status read-outs (connection quality, the live timer) live in the
 * stage-header now, not here, so this bar is actions-only. Less-used
 * actions (Settings, and future devices/diagnostics entries) live under
 * the More ▸ overflow to keep the bar uncluttered.
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
  /** True while the local self-view tile is hidden from this client's stage. */
  selfViewHidden: boolean;
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
  toggleSelfView: [];
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

/**
 * More ▸ overflow menu (WAI-ARIA menu-button pattern). The trigger
 * carries `aria-haspopup="menu"` + `aria-expanded`; the menu is a
 * `role="menu"` of `menuitem` buttons. Opening focuses the first item;
 * Escape, an outside click, or selecting an item closes it. Selecting an
 * item routes focus to whatever it opens (e.g. the settings dialog),
 * while Escape returns focus to the trigger.
 */
const moreOpen = ref(false);
const moreWrapEl = ref<HTMLElement | null>(null);
const moreTriggerEl = ref<HTMLButtonElement | null>(null);
const moreMenuEl = ref<HTMLElement | null>(null);

function menuItems(): HTMLElement[] {
  const root = moreMenuEl.value;
  if (!root) return [];
  // Includes the checkbox item (Self-view) alongside the plain action items so
  // every entry stays in the arrow-key roving-focus order.
  return Array.from(
    root.querySelectorAll<HTMLElement>('[role="menuitem"],[role="menuitemcheckbox"]'),
  );
}

async function openMore(focusFirst: boolean): Promise<void> {
  moreOpen.value = true;
  if (!focusFirst) return;
  await nextTick();
  menuItems()[0]?.focus();
}

function closeMore(returnFocus: boolean): void {
  if (!moreOpen.value) return;
  moreOpen.value = false;
  if (returnFocus) moreTriggerEl.value?.focus();
}

function onMoreTriggerKeydown(event: KeyboardEvent): void {
  if (event.key === "ArrowDown" || event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    void openMore(true);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    void openMore(true).then(() => menuItems().at(-1)?.focus());
  } else if (event.key === "Escape") {
    closeMore(true);
  }
}

function onMoreMenuKeydown(event: KeyboardEvent): void {
  const items = menuItems();
  if (items.length === 0) return;
  const index = items.indexOf(document.activeElement as HTMLElement);
  switch (event.key) {
    case "Escape":
      event.preventDefault();
      closeMore(true);
      break;
    case "Tab":
      // Leaving the menu by Tab dismisses it without stealing focus.
      closeMore(false);
      break;
    case "ArrowDown":
      event.preventDefault();
      items[(index + 1) % items.length]?.focus();
      break;
    case "ArrowUp":
      event.preventDefault();
      items[(index - 1 + items.length) % items.length]?.focus();
      break;
    case "Home":
      event.preventDefault();
      items[0]?.focus();
      break;
    case "End":
      event.preventDefault();
      items.at(-1)?.focus();
      break;
  }
}

function selectSettings(): void {
  emit("openSettings");
  // Focus moves into the dialog that opens — don't yank it back to the trigger.
  closeMore(false);
}

function selectSelfView(): void {
  emit("toggleSelfView");
  // Toggling self-view leaves no new surface to receive focus, so return it to
  // the trigger as the menu closes.
  closeMore(true);
}

function onDocumentPointerDown(event: PointerEvent): void {
  const target = event.target as Node | null;
  if (target && moreWrapEl.value?.contains(target)) return;
  closeMore(false);
}

watch(moreOpen, (open) => {
  if (typeof document === "undefined") return;
  if (open) {
    document.addEventListener("pointerdown", onDocumentPointerDown, true);
  } else {
    document.removeEventListener("pointerdown", onDocumentPointerDown, true);
  }
});

onBeforeUnmount(() => {
  if (typeof document !== "undefined") {
    document.removeEventListener("pointerdown", onDocumentPointerDown, true);
  }
});
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
    <div ref="moreWrapEl" class="call-controls__more">
      <button
        ref="moreTriggerEl"
        type="button"
        class="chat-icon-button chat-icon-button--md hover:bg-muted"
        :class="{ 'bg-muted text-foreground': moreOpen }"
        aria-label="More options"
        aria-haspopup="menu"
        :aria-expanded="moreOpen"
        @click="moreOpen ? closeMore(false) : openMore(true)"
        @keydown="onMoreTriggerKeydown"
      >
        <MoreHorizontal class="w-4 h-4" />
      </button>
      <div
        v-show="moreOpen"
        ref="moreMenuEl"
        class="call-controls__more-menu"
        role="menu"
        aria-label="More options"
        @keydown="onMoreMenuKeydown"
      >
        <button
          type="button"
          role="menuitemcheckbox"
          :aria-checked="!selfViewHidden"
          class="call-controls__more-item"
          @click="selectSelfView"
        >
          <ScanFace class="w-4 h-4" />
          <span>Self-view</span>
          <Check v-if="!selfViewHidden" class="ml-auto w-4 h-4 text-primary" />
        </button>
        <button
          type="button"
          role="menuitem"
          class="call-controls__more-item"
          @click="selectSettings"
        >
          <Settings class="w-4 h-4" />
          <span>Call settings</span>
        </button>
      </div>
    </div>

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

/* More ▸ overflow. The menu floats above the bar (the bar sits at the
 * bottom of both surfaces), anchored to the trigger. */
.call-controls__more {
  position: relative;
  display: inline-flex;
}

.call-controls__more-menu {
  position: absolute;
  bottom: calc(100% + 0.375rem);
  right: 0;
  z-index: 10;
  min-width: 12rem;
  padding: var(--space-2xs);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--popover, var(--background));
  box-shadow: var(--shadow-lg, 0 10px 30px rgb(0 0 0 / 0.18));
}

.call-controls__more-item {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  width: 100%;
  padding: 0.5rem 0.625rem;
  border-radius: var(--radius-sm);
  text-align: start;
  color: var(--foreground);
}

.call-controls__more-item:hover,
.call-controls__more-item:focus-visible {
  background: var(--muted);
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
