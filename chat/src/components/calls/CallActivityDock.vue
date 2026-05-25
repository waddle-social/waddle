<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Hash, MessageCircle, Phone, Video } from "lucide-vue-next";
import {
  $mucCallParticipants,
  mucCallParticipantCounts,
} from "@/lib/calls/muc-call-presence";
import { $dmCallActivities } from "@/lib/calls/dm-call-activity";
import {
  buildCallActivityDockEntries,
  type CallActivityDockEntry,
  type SidebarMode,
} from "@/lib/calls/call-activity-dock";
import type { ChannelSummary } from "@/lib/chat-types";
import type { DmConversation } from "@/lib/xmpp-client";

const props = defineProps<{
  channels: ChannelSummary[];
  conversations: DmConversation[];
  activeChannelId: string | null;
  activeChannelRoomJid?: string | null;
  activePeerJid: string | null;
  sidebarMode: SidebarMode;
  activeChannelJids: Set<string>;
}>();

const emit = defineEmits<{
  selectChannel: [channelId: string | null, roomJid: string];
  selectDm: [peerJid: string];
}>();

const mucCallParticipantsStore = useStore($mucCallParticipants);
const dmCallActivities = useStore($dmCallActivities);

const callParticipantCounts = computed<Record<string, number>>(() => {
  return mucCallParticipantCounts(mucCallParticipantsStore.value);
});

const entries = computed(() => buildCallActivityDockEntries({
  channels: props.channels,
  conversations: props.conversations,
  activeChannelId: props.activeChannelId,
  activeChannelRoomJid: props.activeChannelRoomJid ?? null,
  activePeerJid: props.activePeerJid,
  sidebarMode: props.sidebarMode,
  activeChannelJids: props.activeChannelJids,
  callParticipantCounts: callParticipantCounts.value,
  dmCallActivities: dmCallActivities.value,
}));

function entryStatus(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const noun = entry.participantCount === 1 ? "person" : "people";
    return `${entry.participantCount} ${noun}`;
  }
  if (entry.state === "ringing") {
    if (entry.direction === "outgoing") return "Calling";
    if (entry.direction === "incoming") return "Ringing";
    return "Ringing";
  }
  return "Live";
}

function entryTitle(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    return `Join ${entry.title} call, ${entryStatus(entry)}`;
  }
  return `Open ${entry.title} call, ${entryStatus(entry)}`;
}

function selectEntry(entry: CallActivityDockEntry): void {
  if (entry.kind === "channel") {
    emit("selectChannel", entry.channelId, entry.roomJid);
    return;
  }
  emit("selectDm", entry.peerJid);
}
</script>

<template>
  <div
    v-if="entries.length > 0"
    class="call-activity-dock"
    aria-label="Active calls"
  >
    <div class="call-activity-dock__header">
      <span class="call-activity-dock__pulse" aria-hidden="true" />
      <span class="type-section-label">Calls</span>
    </div>
    <div class="call-activity-dock__list">
      <button
        v-for="entry in entries"
        :key="entry.key"
        type="button"
        class="call-activity-dock__row"
        :class="{ 'call-activity-dock__row--active': entry.isActive }"
        :aria-current="entry.isActive ? 'page' : undefined"
        :title="entryTitle(entry)"
        @click="selectEntry(entry)"
      >
        <span class="call-activity-dock__icon" aria-hidden="true">
          <Hash v-if="entry.kind === 'channel'" class="h-3.5 w-3.5" />
          <Video v-else-if="entry.media.video" class="h-3.5 w-3.5" />
          <MessageCircle v-else-if="entry.state === 'ringing'" class="h-3.5 w-3.5" />
          <Phone v-else class="h-3.5 w-3.5" />
        </span>
        <span class="call-activity-dock__copy">
          <span class="call-activity-dock__title type-control">{{ entry.title }}</span>
          <span class="call-activity-dock__status type-meta">
            {{ entryStatus(entry) }}
          </span>
        </span>
        <span
          v-if="entry.kind === 'channel'"
          class="call-activity-dock__count type-count-badge"
          aria-hidden="true"
        >
          {{ entry.participantCount }}
        </span>
      </button>
    </div>
  </div>
</template>

<style scoped>
.call-activity-dock {
  display: flex;
  flex-shrink: 0;
  flex-direction: column;
  gap: 0.375rem;
  min-height: 0;
  max-height: min(40dvh, 18rem);
  border-top: 1px solid var(--border);
  border-right: 1px solid var(--border);
  background:
    linear-gradient(
      180deg,
      color-mix(in oklab, var(--card) 92%, transparent),
      color-mix(in oklab, var(--sidebar-accent) 28%, var(--card))
    );
  padding: 0.5rem;
}

.call-activity-dock__header {
  display: flex;
  align-items: center;
  gap: 0.375rem;
  padding: 0 0.25rem;
  color: var(--sidebar-muted);
}

.call-activity-dock__pulse {
  width: 0.375rem;
  height: 0.375rem;
  border-radius: 9999px;
  background: oklch(0.7 0.18 145);
  box-shadow: 0 0 0 3px color-mix(in oklab, oklch(0.7 0.18 145) 24%, transparent);
}

.call-activity-dock__list {
  display: grid;
  gap: 0.25rem;
  min-height: 0;
  overflow-y: auto;
}

.call-activity-dock__row {
  display: grid;
  grid-template-columns: 1.5rem minmax(0, 1fr) auto;
  min-height: 2.5rem;
  align-items: center;
  gap: 0.5rem;
  border-radius: var(--radius-sm);
  padding: 0.25rem 0.5rem;
  color: var(--sidebar-muted);
  text-align: left;
  transition:
    background-color 160ms ease-out,
    color 160ms ease-out,
    transform 160ms ease-out;
}

.call-activity-dock__row:hover,
.call-activity-dock__row--active {
  background: color-mix(in oklab, var(--sidebar-accent) 72%, transparent);
  color: var(--sidebar-foreground);
}

.call-activity-dock__row:hover {
  transform: translateY(-1px);
}

.call-activity-dock__row:focus-visible {
  outline: none;
  box-shadow: 0 0 0 2px color-mix(in oklab, var(--primary) 42%, transparent);
}

.call-activity-dock__icon {
  display: inline-flex;
  width: 1.5rem;
  height: 1.5rem;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-sm);
  background: color-mix(in oklab, oklch(0.7 0.18 145) 12%, transparent);
  color: oklch(0.54 0.15 145);
}

:global(.dark) .call-activity-dock__icon {
  color: oklch(0.82 0.14 145);
}

.call-activity-dock__copy {
  display: grid;
  min-width: 0;
  gap: 0.0625rem;
}

.call-activity-dock__title,
.call-activity-dock__status {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.call-activity-dock__status {
  color: var(--sidebar-muted);
}

.call-activity-dock__count {
  display: inline-flex;
  min-width: 1.125rem;
  height: 1.125rem;
  align-items: center;
  justify-content: center;
  border-radius: 9999px;
  background: color-mix(in oklab, oklch(0.7 0.18 145) 18%, transparent);
  color: oklch(0.48 0.14 145);
  font-variant-numeric: tabular-nums;
}

.call-activity-dock.call-activity-dock--mobile {
  max-height: none;
  border-top: 0;
  border-right: 0;
  border-bottom: 1px solid var(--border);
  padding: 0.375rem 0.75rem;
}

.call-activity-dock.call-activity-dock--mobile .call-activity-dock__header {
  display: none;
}

.call-activity-dock.call-activity-dock--mobile .call-activity-dock__list {
  display: flex;
  gap: 0.375rem;
  overflow-x: auto;
  overflow-y: hidden;
  overscroll-behavior-x: contain;
  scrollbar-width: none;
}

.call-activity-dock.call-activity-dock--mobile .call-activity-dock__list::-webkit-scrollbar {
  display: none;
}

.call-activity-dock.call-activity-dock--mobile .call-activity-dock__row {
  min-width: min(14rem, 78vw);
}

@media (min-width: 64rem) {
  .call-activity-dock.call-activity-dock--mobile {
    display: none;
  }
}

@media (prefers-reduced-motion: reduce) {
  .call-activity-dock__row {
    transition: none;
  }

  .call-activity-dock__row:hover {
    transform: none;
  }
}
</style>
