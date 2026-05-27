<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { ArrowRight, Hash, MessageCircle, Phone, PhoneIncoming, PhoneOff, PhoneOutgoing, Video } from "lucide-vue-next";
import {
  $mucCallParticipants,
  mucCallParticipantCounts,
  normalizeMucCallRoomJid,
} from "@/lib/calls/muc-call-presence";
import { $callState } from "@/lib/calls/call-store";
import { $dmCallActivities, dmCallResumeBlockReason } from "@/lib/calls/dm-call-activity";
import { mucCallParticipantPreview } from "@/lib/calls/muc-call-indicators";
import {
  callActivityDockAction,
  buildCallActivityDockEntries,
  canEndRecoveredDmCallActivity,
  callActivityDockSelection,
  type CallActivityDockEntry,
  type SidebarMode,
} from "@/lib/calls/call-activity-dock";
import { readRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import type { CallMedia } from "@/lib/calls/types";
import type { ChannelSummary } from "@/lib/chat-types";
import type { DmConversation } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp/jid";

defineOptions({ inheritAttrs: false });

const props = withDefaults(defineProps<{
  channels: ChannelSummary[];
  conversations: DmConversation[];
  activeChannelId: string | null;
  activeChannelRoomJid?: string | null;
  activePeerJid: string | null;
  sidebarMode: SidebarMode;
  activeChannelJids: Set<string>;
  callParticipants?: Record<string, readonly string[]>;
  callMediaByRoom?: Record<string, CallMedia>;
  managedMucDomain?: string | null;
  selfFullJid?: string | null;
  showDmCalls?: boolean;
  hideCurrentCall?: boolean;
}>(), {
  showDmCalls: true,
  hideCurrentCall: false,
});

const emit = defineEmits<{
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  selectChannel: [channelId: string | null, roomJid: string];
  leaveChannelCall: [roomJid: string];
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  selectDm: [peerJid: string];
  reconnectDm: [peerJid: string, media: CallMedia];
  endDm: [peerJid: string, sid?: string];
}>();

const mucCallParticipantsStore = useStore($mucCallParticipants);
const callState = useStore($callState);
const dmCallActivities = useStore($dmCallActivities);

const callParticipants = computed(() => props.callParticipants ?? mucCallParticipantsStore.value);
const callParticipantCounts = computed<Record<string, number>>(() => {
  return mucCallParticipantCounts(callParticipants.value);
});

const entries = computed(() => buildCallActivityDockEntries({
  channels: props.channels,
  conversations: props.conversations,
  activeChannelId: props.activeChannelId,
  activeChannelRoomJid: props.activeChannelRoomJid ?? null,
  activePeerJid: props.activePeerJid,
  sidebarMode: props.sidebarMode,
  activeChannelJids: props.activeChannelJids,
  managedMucDomain: props.managedMucDomain ?? null,
  callParticipantCounts: callParticipantCounts.value,
  callParticipants: callParticipants.value,
  callMediaByRoom: props.callMediaByRoom,
  dmCallActivities: props.showDmCalls === false ? {} : dmCallActivities.value,
}));
const visibleEntries = computed(() =>
  props.hideCurrentCall
    ? entries.value.filter((entry) => !isCurrentCallEntry(entry))
    : entries.value
);
const entryCount = computed(() => visibleEntries.value.length);
const currentCallHidden = computed(() => {
  if (!props.hideCurrentCall) return false;
  const current = callState.value;
  if (current.phase === "muc-pending") return current.kind === "muc";
  if (current.phase !== "active") return false;
  if (current.kind === "muc") return true;
  return props.showDmCalls !== false && current.kind === "dm";
});
const statusMessage = computed(() => {
  const count = entryCount.value;
  const qualifier = props.showDmCalls === false ? "group " : "";
  const other = currentCallHidden.value ? "other " : "";
  if (count === 0) {
    return `No ${other}active ${qualifier}calls.`;
  }
  return `${count} ${other}active ${qualifier}${count === 1 ? "call" : "calls"}.`;
});

function isCurrentCallEntry(entry: CallActivityDockEntry): boolean {
  const current = callState.value;
  if (entry.kind === "channel") {
    if (current.phase !== "active" && current.phase !== "muc-pending") return false;
    if (current.kind !== "muc") return false;
    return normalizeMucCallRoomJid(entry.roomJid) === normalizeMucCallRoomJid(current.peer);
  }
  if (current.phase !== "active" || current.kind !== "dm") return false;
  return entry.peerJid.toLowerCase() === barePeerJid(current.peer).toLowerCase() &&
    entry.sid === current.sid;
}

function entryStatus(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const noun = entry.participantCount === 1 ? "person" : "people";
    return `${entry.participantCount} ${noun}`;
  }
  if (entry.state === "accepted") return acceptedDmEntryStatus(entry);
  if (entry.state === "ringing") {
    if (entry.direction === "outgoing") return "Calling";
    if (entry.direction === "incoming") return "Ringing";
    return "Ringing";
  }
  return "Live";
}

function acceptedDmEntryStatus(entry: Extract<CallActivityDockEntry, { kind: "dm" }>): string {
  const reason = dmCallResumeBlockReason(entry, props.selfFullJid ?? null);
  if (reason === null) return "Live";
  if (reason === "other-resource") return "Other device";
  if (reason === "expired-token" || reason === "invalid-token") {
    return entryCanEndDm(entry) ? "End available" : "Expired";
  }
  return "Details pending";
}

function entryKindLabel(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const media = entry.media.video ? "Group video call" : "Group call";
    return entry.isKnownChannel ? media : `${media} · syncing`;
  }
  if (entry.mediaKnown === false) return "Call";
  return entry.media.video ? "Video call" : "Voice call";
}

function entryMeta(entry: CallActivityDockEntry): string {
  return `${entryKindLabel(entry)} · ${entryStatus(entry)}`;
}

function entryParticipantPreview(entry: CallActivityDockEntry): string {
  if (entry.kind !== "channel") return "";
  return mucCallParticipantPreview(entry.participantLabels);
}

function entryAction(entry: CallActivityDockEntry): string {
  switch (callActivityDockAction(entry, callState.value, props.selfFullJid ?? null)) {
    case "answer":
      return "Answer";
    case "join":
      return entry.kind === "channel" && entryCanLeaveChannel(entry) ? "Rejoin" : "Join";
    case "return":
      return "Return";
    case "reconnect":
      return "Reconnect";
    case "open":
      return "Open";
  }
}

function entryTitle(entry: CallActivityDockEntry): string {
  const participants = entryParticipantPreview(entry);
  const participantPart = participants ? `, ${participants} in call` : "";
  return `${entryAction(entry)} ${entry.title}, ${entryMeta(entry)}${participantPart}`;
}

function entryCanSelect(entry: CallActivityDockEntry): boolean {
  return entry.kind !== "channel" || entry.roomJid.length > 0;
}

function entryCanEndDm(entry: CallActivityDockEntry): boolean {
  return entry.kind === "dm" &&
    canEndRecoveredDmCallActivity(entry, callState.value, props.selfFullJid ?? null);
}

function entryCanLeaveChannel(entry: CallActivityDockEntry): boolean {
  if (entry.kind !== "channel" || !entry.roomJid) return false;
  if (currentMucCallRoomJid() === normalizeMucCallRoomJid(entry.roomJid)) return false;
  return readRoomHasActiveCall(entry.roomJid).localResourceInCall;
}

function currentMucCallRoomJid(): string {
  const current = callState.value;
  if (current.phase !== "active" && current.phase !== "muc-pending") return "";
  if (current.kind !== "muc") return "";
  return normalizeMucCallRoomJid(current.peer);
}

function entryCanEnd(entry: CallActivityDockEntry): boolean {
  return entryCanEndDm(entry) || entryCanLeaveChannel(entry);
}

function endEntryLabel(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") return `Leave ${entry.title} call`;
  return endDmLabel(entry);
}

function endEntry(entry: CallActivityDockEntry): void {
  if (entry.kind === "channel" && entryCanLeaveChannel(entry)) {
    emit("leaveChannelCall", entry.roomJid);
    return;
  }
  if (entry.kind === "dm" && entryCanEndDm(entry)) {
    emit("endDm", entry.peerJid, entry.sid);
  }
}

function endDmLabel(entry: CallActivityDockEntry): string {
  if (entry.kind !== "dm") return "";
  const media = entry.mediaKnown !== false
    ? `${entry.media.video ? "video" : "voice"} call`
    : "call";
  return `End ${entry.title} ${media}`;
}

function visibleParticipantLabels(entry: CallActivityDockEntry): string[] {
  if (entry.kind !== "channel") return [];
  return entry.participantLabels.slice(0, 3);
}

function participantInitial(label: string): string {
  return label.trim().charAt(0).toUpperCase() || "?";
}

function selectEntry(entry: CallActivityDockEntry): void {
  if (!entryCanSelect(entry)) return;
  const selection = callActivityDockSelection(entry, callState.value, props.selfFullJid ?? null);
  switch (selection.kind) {
    case "dm-answer":
      emit("answerDm", selection.peerJid, selection.remoteFullJid, selection.sid, selection.media);
      return;
    case "channel-join":
      emit("joinChannelCall", selection.channelId, selection.roomJid, selection.media);
      return;
    case "channel":
      emit("selectChannel", selection.channelId, selection.roomJid);
      return;
    case "dm-reconnect":
      emit("reconnectDm", selection.peerJid, selection.media);
      return;
    case "dm-open":
      emit("selectDm", selection.peerJid);
      return;
  }
}
</script>

<template>
  <div class="call-activity-dock-host">
    <p class="sr-only" role="status" aria-live="polite" aria-atomic="true">
      {{ statusMessage }}
    </p>
    <section
      v-if="visibleEntries.length > 0"
      class="call-activity-dock"
      :class="$attrs.class"
      role="region"
      aria-label="Active calls"
    >
      <div class="call-activity-dock__header">
        <span class="call-activity-dock__pulse" aria-hidden="true" />
        <span class="type-section-label">Active calls</span>
        <span class="call-activity-dock__total type-count-badge" aria-hidden="true">
          {{ entryCount }}
        </span>
      </div>
      <div class="call-activity-dock__list">
        <div
          v-for="entry in visibleEntries"
          :key="entry.key"
          class="call-activity-dock__row-shell"
          :class="{
            'call-activity-dock__row-shell--active': entry.isActive,
            'call-activity-dock__row-shell--disabled': !entryCanSelect(entry),
          }"
        >
          <button
            type="button"
            class="call-activity-dock__row"
            :disabled="!entryCanSelect(entry)"
            :aria-current="entry.isActive ? 'page' : undefined"
            :title="entryTitle(entry)"
            :aria-label="entryTitle(entry)"
            @click="selectEntry(entry)"
          >
            <span class="call-activity-dock__icon" aria-hidden="true">
              <Video v-if="entry.kind === 'channel' && entry.media.video" class="h-3.5 w-3.5" />
              <Hash v-else-if="entry.kind === 'channel'" class="h-3.5 w-3.5" />
              <Video v-else-if="entry.mediaKnown !== false && entry.media.video" class="h-3.5 w-3.5" />
              <PhoneIncoming v-else-if="entry.state === 'ringing' && entry.direction === 'incoming'" class="h-3.5 w-3.5" />
              <PhoneOutgoing v-else-if="entry.state === 'ringing' && entry.direction === 'outgoing'" class="h-3.5 w-3.5" />
              <MessageCircle v-else-if="entry.state === 'ringing'" class="h-3.5 w-3.5" />
              <Phone v-else class="h-3.5 w-3.5" />
            </span>
            <span class="call-activity-dock__copy">
              <span class="call-activity-dock__title type-control">{{ entry.title }}</span>
              <span class="call-activity-dock__status type-meta">
                {{ entryMeta(entry) }}
              </span>
              <span
                v-if="entryParticipantPreview(entry)"
                class="call-activity-dock__participants type-meta"
                :title="`${entryParticipantPreview(entry)} in call`"
              >
                <span class="call-activity-dock__participant-stack" aria-hidden="true">
                  <span
                    v-for="label in visibleParticipantLabels(entry)"
                    :key="`${entry.key}:${label}`"
                    class="call-activity-dock__participant"
                  >
                    {{ participantInitial(label) }}
                  </span>
                </span>
                <span class="call-activity-dock__participant-copy">
                  {{ entryParticipantPreview(entry) }}
                </span>
              </span>
            </span>
            <span
              v-if="entry.kind === 'channel'"
              class="call-activity-dock__count type-count-badge"
              aria-hidden="true"
            >
              {{ entry.participantCount }}
            </span>
            <span class="call-activity-dock__action type-meta" aria-hidden="true">
              {{ entryAction(entry) }}
              <ArrowRight v-if="entryCanSelect(entry)" class="h-3 w-3" />
            </span>
          </button>
          <button
            v-if="entryCanEnd(entry)"
            type="button"
            class="call-activity-dock__end"
            :title="endEntryLabel(entry)"
            :aria-label="endEntryLabel(entry)"
            @click="endEntry(entry)"
          >
            <PhoneOff class="h-3.5 w-3.5" aria-hidden="true" />
          </button>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.call-activity-dock-host {
  display: contents;
}

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

.call-activity-dock__total {
  display: inline-flex;
  min-width: 1.125rem;
  height: 1.125rem;
  align-items: center;
  justify-content: center;
  border-radius: 9999px;
  background: color-mix(in oklab, var(--sidebar-accent) 76%, transparent);
  color: var(--sidebar-foreground);
  font-variant-numeric: tabular-nums;
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

.call-activity-dock__row-shell {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 0.25rem;
  border-radius: var(--radius-sm);
}

.call-activity-dock__row {
  display: grid;
  width: 100%;
  grid-template-columns: 1.75rem minmax(0, 1fr) auto auto;
  min-height: 3rem;
  align-items: center;
  gap: 0.5rem;
  border-radius: var(--radius-sm);
  padding: 0.375rem 0.5rem;
  color: var(--sidebar-muted);
  text-align: left;
  transition:
    background-color 160ms ease-out,
    color 160ms ease-out,
    transform 160ms ease-out;
}

.call-activity-dock__row:hover:not(:disabled),
.call-activity-dock__row-shell--active .call-activity-dock__row {
  background: color-mix(in oklab, var(--sidebar-accent) 72%, transparent);
  color: var(--sidebar-foreground);
}

.call-activity-dock__row:hover:not(:disabled) {
  transform: translateY(-1px);
}

.call-activity-dock__row-shell--disabled .call-activity-dock__row,
.call-activity-dock__row-shell--disabled .call-activity-dock__row:hover {
  cursor: default;
  opacity: 0.72;
  transform: none;
}

.call-activity-dock__row-shell--disabled .call-activity-dock__action {
  background: color-mix(in oklab, var(--sidebar-accent) 58%, transparent);
  color: var(--sidebar-muted);
}

.call-activity-dock__row:focus-visible {
  outline: none;
  box-shadow: 0 0 0 2px color-mix(in oklab, var(--primary) 42%, transparent);
}

.call-activity-dock__icon {
  display: inline-flex;
  width: 1.75rem;
  height: 1.75rem;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-sm);
  background: color-mix(in oklab, oklch(0.7 0.18 145) 12%, transparent);
  color: var(--success-foreground);
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
.call-activity-dock__status,
.call-activity-dock__participants,
.call-activity-dock__participant-copy {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.call-activity-dock__status {
  color: var(--sidebar-muted);
}

.call-activity-dock__participants {
  display: flex;
  align-items: center;
  gap: 0.375rem;
  color: var(--sidebar-muted);
}

.call-activity-dock__participant-stack {
  display: inline-flex;
  flex-shrink: 0;
  padding-left: 0.125rem;
}

.call-activity-dock__participant {
  display: inline-flex;
  width: 1rem;
  height: 1rem;
  align-items: center;
  justify-content: center;
  border: 1px solid color-mix(in oklab, var(--sidebar-border) 74%, transparent);
  border-radius: 9999px;
  background: color-mix(in oklab, var(--sidebar-accent) 82%, var(--card));
  color: var(--sidebar-foreground);
  font-size: 0.5625rem;
  font-weight: 700;
  line-height: 1;
}

.call-activity-dock__participant + .call-activity-dock__participant {
  margin-left: -0.25rem;
}

.call-activity-dock__count {
  display: inline-flex;
  min-width: 1.125rem;
  height: 1.125rem;
  align-items: center;
  justify-content: center;
  border-radius: 9999px;
  background: color-mix(in oklab, oklch(0.7 0.18 145) 18%, transparent);
  color: var(--success-foreground);
  font-variant-numeric: tabular-nums;
}

.call-activity-dock__action {
  display: inline-flex;
  min-width: 3.25rem;
  height: 1.625rem;
  align-items: center;
  justify-content: center;
  gap: 0.25rem;
  border-radius: 9999px;
  background: color-mix(in oklab, oklch(0.7 0.18 145) 16%, transparent);
  color: var(--success-foreground);
  white-space: nowrap;
}

.call-activity-dock__row:hover:not(:disabled) .call-activity-dock__action,
.call-activity-dock__row-shell--active .call-activity-dock__action {
  background: color-mix(in oklab, oklch(0.7 0.18 145) 26%, transparent);
  color: var(--success-foreground);
}

:global(.dark) .call-activity-dock__action,
:global(.dark) .call-activity-dock__row:hover:not(:disabled) .call-activity-dock__action,
:global(.dark) .call-activity-dock__row-shell--active .call-activity-dock__action {
  color: oklch(0.86 0.13 145);
}

.call-activity-dock__end {
  display: inline-flex;
  width: 2.25rem;
  min-height: 3rem;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-sm);
  color: var(--destructive);
  transition:
    background-color 160ms ease-out,
    color 160ms ease-out;
}

.call-activity-dock__end:hover {
  background: color-mix(in oklab, var(--destructive) 10%, transparent);
}

.call-activity-dock__end:focus-visible {
  outline: none;
  box-shadow: 0 0 0 2px color-mix(in oklab, var(--destructive) 32%, transparent);
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
  min-width: min(17rem, 84vw);
}

.call-activity-dock.call-activity-dock--mobile .call-activity-dock__row-shell {
  min-width: min(20rem, 92vw);
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
