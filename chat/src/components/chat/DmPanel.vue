<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { ArrowRight, MessageCircle, Phone, PhoneCall, PhoneIncoming, PhoneOutgoing, Plus, Video } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { formatTimelineStamp } from "@/channels/timeline";
import { $callState } from "@/lib/calls/call-store";
import { dmCallActivityAction } from "@/lib/calls/call-activity-dock";
import { $dmCallActivities, hasKnownDmCallMedia } from "@/lib/calls/dm-call-activity";
import type { DmCallActivity } from "@/lib/calls/dm-call-activity";
import type { CallMedia } from "@/lib/calls/types";
import { barePeerJid } from "@/lib/xmpp/jid";
import type { DmConversation } from "@/lib/xmpp-client";

const props = defineProps<{
  conversations: DmConversation[];
  activePeerJid: string | null;
}>();

const emit = defineEmits<{
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  selectDm: [peerJid: string];
  reconnectDm: [peerJid: string, media: CallMedia];
  newDm: [];
}>();

const dmCallActivities = useStore($dmCallActivities);
const callState = useStore($callState);
const activePeer = computed(() => normalizedPeerJid(props.activePeerJid ?? ""));
const activeCallRows = computed(() =>
  Object.values(dmCallActivities.value)
    .map((activity) => ({
      activity,
      peerJid: normalizedPeerJid(activity.peerJid),
      title: callActivityTitle(activity),
      meta: callActivityMeta(activity),
      action: callActivityActionLabel(activity),
    }))
    .filter((row) => row.peerJid)
    .sort((left, right) => {
      const rightMs = timestampMs(right.activity.updatedAt);
      const leftMs = timestampMs(left.activity.updatedAt);
      if (rightMs !== leftMs) return rightMs - leftMs;
      return left.peerJid.localeCompare(right.peerJid);
    }),
);
const visibleConversations = computed<DmConversation[]>(() => {
  const activeCallPeers = new Set(activeCallRows.value.map((row) => row.peerJid));
  return props.conversations
    .filter((conversation) => !activeCallPeers.has(normalizedPeerJid(conversation.peerJid)))
    .sort(compareDmConversations);
});

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
  const normalized = normalizedPeerJid(peerJid);
  return !!normalized && !!dmCallActivities.value[normalized];
}

function callActivity(peerJid: string): DmCallActivity | null {
  const normalized = normalizedPeerJid(peerJid);
  return normalized ? dmCallActivities.value[normalized] ?? null : null;
}

function callActivityLabel(peerJid: string): string {
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

function callActivityMeta(activity: DmCallActivity): string {
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

function callActivityActionLabel(activity: DmCallActivity): string {
  switch (dmCallActivityAction(activity, callState.value)) {
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

function selectCallActivity(activity: DmCallActivity): void {
  const peerJid = normalizedPeerJid(activity.peerJid);
  if (!peerJid) return;
  switch (dmCallActivityAction(activity, callState.value)) {
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
    parts.push(callActivityMeta(activity), `${callActivityActionLabel(activity)} call`);
  } else if (conversation.lastMessageBody) {
    parts.push(preview(conversation.lastMessageBody));
  }
  if (conversation.unreadCount > 0) {
    parts.push(`${conversation.unreadCount} unread`);
  }
  return parts.join(", ");
}
</script>

<template>
  <div class="chat-sidebar-pane glass-panel">
    <div class="chat-sidebar-header">
      <div class="flex items-center gap-2">
        <MessageCircle class="w-4 h-4 text-primary/70" />
        <h2 class="type-pane-title text-sidebar-foreground">Direct messages</h2>
      </div>
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

    <div class="chat-pane-scroll chat-sidebar-scroll">
      <!-- Empty state — halo + MessageCircle glyph + caption + hint
           matches the iter-37 / 47 / 48 authored-empty-state pattern
           used elsewhere in the app. -->
      <div v-if="visibleConversations.length === 0 && activeCallRows.length === 0" class="flex flex-col items-center justify-center gap-2 py-10 text-center">
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
          v-if="activeCallRows.length > 0"
          class="grid gap-1"
          aria-label="Active direct calls"
        >
          <div class="type-section-label flex items-center gap-1.5 px-2 pt-2 pb-1 text-sidebar-muted">
            <PhoneCall class="h-3 w-3 text-success" aria-hidden="true" />
            <span class="flex-1 truncate">Active calls</span>
            <span
              class="type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-success/15 text-success ring-1 ring-success/30"
              aria-hidden="true"
            >
              {{ activeCallRows.length }}
            </span>
          </div>
          <button
            v-for="row in activeCallRows"
            :key="`dm-call:${row.peerJid}:${row.activity.sid}`"
            class="chat-list-row w-full min-h-11 flex items-center gap-2.5 px-3 py-2 text-left group text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground"
            :class="isActiveConversation(row.peerJid) ? 'chat-list-row--active bg-sidebar-accent text-sidebar-foreground' : ''"
            type="button"
            :aria-current="isActiveConversation(row.peerJid) ? 'page' : undefined"
            :aria-label="`${row.action} ${row.title} call, ${row.meta}`"
            @click="selectCallActivity(row.activity)"
          >
            <span class="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-md bg-success/10 text-success">
              <Video v-if="hasKnownDmCallMedia(row.activity) && row.activity.media.video" class="h-3.5 w-3.5" aria-hidden="true" />
              <PhoneIncoming v-else-if="row.activity.state === 'ringing' && row.activity.direction === 'incoming'" class="h-3.5 w-3.5" aria-hidden="true" />
              <PhoneOutgoing v-else-if="row.activity.state === 'ringing' && row.activity.direction === 'outgoing'" class="h-3.5 w-3.5" aria-hidden="true" />
              <Phone v-else class="h-3.5 w-3.5" aria-hidden="true" />
            </span>
            <span class="min-w-0 flex-1">
              <span class="type-control type-strong block truncate text-sidebar-foreground">
                {{ row.title }}
              </span>
              <span class="type-meta block truncate text-sidebar-muted">
                {{ row.meta }}
              </span>
            </span>
            <span
              class="type-meta hidden shrink-0 rounded-md border border-success/25 bg-success/10 px-1.5 py-0.5 text-success xl:inline-flex"
              aria-hidden="true"
            >
              {{ row.action }}
            </span>
            <ArrowRight class="h-3.5 w-3.5 shrink-0 text-success" aria-hidden="true" />
          </button>
        </section>

        <button
          v-for="conversation in visibleConversations"
          :key="conversation.peerJid"
          class="chat-list-row w-full min-h-14 flex items-center gap-3 px-3 py-2 text-left group"
          :class="isActiveConversation(conversation.peerJid)
            ? 'chat-list-row--active bg-sidebar-accent text-sidebar-foreground'
            : 'text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'"
          :aria-current="isActiveConversation(conversation.peerJid) ? 'page' : undefined"
          :aria-label="conversationRowLabel(conversation)"
          type="button"
          @click="emit('selectDm', conversation.peerJid)"
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
                class="type-meta inline-flex h-[18px] shrink-0 items-center gap-1 rounded-full border border-success/25 bg-success/10 px-1.5 text-success"
                :title="callActivityLabel(conversation.peerJid)"
                :aria-label="callActivityLabel(conversation.peerJid)"
              >
                <PhoneCall class="h-3 w-3" />
                <span>{{ callActivityLabel(conversation.peerJid) }}</span>
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
      </div>
    </div>
  </div>
</template>
