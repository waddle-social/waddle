<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { ArrowRight, MessageCircle, MessagesSquare, Phone, PhoneCall, PhoneIncoming, PhoneOff, PhoneOutgoing, Plus, UserPlus, Video } from "lucide-vue-next";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { css, cx } from "styled-system/css";
import { avatar, badge, iconbtn, room } from "styled-system/recipes";
import { conversationPeerAvatarJid } from "@/lib/avatars/author-jid";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import { formatTimelineStamp } from "@/channels/timeline";
import type { MessageThreadEntry } from "@/channels/threads";
import { $callState } from "@/lib/calls/call-store";
import {
  $mucCallMedia,
  $mucCallParticipants,
  mucCallMediaForRoom,
  normalizeMucCallRoomJid,
} from "@/lib/calls/muc-call-presence";
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
import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";
import { useInCallOverlays } from "@/presence/in-call-overlay-store";
import type { GroupDmSummary } from "@/lib/chat-types";
import type { DmConversation, OccupantPresence } from "@/lib/xmpp-client";

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
  addPeopleToDm: [peerJid: string];
}>();

const dmCallActivities = useStore($dmCallActivities);
const mucCallParticipants = useStore($mucCallParticipants);
// Whether a DM peer is in a call (their XEP-0108 overlay, ADR-010 Phase 3),
// for the badge layered on top of their presence dot.
const { peerInCall } = useInCallOverlays();
const mucCallMedia = useStore($mucCallMedia);
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

function presenceOf(show?: DmConversation["presenceShow"]): OccupantPresence {
  if (show === "available") return "online";
  if (show === "away" || show === "xa") return "away";
  return show === "dnd" ? "dnd" : "offline";
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
  return existing?.peerUsername || jidLocalpart(peerJid) || peerJid;
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

function groupCallParticipantCount(roomJid: string): number {
  const normalized = normalizeMucCallRoomJid(roomJid);
  return normalized ? mucCallParticipants.value[normalized]?.length ?? 0 : 0;
}

function groupCallMediaLabel(roomJid: string): string {
  return mucCallMediaForRoom(roomJid, mucCallMedia.value).video ? "Video call" : "Voice call";
}

function hasGroupCallActivity(roomJid: string): boolean {
  return groupCallParticipantCount(roomJid) > 0;
}

function groupCallActivityLabel(roomJid: string): string {
  const count = groupCallParticipantCount(roomJid);
  if (count <= 0) return "";
  const noun = count === 1 ? "person" : "people";
  return `${groupCallMediaLabel(roomJid)} live, ${count} ${noun}`;
}

function groupDmRowLabel(group: GroupDmSummary): string {
  const parts = [`Open group message ${group.name}`];
  const call = groupCallActivityLabel(group.roomJid);
  if (call) parts.push(call);
  return parts.join(", ");
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

// The conversation list (Rooms page): sidebar-style rows on the rail.
// Call rows take their colour from `data-tone`: live (aurora), action or warning.
const cls = {
  root: css({ display: "flex", flexDirection: "column", minWidth: 0, minHeight: 0, flex: 1, bg: "bg.rail" }),
  head: css({ display: "flex", alignItems: "center", gap: "2", flex: "none", minHeight: "header", px: "4", borderBottom: "1px solid {colors.border.soft}" }),
  headIcon: css({ width: "18px", height: "18px", color: "fg.soft", flex: "none" }),
  title: css({ margin: 0, flex: 1, minWidth: 0, font: "700 16px/1.3 {fonts.display}", color: "fg" }),
  scroll: css({ flex: 1, minHeight: 0, overflowX: "hidden", overflowY: "auto", p: "2" }),
  srOnly: css({ srOnly: true }),
  empty: css({ display: "flex", flexDirection: "column", alignItems: "center", gap: "2", py: "10", px: "4", textAlign: "center" }),
  emptyIcon: css({ display: "inline-flex", alignItems: "center", justifyContent: "center", width: "56px", height: "56px", borderRadius: "pill", bg: "action.subtle", color: "action.text", "& svg": { width: "24px", height: "24px" } }),
  emptyTitle: css({ fontSize: "sm", fontWeight: 700, color: "fg" }),
  emptyBody: css({ textStyle: "caption", color: "fg.muted", maxWidth: "14rem" }),
  stack: css({ display: "flex", flexDirection: "column", gap: "3" }),
  section: css({ display: "grid", gap: "0.5" }),
  sectionLabel: css({ display: "flex", alignItems: "center", gap: "1.5", px: "2.5", pt: "2", pb: "1", textStyle: "sidebarSection", color: "fg.muted", "& svg": { width: "14px", height: "14px" } }),
  grow: css({ flex: 1, minWidth: 0, truncate: true }),
  row: cx(room(), css({ width: "100%", minHeight: "48px", py: "1.5", border: 0, bg: "transparent", textAlign: "left", cursor: "pointer" })),
  // The DM row is a div around two buttons; the selected look follows the inner button.
  rowSelected: css({ "&:has(> button[aria-current=page])": { bg: "state.selected", color: "fg", fontWeight: 700 } }),
  rowButton: css({ display: "flex", flex: 1, minWidth: 0, alignItems: "center", gap: "2.5", border: 0, p: 0, bg: "transparent", color: "inherit", font: "inherit", textAlign: "left", cursor: "pointer" }),
  lines: css({ display: "flex", flexDirection: "column", flex: 1, minWidth: 0 }),
  line: css({ display: "flex", alignItems: "center", gap: "1.5", minWidth: 0 }),
  name: css({ flex: 1, minWidth: 0, truncate: true, color: "fg" }),
  sub: css({ flex: 1, minWidth: 0, truncate: true, textStyle: "caption", fontWeight: 400, color: "fg.muted" }),
  time: css({ textStyle: "timestamp", color: "fg.muted", fontVariantNumeric: "tabular-nums", flex: "none" }),
  groupIcon: cx(avatar({ size: "sm" }), css({ "& svg": { width: "16px", height: "16px", color: "inherit" } })),
  badges: css({ display: "flex", alignItems: "center", gap: "1", flex: "none" }),
  livePill: css({
    display: "inline-flex",
    alignItems: "center",
    gap: "1",
    height: "20px",
    px: "1.5",
    flex: "none",
    borderRadius: "pill",
    bg: "live.subtle",
    color: "live.text",
    textStyle: "caption",
    fontWeight: 600,
    "& svg": { width: "12px", height: "12px", color: "inherit" },
    "&[data-muted]": { bg: "surface.2", color: "fg.muted" },
  }),
  mentionBadge: css({ bg: "live", color: "live.fg" }),
  rowAction: css({ opacity: 0.7, _hover: { opacity: 1 }, "&:focus-visible": { opacity: 1 } }),
  callRow: css({
    "--tone": "{colors.live.text}",
    "--tone-bg": "{colors.live.subtle}",
    display: "flex",
    alignItems: "center",
    gap: "2",
    minHeight: "64px",
    px: "3",
    py: "2.5",
    borderRadius: "control",
    border: "1px solid color-mix(in oklab, var(--tone) 30%, transparent)",
    bg: "var(--tone-bg)",
    color: "fg",
    "&[data-tone=primary]": { "--tone": "{colors.action.text}", "--tone-bg": "{colors.action.subtle}" },
    "&[data-tone=warning]": { "--tone": "{colors.warning.text}", "--tone-bg": "{colors.warning.subtle}" },
    "&[data-active]": { boxShadow: "inset 0 0 0 1px var(--tone)" },
  }),
  callFace: css({ position: "relative", flex: "none" }),
  callBadge: css({
    position: "absolute",
    right: "-4px",
    bottom: "-4px",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    width: "16px",
    height: "16px",
    borderRadius: "pill",
    border: "1px solid var(--tone)",
    bg: "surface",
    color: "var(--tone)",
    "& svg": { width: "10px", height: "10px" },
  }),
  eyebrow: css({ display: "flex", alignItems: "center", gap: "1", textStyle: "caption", fontWeight: 600, color: "var(--tone)" }),
  dot: css({ width: "6px", height: "6px", borderRadius: "pill", bg: "var(--tone)", flex: "none" }),
  callTitle: css({ display: "block", truncate: true, fontSize: "sm", fontWeight: 700, color: "fg" }),
  callMeta: css({ display: "block", truncate: true, textStyle: "caption", color: "fg.muted" }),
  pill: css({ flex: "none", px: "2", py: "1", borderRadius: "pill", border: "1px solid var(--tone)", textStyle: "caption", fontWeight: 600, color: "var(--tone)" }),
  arrow: css({ width: "14px", height: "14px", flex: "none", color: "var(--tone)" }),
  endButton: css({ color: "danger.text", _hover: { bg: "danger.subtle", color: "danger.text" } }),
};
</script>

<template>
  <div :class="cls.root">
    <div :class="cls.head">
      <MessageCircle :class="cls.headIcon" aria-hidden="true" />
      <h2 :class="cls.title">Direct messages</h2>
      <AppTooltip label="New message">
        <button
          :class="iconbtn()"
          aria-label="New message"
          type="button"
          @click="emit('newDm')"
        >
          <Plus aria-hidden="true" />
        </button>
      </AppTooltip>
    </div>

    <div :class="cls.scroll">
      <p :class="cls.srOnly" role="status" aria-live="polite" aria-atomic="true">
        {{ activeCallStatusMessage }}
      </p>
      <div v-if="visibleGroupDms.length === 0 && visibleConversations.length === 0 && activeCallRows.length === 0" :class="cls.empty">
        <span :class="cls.emptyIcon" aria-hidden="true"><MessageCircle /></span>
        <div :class="cls.emptyTitle">No conversations yet</div>
        <div :class="cls.emptyBody">Start one with the + button above.</div>
      </div>

      <div v-else :class="cls.stack">
        <section
          v-if="visibleThreadEntries.length > 0"
          :class="cls.section"
          aria-label="Direct message threads"
        >
          <div :class="cls.sectionLabel">
            <MessagesSquare aria-hidden="true" />
            <span :class="cls.grow">Threads</span>
            <span :class="badge({ quiet: true })" aria-hidden="true">{{ visibleThreadEntries.length }}</span>
          </div>
          <button
            v-for="entry in visibleThreadEntries"
            :key="entry.threadId"
            :class="cls.row"
            type="button"
            :aria-label="threadEntryLabel(entry)"
            @click="emit('selectThread', entry.threadId)"
          >
            <MessagesSquare aria-hidden="true" />
            <span :class="cls.lines">
              <span :class="cls.name">{{ threadEntryTitle(entry) }}</span>
              <span :class="cls.sub">
                {{ entry.count }} {{ entry.count === 1 ? 'reply' : 'replies' }}<template v-if="entry.lastTs"> · {{ formatTimelineStamp(entry.lastTs) }}</template>
              </span>
            </span>
          </button>
        </section>

        <section
          v-if="activeCallRows.length > 0"
          :class="cls.section"
          aria-label="Active direct calls"
        >
          <div :class="cls.sectionLabel">
            <PhoneCall aria-hidden="true" />
            <span :class="cls.grow">Active calls</span>
            <span :class="badge({ quiet: true })" aria-hidden="true">{{ activeCallRows.length }}</span>
          </div>
          <div
            v-for="row in activeCallRows"
            :key="`dm-call:${row.peerJid}:${row.activity.sid}`"
            :class="cls.callRow"
            :data-tone="callActivityVisualTone(row.activity)"
            :data-active="isActiveConversation(row.peerJid) ? '' : undefined"
          >
            <button
              :class="cls.rowButton"
              type="button"
              :aria-current="isActiveConversation(row.peerJid) ? 'page' : undefined"
              :aria-label="activeCallRowLabel(row)"
              @click="selectCallActivity(row.activity)"
            >
              <span :class="cls.callFace">
                <UserAvatar :name="row.title" :jid="conversationPeerAvatarJid(row.peerJid)" size="sm" />
                <span :class="cls.callBadge">
                  <Video v-if="hasKnownDmCallMedia(row.activity) && row.activity.media.video" aria-hidden="true" />
                  <PhoneIncoming v-else-if="row.activity.state === 'ringing' && row.activity.direction === 'incoming'" aria-hidden="true" />
                  <PhoneOutgoing v-else-if="row.activity.state === 'ringing' && row.activity.direction === 'outgoing'" aria-hidden="true" />
                  <Phone v-else aria-hidden="true" />
                </span>
              </span>
              <span :class="cls.lines">
                <span :class="cls.eyebrow">
                  <span :class="cls.dot" aria-hidden="true" />
                  <span :class="cls.grow">{{ row.eyebrow }}</span>
                </span>
                <span :class="cls.callTitle">{{ row.title }}</span>
                <span :class="cls.callMeta">
                  {{ row.meta }}<template v-if="row.description"> · {{ row.description }}</template><template v-if="row.since"> · {{ row.since }}</template>
                </span>
              </span>
              <span :class="cls.pill" aria-hidden="true">{{ row.action }}</span>
              <ArrowRight :class="cls.arrow" aria-hidden="true" />
            </button>
            <AppTooltip v-if="canEndCallActivity(row.activity)" :label="endCallActivityLabel(row)">
              <button
                type="button"
                :class="[iconbtn({ size: 'sm' }), cls.endButton]"
                :aria-label="endCallActivityLabel(row)"
                @click="endCallActivity(row.activity)"
              >
                <PhoneOff aria-hidden="true" />
              </button>
            </AppTooltip>
          </div>
        </section>

        <section
          v-if="visibleGroupDms.length > 0"
          :class="cls.section"
          aria-label="Group direct messages"
        >
          <div :class="cls.sectionLabel">
            <MessagesSquare aria-hidden="true" />
            <span :class="cls.grow">Groups</span>
          </div>
          <button
            v-for="group in visibleGroupDms"
            :key="group.roomJid"
            :class="cls.row"
            type="button"
            :data-unread="(group.unreadCount ?? 0) > 0 ? '' : undefined"
            :aria-current="normalizedPeerJid(activeGroupDmRoomJid ?? '') === normalizedPeerJid(group.roomJid) ? 'page' : undefined"
            :aria-label="groupDmRowLabel(group)"
            @click="emit('selectGroupDm', group.roomJid)"
          >
            <span :class="cls.groupIcon"><MessagesSquare aria-hidden="true" /></span>
            <span :class="cls.lines">
              <span :class="cls.name">{{ group.name }}</span>
              <span :class="cls.sub">
                <template v-if="hasGroupCallActivity(group.roomJid)">{{ groupCallActivityLabel(group.roomJid) }}</template>
                <template v-else>Group message</template>
              </span>
            </span>
            <span :class="cls.badges">
              <span
                v-if="hasGroupCallActivity(group.roomJid)"
                :class="cls.livePill"
                :title="groupCallMediaLabel(group.roomJid)"
                :aria-label="groupCallMediaLabel(group.roomJid)"
              >
                <PhoneCall aria-hidden="true" />
                <span>{{ groupCallMediaLabel(group.roomJid) }}</span>
              </span>
              <span
                v-if="(group.mentionCount ?? 0) > 0"
                :class="[badge(), cls.mentionBadge]"
                aria-hidden="true"
              >@{{ group.mentionCount }}</span>
              <span
                v-if="(group.unreadCount ?? 0) > 0"
                :class="badge()"
                aria-hidden="true"
              >{{ group.unreadCount }}</span>
            </span>
          </button>
        </section>

        <section :class="cls.section">
          <div
            v-for="conversation in visibleConversations"
            :key="conversation.peerJid"
            :class="[cls.row, cls.rowSelected]"
            :data-unread="conversation.unreadCount > 0 ? '' : undefined"
            @click="emit('selectDm', conversation.peerJid)"
          >
            <button
              :class="cls.rowButton"
              type="button"
              :aria-current="isActiveConversation(conversation.peerJid) ? 'page' : undefined"
              :aria-label="conversationRowLabel(conversation)"
              @click.stop="emit('selectDm', conversation.peerJid)"
            >
              <UserAvatar
                :name="conversation.peerUsername"
                :jid="conversationPeerAvatarJid(conversation.peerJid)"
                :presence="presenceOf(conversation.presenceShow)"
                size="sm"
                :in-call="peerInCall(conversation.peerJid)"
              />
              <span :class="cls.lines">
                <span :class="cls.line">
                  <span :class="cls.name">{{ conversation.peerUsername }}</span>
                  <span v-if="conversation.lastMessageAt" :class="cls.time">
                    {{ formatTimelineStamp(conversation.lastMessageAt) }}
                  </span>
                </span>
                <span :class="cls.line">
                  <span :class="cls.sub">{{ preview(conversation.lastMessageBody) }}</span>
                  <span
                    v-if="hasCallActivity(conversation.peerJid)"
                    :class="cls.livePill"
                    :data-muted="isHiddenCurrentCallPeer(conversation.peerJid) ? '' : undefined"
                    :title="callActivityLabel(conversation.peerJid)"
                    :aria-label="callActivityLabel(conversation.peerJid)"
                  >
                    <PhoneCall aria-hidden="true" />
                    <span>{{ isHiddenCurrentCallPeer(conversation.peerJid) ? 'Current call' : callActivityLabel(conversation.peerJid) }}</span>
                  </span>
                  <span v-if="conversation.unreadCount > 0" :class="badge()">
                    {{ conversation.unreadCount }}
                  </span>
                </span>
              </span>
            </button>
            <AppTooltip :label="addPeopleLabel(conversation)">
              <button
                :class="[iconbtn({ size: 'sm' }), cls.rowAction]"
                type="button"
                :aria-label="addPeopleLabel(conversation)"
                @click.stop="emit('addPeopleToDm', conversation.peerJid)"
              >
                <UserPlus aria-hidden="true" />
              </button>
            </AppTooltip>
          </div>
        </section>
      </div>
    </div>
  </div>
</template>
