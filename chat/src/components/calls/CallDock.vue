<script setup lang="ts">
import { computed } from "vue";
import { X } from "lucide-vue-next";
import { css } from "styled-system/css";
import { badge, iconbtn, tab, tabs } from "styled-system/recipes";
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

// HuddleStage board "Call dock": a floe on the dark stage.
const rootClass = css({
  flex: "0 0 320px",
  minWidth: 0,
  minHeight: 0,
  display: "flex",
  flexDirection: "column",
  borderRadius: "panel",
  bg: "surface",
  border: "1px solid {colors.border.soft}",
  overflow: "hidden",
  color: "fg",
  "@media (max-width: 760px)": { flexBasis: "auto", width: "100%", maxHeight: "18rem" },
});
const headerClass = css({ flex: "none", display: "flex", alignItems: "center", gap: "1", padding: "0 {spacing.2} 0 {spacing.1}", borderBottom: "1px solid {colors.border.soft}" });
const tabsClass = css({ flex: 1, minWidth: 0, borderBottom: 0 });
const bodyClass = css({ flex: "1 1 auto", minHeight: 0, display: "flex", flexDirection: "column" });
const countClass = badge({ variant: "quiet" });
const unreadClass = badge();
</script>

<template>
  <aside :class="rootClass" role="region" aria-label="Call dock">
    <header :class="headerClass">
      <div :class="[tabs(), tabsClass]" role="tablist" aria-label="Dock">
        <button
          id="call-dock-tab-participants"
          type="button"
          role="tab"
          :class="tab()"
          :aria-selected="activeTab === 'participants'"
          aria-controls="call-dock-participants"
          :tabindex="activeTab === 'participants' ? 0 : -1"
          @click="emit('setTab', 'participants')"
          @keydown="onTabKeydown"
        >
          Participants
          <span :class="countClass">{{ participantCount }}</span>
        </button>
        <button
          id="call-dock-tab-chat"
          type="button"
          role="tab"
          :class="tab()"
          :aria-selected="activeTab === 'chat'"
          aria-controls="call-dock-chat"
          :tabindex="activeTab === 'chat' ? 0 : -1"
          @click="emit('setTab', 'chat')"
          @keydown="onTabKeydown"
        >
          Chat
          <span
            v-if="chatUnread > 0"
            :class="unreadClass"
            :aria-label="`${chatUnread} unread message${chatUnread === 1 ? '' : 's'}`"
          >{{ chatUnread }}</span>
        </button>
      </div>
      <button
        type="button"
        :class="iconbtn({ size: 'sm' })"
        aria-label="Close dock"
        @click="emit('close')"
      >
        <X aria-hidden="true" />
      </button>
    </header>
    <div
      id="call-dock-participants"
      v-show="activeTab === 'participants'"
      :class="bodyClass"
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
      :class="bodyClass"
      role="tabpanel"
      aria-labelledby="call-dock-tab-chat"
    >
      <slot name="chat" />
    </div>
  </aside>
</template>
