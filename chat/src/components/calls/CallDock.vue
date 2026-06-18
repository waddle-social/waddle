<script setup lang="ts">
import { computed } from "vue";
import { X } from "lucide-vue-next";
import CallParticipantsPanel from "./CallParticipantsPanel.vue";
import type { CallDockTab } from "@/lib/calls/call-dock-state";
import type { CallRosterRow } from "@/lib/calls/call-roster";
import type { CallVolumeMixerRow } from "@/lib/calls/call-volume-mixer";

/**
 * The in-call **Dock**: a right-side panel that reflows the Expanded
 * stage (and overlays it in Immersive). A single-choice tab strip heads
 * it — **Participants** (roster + volume) and **Chat** (call-thread
 * messages + composer). Stateless: the parent surface owns the dock
 * open/active-tab stores and the roster controller; the dock only
 * forwards intents and renders the Chat panel from the `chat` slot.
 */
const props = defineProps<{
  rows: readonly CallRosterRow[];
  activeTab: CallDockTab;
  chatUnread: number;
}>();

const emit = defineEmits<{
  setVolume: [row: CallVolumeMixerRow, level: number];
  resetAll: [];
  close: [];
  setTab: [tab: CallDockTab];
}>();

const participantCount = computed(() => props.rows.length);

/**
 * Arrow-key navigation across the two tabs, per the WAI-ARIA tabs pattern with
 * roving tabindex: an arrow moves selection to the other tab (every arrow flips
 * with two tabs) and focus follows it.
 */
function onTabKeydown(event: KeyboardEvent): void {
  const isArrow =
    event.key === "ArrowRight" ||
    event.key === "ArrowDown" ||
    event.key === "ArrowLeft" ||
    event.key === "ArrowUp";
  if (!isArrow) return;
  event.preventDefault();
  const next: CallDockTab = props.activeTab === "participants" ? "chat" : "participants";
  emit("setTab", next);
  const current = event.currentTarget as HTMLElement;
  const other = current.nextElementSibling ?? current.previousElementSibling;
  if (other instanceof HTMLElement) other.focus();
}
</script>

<template>
  <aside class="call-dock" role="region" aria-label="Call dock">
    <header class="call-dock__header">
      <div class="call-dock__tabs" role="tablist" aria-label="Dock">
        <button
          id="call-dock-tab-participants"
          type="button"
          role="tab"
          class="call-dock__tab"
          :aria-selected="activeTab === 'participants'"
          aria-controls="call-dock-participants"
          :tabindex="activeTab === 'participants' ? 0 : -1"
          @click="emit('setTab', 'participants')"
          @keydown="onTabKeydown"
        >
          Participants
          <span class="call-dock__count">{{ participantCount }}</span>
        </button>
        <button
          id="call-dock-tab-chat"
          type="button"
          role="tab"
          class="call-dock__tab"
          :aria-selected="activeTab === 'chat'"
          aria-controls="call-dock-chat"
          :tabindex="activeTab === 'chat' ? 0 : -1"
          @click="emit('setTab', 'chat')"
          @keydown="onTabKeydown"
        >
          Chat
          <span
            v-if="chatUnread > 0"
            class="call-dock__unread"
            :aria-label="`${chatUnread} unread message${chatUnread === 1 ? '' : 's'}`"
          >{{ chatUnread }}</span>
        </button>
      </div>
      <button
        type="button"
        class="call-dock__close chat-icon-button chat-icon-button--md hover:bg-muted"
        aria-label="Close dock"
        @click="emit('close')"
      >
        <X class="w-4 h-4" />
      </button>
    </header>
    <div
      id="call-dock-participants"
      v-show="activeTab === 'participants'"
      class="call-dock__body"
      role="tabpanel"
      aria-labelledby="call-dock-tab-participants"
    >
      <CallParticipantsPanel
        :rows="rows"
        @set-volume="(row, level) => emit('setVolume', row, level)"
        @reset-all="emit('resetAll')"
      />
    </div>
    <div
      id="call-dock-chat"
      v-show="activeTab === 'chat'"
      class="call-dock__body"
      role="tabpanel"
      aria-labelledby="call-dock-tab-chat"
    >
      <slot name="chat" />
    </div>
  </aside>
</template>

<style scoped>
.call-dock {
  flex: 0 0 auto;
  width: min(20rem, 34vw);
  min-width: 15rem;
  display: flex;
  flex-direction: column;
  min-height: 0;
  border-left: 1px solid var(--border);
  background: color-mix(in oklab, var(--muted) 20%, var(--background));
}

.call-dock__header {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.5rem;
  padding: 0.5rem 0.5rem 0.5rem 0.75rem;
  border-bottom: 1px solid var(--border);
}

.call-dock__tabs {
  display: flex;
  align-items: center;
  gap: 0.25rem;
  min-width: 0;
}

.call-dock__tab {
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.25rem 0.5rem;
  border-radius: var(--radius-sm);
  color: var(--foreground);
  font: inherit;
  font-weight: 600;
}

.call-dock__tab[aria-selected="true"] {
  background: var(--muted);
}

.call-dock__count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 1.25rem;
  height: 1.25rem;
  padding: 0 0.375rem;
  border-radius: 9999px;
  background: color-mix(in oklab, var(--foreground) 12%, transparent);
  color: var(--muted-foreground);
  font-size: 0.75rem;
}

/* The Chat tab's unread badge — a small accent pill so a new message is
 * noticeable even when the dock is parked on the Participants tab. */
.call-dock__unread {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 1.25rem;
  height: 1.25rem;
  padding: 0 0.375rem;
  border-radius: 9999px;
  background: var(--primary);
  color: var(--primary-foreground);
  font-size: 0.75rem;
  font-variant-numeric: tabular-nums;
}

.call-dock__close {
  flex: 0 0 auto;
}

.call-dock__body {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

@media (max-width: 760px) {
  .call-dock {
    width: 100%;
    min-width: 0;
    max-height: 18rem;
    border-left: 0;
    border-top: 1px solid var(--border);
  }
}
</style>
