<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { ArrowRight, MessageCircle, MessagesSquare, Phone, PhoneCall, PhoneIncoming, PhoneOff, PhoneOutgoing, Plus, UserPlus, Video } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { formatTimelineStamp } from "@/channels/timeline";
import type { MessageThreadEntry } from "@/channels/threads";
import { $callState } from "@/lib/calls/call-store";
import {
  canEndRecoveredDmCallActivity,
  dmCallActivityAction,
  dmCallActivitySortPriority,
} from "@/lib/calls/call-activity-dock";
import {
  $dmCallActivities,
  dmCallActivitiesForPeer,
  dmCallResumeBlockReason,
  hasKnownDmCallMedia,
} from "@/lib/calls/dm-call-activity";
import type { DmCallActivity } from "@/lib/calls/dm-call-activity";
import type { CallMedia } from "@/lib/calls/types";
import { barePeerJid } from "@/lib/xmpp/jid";
import type { GroupDmSummary } from "@/lib/chat-types";
import type { DmConversation } from "@/lib/xmpp-client";

const props = withDefaults(defineProps<{
  conversations: DmConversation[];
  groupDms?: GroupDmSummary[];
  activePeerJid: string | null;
  activeGroupDmRoomJid?: string | null;
  threadEntries?: MessageThreadEntry[];
  selfFullJid?: string | null;
  hideCurrentCall?: boolean;
}>(), {
  groupDms: () => [],
  activeGroupDmRoomJid: null,
  threadEntries: () => [],
  hideCurrentCall: false,
});

const emit = defineEmits<{
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  selectDm: [peerJid: string];
  selectGroupDm: [roomJid: string];
  selectThread: [threadId: string];
  reconnectDm: [peerJid: string, media: CallMedia];
  endDm: [peerJid: string, sid?: string];
  newDm: [];
  newGroupDm: [];
  addPeopleToDm: [peerJid: string];
}>();

const dmCallActivities = useStore($dmCallActivities);
const callState = useStore($callState);
const activePeer = computed(() => normalizedPeerJid(props.activePeerJid ?? ""));
const currentActiveDmPeer = computed(() => {
  const current = callState.value;
  if (!props.hideCurrentCall || current.phase !== "active" || current.kind !== "dm") return "";
  return normalizedPeerJid(current.peer);
});
const hiddenCurrentCallPeer = computed(() => {
  const peer = currentActiveDmPeer.value;
  return peer || "";
});
const activeCallRows = computed(() =>
  Object.values(dmCallActivities.value)
    .map((activity) => ({
      activity,
      peerJid: normalizedPeerJid(activity.peerJid),
      title: callActivityTitle(activity),
      meta: callActivityMeta(activity),
      description: callActivityDescription(activity),
      action: callActivityActionLabel(activity),
      eyebrow: callActivityEyebrow(activity),
      since: callActivitySince(activity),
      avatarUrl: callActivityAvatarUrl(activity),
    }))
    .filter((row) => row.peerJid && !isCurrentDmActivity(row.activity))
    .sort(compareActiveCallRows),
);
const activeCallStatusMessage = computed(() => {
  const count = activeCallRows.value.length;
  const other = hiddenCurrentCallPeer.value ? "other " : "";
  if (count === 0) return `No ${other}active direct calls.`;
  return `${count} ${other}active direct ${count === 1 ? "call" : "calls"}.`;
});
const visibleConversations = computed<DmConversation[]>(() => {
  const activeCallPeers = new Set(activeCallRows.value.map((row) => row.peerJid));
  return props.conversations
    .filter((conversation) => !activeCallPeers.has(normalizedPeerJid(conversation.peerJid)))
    .sort(compareDmConversations);
});
const visibleGroupDms = computed(() =>
  [...props.groupDms].sort((a, b) => (a.position ?? 0) - (b.position ?? 0) || a.name.localeCompare(b.name)),
);
const visibleThreadEntries = computed(() => props.threadEntries.filter((entry) => entry.count > 0));

function normalizedPeerJid(peerJid: string): string {
  return barePeerJid(peerJid).toLowerCase();
}

function timestampMs(timestamp?: string): number {
  const ms = Date.parse(timestamp ?? "");
  return Number.isFinite(ms) ? ms : Number.NEGATIVE_INFINITY;
}

function conversationSortMs(conversation: DmConversation): number {
  const callUpdatedAt = callActivity(conversation.peerJid)?.updatedAt;
  return Math.max(timestampMs(conversation.lastMessageAt), timestampMs(callUpdatedAt));
}

function compareDmConversations(left: DmConversation, right: DmConversation): number {
  const leftMs = conversationSortMs(left);
  const rightMs = conversationSortMs(right);
  if (leftMs !== rightMs) return rightMs - leftMs;
  return normalizedPeerJid(left.peerJid).localeCompare(normalizedPeerJid(right.peerJid));
}

function compareActiveCallRows(
  left: { activity: DmCallActivity; peerJid: string },
  right: { activity: DmCallActivity; peerJid: string },
): number {
  const leftPriority = dmCallActivitySortPriority(left.activity, callState.value, props.selfFullJid ?? null);
  const rightPriority = dmCallActivitySortPriority(right.activity, callState.value, props.selfFullJid ?? null);
  if (leftPriority !== rightPriority) return leftPriority - rightPriority;
  const rightMs = timestampMs(right.activity.updatedAt);
  const leftMs = timestampMs(left.activity.updatedAt);
  if (rightMs !== leftMs) return rightMs - leftMs;
  return left.peerJid.localeCompare(right.peerJid);
}

function preview(text?: string): string {
  if (!text) return "";
  return text.length > 44 ? `${text.slice(0, 44)}…` : text;
}

function dotClass(show?: DmConversation["presenceShow"]): string {
  if (show === "away") return "bg-warning/75";
  if (show === "dnd") return "bg-destructive/75";
  if (show === "xa") return "bg-warning/55";
  if (show === "available") return "bg-success/75";
  return "bg-muted-foreground/25";
}

function hasCallActivity(peerJid: string): boolean {
  return dmCallActivitiesForPeer(dmCallActivities.value, peerJid, props.selfFullJid ?? null).length > 0;
}

function isHiddenCurrentCallPeer(peerJid: string): boolean {
  const normalized = normalizedPeerJid(peerJid);
  return !!normalized && hiddenCurrentCallPeer.value === normalized;
}

function isCurrentDmActivity(activity: DmCallActivity): boolean {
  const current = callState.value;
  if (current.phase !== "active" || current.kind !== "dm") return false;
  return normalizedPeerJid(activity.peerJid) === normalizedPeerJid(current.peer) &&
    activity.sid === current.sid;
}

function callActivity(peerJid: string): DmCallActivity | null {
  return dmCallActivitiesForPeer(dmCallActivities.value, peerJid, props.selfFullJid ?? null)[0] ?? null;
}

function callActivityLabel(peerJid: string): string {
  if (isHiddenCurrentCallPeer(peerJid)) return "Current call shown separately";
  const activity = callActivity(peerJid);
  return activity?.state === "ringing" ? "Ringing" : "Live";
}

function callActivityTitle(activity: DmCallActivity): string {
  const peerJid = normalizedPeerJid(activity.peerJid);
  const existing = props.conversations.find((conversation) =>
    normalizedPeerJid(conversation.peerJid) === peerJid
  );
  return existing?.peerUsername || peerJid.split("@")[0] || peerJid;
}

function callActivityAvatarUrl(activity: DmCallActivity): string | null {
  const peerJid = normalizedPeerJid(activity.peerJid);
  const existing = props.conversations.find((conversation) =>
    normalizedPeerJid(conversation.peerJid) === peerJid
  );
  return existing?.peerAvatarUrl ?? null;
}

type CallActivityVisualTone = "primary" | "success" | "warning";

function acceptedCallAvailability(activity: DmCallActivity): {
  eyebrow: string;
  meta: string;
  description: string;
  tone: CallActivityVisualTone;
} | null {
  if (activity.state !== "accepted") return null;

  const mediaLabel = hasKnownDmCallMedia(activity)
    ? `${activity.media.video ? "video" : "voice"} call`
    : "call";
  const mediaTitle = `${mediaLabel.charAt(0).toUpperCase()}${mediaLabel.slice(1)}`;
  const reason = dmCallResumeBlockReason(activity, props.selfFullJid ?? null);
  if (reason === null) {
    return {
      eyebrow: "Live now",
      meta: `Live ${mediaLabel}`,
      description: `The ${mediaLabel} is still live.`,
      tone: "success",
    };
  }
  if (reason === "other-resource") {
    return {
      eyebrow: "Other device",
      meta: `${mediaTitle} · Other device`,
      description: "This call is live on another browser or device.",
      tone: "warning",
    };
  }
  if (reason === "expired-token" || reason === "invalid-token") {
    if (canEndRecoveredDmCallActivity(activity, callState.value, props.selfFullJid ?? null)) {
      return {
        eyebrow: "Recovered after refresh",
        meta: `${mediaTitle} · End available`,
        description: `The saved reconnect details expired, but this tab can still end the call.`,
        tone: "warning",
      };
    }
    return {
      eyebrow: "Expired",
      meta: `${mediaTitle} · Expired`,
      description: "The saved reconnect details expired.",
      tone: "warning",
    };
  }
  return {
    eyebrow: "Syncing",
    meta: `${mediaTitle} · Details pending`,
    description: "Reconnect details are not available on this tab yet.",
    tone: "primary",
  };
}

function callActivityVisualTone(activity: DmCallActivity): CallActivityVisualTone {
  if (activity.state === "ringing" && activity.direction === "incoming") return "warning";
  if (activity.state === "ringing" && activity.direction === "outgoing") return "primary";
  return acceptedCallAvailability(activity)?.tone ?? "success";
}

function callActivityEyebrow(activity: DmCallActivity): string {
  const availability = acceptedCallAvailability(activity);
  if (availability) return availability.eyebrow;
  if (activity.state === "accepted") return "Live now";
  if (activity.direction === "incoming") return "Incoming call";
  if (activity.direction === "outgoing") return "Calling";
  return "Ringing";
}

function callActivityToneClass(activity: DmCallActivity): string {
  switch (callActivityVisualTone(activity)) {
    case "warning":
      return "border-warning/25 bg-warning/10 hover:bg-warning/15";
    case "primary":
      return "border-primary/25 bg-primary/8 hover:bg-primary/12";
    case "success":
      return "border-success/20 bg-success/10 hover:bg-success/15";
  }
}

function callActivityActiveToneClass(activity: DmCallActivity): string {
  switch (callActivityVisualTone(activity)) {
    case "warning":
      return "bg-warning/15 text-sidebar-foreground ring-1 ring-warning/30";
    case "primary":
      return "bg-primary/10 text-sidebar-foreground ring-1 ring-primary/30";
    case "success":
      return "bg-success/15 text-sidebar-foreground ring-1 ring-success/30";
  }
}

function callActivityIconClass(activity: DmCallActivity): string {
  switch (callActivityVisualTone(activity)) {
    case "warning":
      return "border-warning/25 bg-background/90 text-warning-foreground";
    case "primary":
      return "border-primary/25 bg-background/90 text-primary";
    case "success":
      return "border-success/25 bg-background/90 text-success-foreground";
  }
}

function callActivityAccentClass(activity: DmCallActivity): string {
  switch (callActivityVisualTone(activity)) {
    case "warning":
      return "text-warning-foreground";
    case "primary":
      return "text-primary";
    case "success":
      return "text-success-foreground";
  }
}

function callActivityDotClass(activity: DmCallActivity): string {
  switch (callActivityVisualTone(activity)) {
    case "warning":
      return "bg-warning shadow-[0_0_6px_var(--warning)]";
    case "primary":
      return "bg-primary shadow-[0_0_6px_var(--primary)]";
    case "success":
      return "bg-success shadow-[0_0_6px_var(--success)]";
  }
}

function callActivityPillClass(activity: DmCallActivity): string {
  switch (callActivityVisualTone(activity)) {
    case "warning":
      return "border-warning/25 bg-background/70 text-warning-foreground";
    case "primary":
      return "border-primary/25 bg-background/70 text-primary";
    case "success":
      return "border-success/25 bg-background/70 text-success-foreground";
  }
}

function callActivitySince(activity: DmCallActivity): string {
  const stamp = formatTimelineStamp(activity.updatedAt);
  return stamp ? `Updated ${stamp}` : "";
}

function callActivityMeta(activity: DmCallActivity): string {
  const availability = acceptedCallAvailability(activity);
  if (availability) return availability.meta;
  if (!hasKnownDmCallMedia(activity)) {
    if (activity.state === "accepted") return "Live call";
    if (activity.direction === "incoming") return "Incoming call";
    if (activity.direction === "outgoing") return "Calling";
    return "Call ringing";
  }
  const media = activity.media.video ? "video" : "voice";
  if (activity.state === "accepted") return `Live ${media} call`;
  if (activity.direction === "incoming") return `Incoming ${media} call`;
  if (activity.direction === "outgoing") return `Calling ${media} call`;
  return `${media[0]?.toUpperCase() ?? "V"}${media.slice(1)} call ringing`;
}

function callActivityDescription(activity: DmCallActivity): string {
  const availability = acceptedCallAvailability(activity);
  if (availability) return availability.description;
  if (activity.direction === "incoming") return "Waiting for an answer.";
  if (activity.direction === "outgoing") return "Still ringing.";
  return "Call is ringing.";
}

function callActivityActionLabel(activity: DmCallActivity): string {
  switch (dmCallActivityAction(activity, callState.value, props.selfFullJid ?? null)) {
    case "answer":
      return "Answer";
    case "reconnect":
      return "Reconnect";
    case "return":
      return "Return";
    case "open":
      return "Open";
  }
}

function activeCallRowLabel(row: {
  action: string;
  title: string;
  meta: string;
  description: string;
  eyebrow: string;
  since: string;
}): string {
  return [
    activeCallRowActionPhrase(row),
    row.eyebrow,
    row.meta,
    row.description,
    row.since,
  ].filter(Boolean).join(", ");
}

function activeCallRowActionPhrase(row: { action: string; title: string }): string {
  if (row.action === "Open") return `Open ${row.title} conversation`;
  return `${row.action} ${row.title} call`;
}

function selectCallActivity(activity: DmCallActivity): void {
  const peerJid = normalizedPeerJid(activity.peerJid);
  if (!peerJid) return;
  switch (dmCallActivityAction(activity, callState.value, props.selfFullJid ?? null)) {
    case "answer":
      if (activity.remoteFullJid) {
        emit("answerDm", peerJid, activity.remoteFullJid, activity.sid, activity.media);
      }
      return;
    case "reconnect":
      emit("reconnectDm", peerJid, activity.media);
      return;
    case "open":
    case "return":
      emit("selectDm", peerJid);
      return;
  }
}

function canEndCallActivity(activity: DmCallActivity): boolean {
  return canEndRecoveredDmCallActivity(activity, callState.value, props.selfFullJid ?? null);
}

function endCallActivity(activity: DmCallActivity): void {
  const peerJid = normalizedPeerJid(activity.peerJid);
  if (!peerJid || !canEndCallActivity(activity)) return;
  emit("endDm", peerJid, activity.sid);
}

function endCallActivityLabel(row: { title: string; activity: DmCallActivity }): string {
  const media = hasKnownDmCallMedia(row.activity)
    ? `${row.activity.media.video ? "video" : "voice"} call`
    : "call";
  return `End ${row.title} ${media}`;
}

function isActiveConversation(peerJid: string): boolean {
  const peer = normalizedPeerJid(peerJid);
  return !!peer && activePeer.value === peer;
}

function conversationRowLabel(conversation: DmConversation): string {
  const parts = [
    conversation.peerUsername,
    isActiveConversation(conversation.peerJid) ? "selected" : "not selected",
  ];
  const activity = callActivity(conversation.peerJid);
  if (activity) {
    if (isHiddenCurrentCallPeer(conversation.peerJid)) {
      parts.push("Current call shown separately", "Open conversation");
    } else {
      parts.push(callActivityMeta(activity), `${callActivityActionLabel(activity)} call`);
    }
  } else if (conversation.lastMessageBody) {
    parts.push(preview(conversation.lastMessageBody));
  }
  if (conversation.unreadCount > 0) {
    parts.push(`${conversation.unreadCount} unread`);
  }
  return parts.join(", ");
}

function addPeopleLabel(conversation: DmConversation): string {
  return `Add people to ${conversation.peerUsername || normalizedPeerJid(conversation.peerJid)}`;
}

function threadEntryTitle(entry: MessageThreadEntry): string {
  const body = entry.root?.body?.trim();
  return body || entry.threadId;
}

function threadEntryLabel(entry: MessageThreadEntry): string {
  const parts = [
    `Open thread ${threadEntryTitle(entry)}`,
    `${entry.count} ${entry.count === 1 ? "reply" : "replies"}`,
  ];
  if (entry.lastTs) parts.push(formatTimelineStamp(entry.lastTs));
  return parts.filter(Boolean).join(", ");
}
</script>

<template>
  <div class="chat-sidebar-pane glass-panel">
    <div class="chat-sidebar-header">
      <div class="flex items-center gap-2">
        <MessageCircle class="w-4 h-4 text-primary/70" />
        <h2 class="type-pane-title text-sidebar-foreground">Direct messages</h2>
      </div>
      <div class="flex items-center gap-1">
        <button
          class="chat-icon-button text-sidebar-muted hover:bg-sidebar-accent hover:text-sidebar-foreground"
          title="New group message"
          aria-label="New group message"
          type="button"
          @click="emit('newGroupDm')"
        >
          <MessagesSquare class="w-4 h-4" />
        </button>
        <button
          class="chat-icon-button text-sidebar-muted hover:bg-sidebar-accent hover:text-sidebar-foreground"
          title="New message"
          aria-label="New direct message"
          type="button"
          @click="emit('newDm')"
        >
          <Plus class="w-4 h-4" />
        </button>
      </div>
    </div>

    <div class="chat-pane-scroll chat-sidebar-scroll">
      <p class="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {{ activeCallStatusMessage }}
      </p>
      <!-- Empty state — halo + MessageCircle glyph + caption + hint
           matches the iter-37 / 47 / 48 authored-empty-state pattern
           used elsewhere in the app. -->
      <div v-if="visibleGroupDms.length === 0 && visibleConversations.length === 0 && activeCallRows.length === 0" class="flex flex-col items-center justify-center gap-2 py-10 text-center">
        <div class="chat-empty-state__halo">
          <span class="chat-empty-state__halo-glow chat-empty-state__halo-glow--primary" aria-hidden="true" />
          <span class="chat-empty-state__halo-ring chat-empty-state__halo-ring--primary">
            <MessageCircle class="w-4 h-4 text-primary" aria-hidden="true" />
          </span>
        </div>
        <div class="type-control text-sidebar-foreground">No conversations yet</div>
        <div class="type-caption text-sidebar-muted max-w-[14rem]">
          Start one with the + button above.
        </div>
      </div>

      <div v-else class="chat-list-stack">
        <section
          v-if="visibleThreadEntries.length > 0"
          class="grid gap-1"
          aria-label="Direct message threads"
        >
          <div class="type-section-label flex items-center gap-1.5 px-2 pt-2 pb-1 text-sidebar-muted">
            <MessagesSquare class="h-3 w-3 text-primary/70" aria-hidden="true" />
            <span class="flex-1 truncate">Threads</span>
            <span
              class="type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-primary/15 text-primary ring-1 ring-primary/25"
              aria-hidden="true"
            >
              {{ visibleThreadEntries.length }}
            </span>
          </div>
          <button
            v-for="entry in visibleThreadEntries"
            :key="entry.threadId"
            class="chat-list-row w-full min-h-12 flex items-center gap-3 px-3 py-2 text-left group text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground"
            type="button"
            :aria-label="threadEntryLabel(entry)"
            @click="emit('selectThread', entry.threadId)"
          >
            <MessagesSquare class="h-4 w-4 shrink-0 text-primary/70" aria-hidden="true" />
            <span class="min-w-0 flex-1">
              <span class="type-control block truncate text-sidebar-foreground">{{ threadEntryTitle(entry) }}</span>
              <span class="type-caption block truncate text-sidebar-muted">
                {{ entry.count }} {{ entry.count === 1 ? 'reply' : 'replies' }}<template v-if="entry.lastTs"> · {{ formatTimelineStamp(entry.lastTs) }}</template>
              </span>
            </span>
          </button>
        </section>

        <section
          v-if="activeCallRows.length > 0"
          class="grid gap-1"
          aria-label="Active direct calls"
        >
          <div class="type-section-label flex items-center gap-1.5 px-2 pt-2 pb-1 text-sidebar-muted">
            <PhoneCall class="h-3 w-3 text-success-foreground" aria-hidden="true" />
            <span class="flex-1 truncate">Active calls</span>
            <span
              class="type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-success/15 text-success-foreground ring-1 ring-success/30"
              aria-hidden="true"
            >
              {{ activeCallRows.length }}
            </span>
          </div>
          <div
            v-for="row in activeCallRows"
            :key="`dm-call:${row.peerJid}:${row.activity.sid}`"
            class="chat-list-row w-full min-h-16 flex items-center gap-3 rounded-lg border px-3 py-2.5 text-left group text-sidebar-muted hover:text-sidebar-foreground"
            :class="[
              callActivityToneClass(row.activity),
              isActiveConversation(row.peerJid) ? `chat-list-row--active ${callActivityActiveToneClass(row.activity)}` : '',
            ]"
          >
            <button
              class="flex min-w-0 flex-1 items-center gap-3 text-left"
              type="button"
              :aria-current="isActiveConversation(row.peerJid) ? 'page' : undefined"
              :aria-label="activeCallRowLabel(row)"
              @click="selectCallActivity(row.activity)"
            >
              <span class="relative shrink-0">
                <AppAvatar :name="row.title" :src="row.avatarUrl" size="sm" />
                <span class="absolute -right-1 -bottom-1 flex h-4 w-4 items-center justify-center rounded-full border" :class="callActivityIconClass(row.activity)">
                  <Video v-if="hasKnownDmCallMedia(row.activity) && row.activity.media.video" class="h-2.5 w-2.5" aria-hidden="true" />
                  <PhoneIncoming v-else-if="row.activity.state === 'ringing' && row.activity.direction === 'incoming'" class="h-2.5 w-2.5" aria-hidden="true" />
                  <PhoneOutgoing v-else-if="row.activity.state === 'ringing' && row.activity.direction === 'outgoing'" class="h-2.5 w-2.5" aria-hidden="true" />
                  <Phone v-else class="h-2.5 w-2.5" aria-hidden="true" />
                </span>
              </span>
              <span class="min-w-0 flex-1 space-y-0.5">
                <span class="type-meta flex items-center gap-1" :class="callActivityAccentClass(row.activity)">
                  <span class="h-1.5 w-1.5 rounded-full" :class="callActivityDotClass(row.activity)" aria-hidden="true" />
                  <span class="truncate">{{ row.eyebrow }}</span>
                </span>
                <span class="type-control type-strong block truncate text-sidebar-foreground">
                  {{ row.title }}
                </span>
                <span class="type-meta block truncate text-sidebar-muted">
                  {{ row.meta }}<template v-if="row.description"> · {{ row.description }}</template><template v-if="row.since"> · {{ row.since }}</template>
                </span>
              </span>
              <span
                class="type-meta shrink-0 rounded-full border px-2 py-1"
                :class="callActivityPillClass(row.activity)"
                aria-hidden="true"
              >
                {{ row.action }}
              </span>
              <ArrowRight class="h-3.5 w-3.5 shrink-0" :class="callActivityAccentClass(row.activity)" aria-hidden="true" />
            </button>
            <button
              v-if="canEndCallActivity(row.activity)"
              type="button"
              class="chat-icon-button shrink-0 text-destructive hover:bg-destructive/10 hover:text-destructive"
              :title="endCallActivityLabel(row)"
              :aria-label="endCallActivityLabel(row)"
              @click="endCallActivity(row.activity)"
            >
              <PhoneOff class="h-3.5 w-3.5" aria-hidden="true" />
            </button>
          </div>
        </section>

        <section
          v-if="visibleGroupDms.length > 0"
          class="grid gap-1"
          aria-label="Group direct messages"
        >
          <div class="type-section-label flex items-center gap-1.5 px-2 pt-2 pb-1 text-sidebar-muted">
            <MessagesSquare class="h-3 w-3 text-primary/70" aria-hidden="true" />
            <span class="flex-1 truncate">Groups</span>
          </div>
          <button
            v-for="group in visibleGroupDms"
            :key="group.roomJid"
            class="chat-list-row w-full min-h-14 flex items-center gap-3 px-3 py-2 text-left group"
            :class="normalizedPeerJid(activeGroupDmRoomJid ?? '') === normalizedPeerJid(group.roomJid)
              ? 'chat-list-row--active bg-sidebar-accent text-sidebar-foreground'
              : 'text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'"
            type="button"
            :aria-current="normalizedPeerJid(activeGroupDmRoomJid ?? '') === normalizedPeerJid(group.roomJid) ? 'page' : undefined"
            :aria-label="`Open group message ${group.name}`"
            @click="emit('selectGroupDm', group.roomJid)"
          >
            <span class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-sidebar-accent text-sidebar-foreground">
              <MessagesSquare class="h-4 w-4" aria-hidden="true" />
            </span>
            <span class="min-w-0 flex-1">
              <span class="type-control block truncate text-sidebar-foreground">{{ group.name }}</span>
              <span class="type-caption block truncate text-sidebar-muted">Group message</span>
            </span>
            <span class="flex shrink-0 items-center gap-1">
              <span
                v-if="(group.mentionCount ?? 0) > 0"
                class="chat-list-row--mention-badge type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
                aria-hidden="true"
              >@{{ group.mentionCount }}</span>
              <span
                v-if="(group.unreadCount ?? 0) > 0"
                class="chat-list-row--unread-badge type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-primary text-primary-foreground"
                aria-hidden="true"
              >{{ group.unreadCount }}</span>
            </span>
          </button>
        </section>

        <div
          v-for="conversation in visibleConversations"
          :key="conversation.peerJid"
          class="chat-list-row w-full min-h-14 flex items-center gap-3 px-3 py-2 text-left group"
          :class="isActiveConversation(conversation.peerJid)
            ? 'chat-list-row--active bg-sidebar-accent text-sidebar-foreground'
            : 'text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'"
          @click="emit('selectDm', conversation.peerJid)"
        >
          <button
            class="flex min-w-0 flex-1 items-center gap-3 text-left"
            type="button"
            :aria-current="isActiveConversation(conversation.peerJid) ? 'page' : undefined"
            :aria-label="conversationRowLabel(conversation)"
            @click.stop="emit('selectDm', conversation.peerJid)"
          >
            <div class="relative">
              <AppAvatar :name="conversation.peerUsername" :src="conversation.peerAvatarUrl ?? null" size="sm" />
              <span class="absolute -right-0.5 -bottom-0.5 w-2 h-2 rounded-full border border-background" :class="dotClass(conversation.presenceShow)" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-center justify-between gap-2">
                <span class="type-control truncate">{{ conversation.peerUsername }}</span>
                <span v-if="conversation.lastMessageAt" class="type-meta type-numeric text-sidebar-muted">
                  {{ formatTimelineStamp(conversation.lastMessageAt) }}
                </span>
              </div>
              <div class="flex items-center gap-1.5">
                <span class="type-caption text-sidebar-muted min-w-0 flex-1 truncate">{{ preview(conversation.lastMessageBody) }}</span>
                <span
                  v-if="hasCallActivity(conversation.peerJid)"
                  class="type-meta inline-flex h-[18px] shrink-0 items-center gap-1 rounded-full border px-1.5"
                  :class="isHiddenCurrentCallPeer(conversation.peerJid)
                    ? 'border-border bg-muted/60 text-sidebar-muted'
                    : 'border-success/25 bg-success/10 text-success-foreground'"
                  :title="callActivityLabel(conversation.peerJid)"
                  :aria-label="callActivityLabel(conversation.peerJid)"
                >
                  <PhoneCall class="h-3 w-3" />
                  <span>{{ isHiddenCurrentCallPeer(conversation.peerJid) ? 'Current call' : callActivityLabel(conversation.peerJid) }}</span>
                </span>
                <span
                  v-if="conversation.unreadCount > 0"
                  class="chat-badge-glow--primary type-count-badge inline-flex min-w-[18px] h-[18px] shrink-0 items-center justify-center rounded-full bg-primary px-1 text-primary-foreground"
                >
                  {{ conversation.unreadCount }}
                </span>
              </div>
            </div>
          </button>
          <button
            class="chat-icon-button shrink-0 text-sidebar-muted hover:bg-sidebar-accent hover:text-sidebar-foreground"
            type="button"
            :title="addPeopleLabel(conversation)"
            :aria-label="addPeopleLabel(conversation)"
            @click.stop="emit('addPeopleToDm', conversation.peerJid)"
          >
            <UserPlus class="h-3.5 w-3.5" aria-hidden="true" />
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
