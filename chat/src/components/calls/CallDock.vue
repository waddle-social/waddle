<script setup lang="ts">
import { computed } from "vue";
import { X } from "lucide-vue-next";
import CallParticipantsPanel from "./CallParticipantsPanel.vue";
import type { CallRosterRow } from "@/lib/calls/call-roster";
import type { CallVolumeMixerRow } from "@/lib/calls/call-volume-mixer";

/**
 * The in-call **Dock**: a right-side panel that reflows the Expanded
 * stage. A single-choice tab strip (currently just **Participants**)
 * heads it so future tabs (e.g. call chat) can slot in without changing
 * the shell. Stateless — the parent surface owns the roster controller
 * and the open/close store; the dock only forwards intents.
 */
const props = defineProps<{
  rows: readonly CallRosterRow[];
}>();

const emit = defineEmits<{
  setVolume: [row: CallVolumeMixerRow, level: number];
  resetAll: [];
  close: [];
}>();

const participantCount = computed(() => props.rows.length);
</script>

<template>
  <aside class="call-dock" role="region" aria-label="Participants dock">
    <header class="call-dock__header">
      <div class="call-dock__tabs" role="tablist" aria-label="Dock">
        <button
          type="button"
          role="tab"
          class="call-dock__tab"
          aria-selected="true"
          aria-controls="call-dock-participants"
        >
          Participants
          <span class="call-dock__count">{{ participantCount }}</span>
        </button>
      </div>
      <button
        type="button"
        class="call-dock__close chat-icon-button chat-icon-button--md hover:bg-muted"
        aria-label="Close participants"
        @click="emit('close')"
      >
        <X class="w-4 h-4" />
      </button>
    </header>
    <div id="call-dock-participants" class="call-dock__body" role="tabpanel">
      <CallParticipantsPanel
        :rows="rows"
        @set-volume="(row, level) => emit('setVolume', row, level)"
        @reset-all="emit('resetAll')"
      />
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
