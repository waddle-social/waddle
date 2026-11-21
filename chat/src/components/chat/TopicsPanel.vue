<script setup lang="ts">
import { computed, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { CalendarDays, Hash, Inbox, ListTree, MessageSquareText, MessagesSquare, Phone, PhoneOff, Plus, Settings, Users, ChevronDown, ChevronRight, MessageCircle, Video } from "lucide-vue-next";
import { isForumChannel as detectForumChannel } from "@/lib/channel-types";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { ChannelThreadInboxEntry } from "@/channels/inbox";
import { groupChannelsBySpace } from "@/lib/channel-grouping";
import {
  callParticipantCountForChannel,
  callRoomJidForChannel,
  collapsedGroupBadgeModel,
  mucCallParticipantPreview,
  refreshedMucCallRooms,
  type RefreshedMucCallRoom,
} from "@/lib/calls/muc-call-indicators";
import { mucCallMediaForRoom, normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { $callState } from "@/lib/calls/call-store";
import { readRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import type { CallMedia } from "@/lib/calls/types";
import type { MemberLoadState } from "@/waddles/directory";
import Skeleton from "@/components/ui/Skeleton.vue";
import { css, cx } from "styled-system/css";
import { badge, iconbtn, liveDot, room, tag } from "styled-system/recipes";

const props = defineProps<{
  waddle: SpaceSummary | null;
  spaces: SpaceSummary[];
  channels: ChannelSummary[];
  activeChannelId: string | null;
  canManageChannels: boolean;
  canManageCommunity: boolean;
  isLoading: boolean;
  memberCount: number | null;
  memberState: MemberLoadState;
  activeChannelJids: Set<string>;
  collapsedGroupIds: Set<string>;
  channelUnreadMap?: Record<string, { unread: number; mentions: number }>;
  threadEntriesFn?: (roomJid: string) => ChannelThreadInboxEntry[];
  roomAvatarHashes?: Record<string, string>;
  /** Currently-active community pseudo-channel (feed / events),
   * or null when a real channel is active. Drives the
   * highlight state on the top-of-sidebar rows. */
  activeCommunitySurface?: "feed" | "events" | null;
  /** Counts surfaced as dot/badge on the pseudo-channel rows. */
  feedNewCount?: number;
  storiesActiveCount?: number;
  upcomingEventCount?: number;
  /** True when the global Threads view is currently active. */
  isThreadsActive?: boolean;
  /** Unread-thread count for the global Threads view badge. */
  threadsUnreadCount?: number;
  /** True when the global Unread view is currently active. */
  isUnreadActive?: boolean;
  /** Total unread count for the global Unread view badge. */
  unreadTotalCount?: number;
  /**
   * Per-room count of occupants currently advertising an active
   * XEP-0272 Muji presence (i.e. in the room's group call). Keyed
   * by the room's full JID (e.g. `room@conference.example.com`).
   * Drives a pulsing Phone+count chip next to channel rows so
   * users see "this channel has an ongoing call" without opening
   * the channel — matches Slack-Huddle / Discord-voice patterns.
   */
  callParticipantCounts?: Record<string, number>;
  /** Per-room nick list from XEP-0272 Muji presence. Used only for
   * richer call affordance copy; counts remain authoritative for
   * visibility. */
  callParticipants?: Record<string, string[]>;
  callMediaByRoom?: Record<string, CallMedia>;
  /** Trusted MUC service domain for id-only channel rows. */
  managedMucDomain?: string | null;
}>();

const callState = useStore($callState);

// Panda classes (static). Rows reuse the sidebar's `room` recipe.
const rootClass = css({ display: "flex", flexDirection: "column", minHeight: 0, color: "fg" });
const headClass = css({
  display: "flex",
  alignItems: "center",
  gap: "1",
  padding: "{spacing.1} {spacing.1} {spacing.2} {spacing.3}",
  "& h3": { margin: 0, flex: 1, minWidth: 0, font: "700 13px/1.3 {fonts.body}", color: "fg.muted" },
});
const smIconClass = iconbtn({ size: "sm" });
const listClass = css({ display: "flex", flexDirection: "column", gap: "0.5", padding: "0 {spacing.1} {spacing.2}" });
const groupClass = css({ display: "flex", flexDirection: "column", gap: "0.5" });
const rowClass = cx(room(), css({ flex: 1, cursor: "pointer", border: 0, bg: "transparent", width: "100%", textAlign: "left", font: "inherit", "& .wd-badge, & .wd-tag": { flex: "none" } }));
const threadRowClass = cx(rowClass, css({ minHeight: "36px", pl: "8", fontSize: "sm", "& svg": { width: "16px", height: "16px" } }));
const rowWrapClass = css({ display: "flex", alignItems: "center", gap: "1" });
const metaClass = css({ display: "block", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", font: "400 12px/1.35 {fonts.body}", color: "fg.muted" });
// Server name beside a room on another server; capped so the room name keeps most of the row.
const remoteHostClass = cx(metaClass, css({ minWidth: 0, maxWidth: "45%" }));
const nameStackClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", "& > :first-child": { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", color: "fg", fontWeight: 600 } });
const countClass = css({ flex: "none", font: "600 12px/1 {fonts.body}", fontVariantNumeric: "tabular-nums", color: "fg.soft" });
const sectionHeadClass = css({
  display: "flex",
  alignItems: "center",
  gap: "1",
  borderRadius: "control",
  _hover: { bg: "state.hover" },
});
const sectionToggleClass = css({
  display: "flex",
  flex: 1,
  minWidth: 0,
  alignItems: "center",
  gap: "1.5",
  minHeight: "36px",
  padding: "{spacing.2.5} {spacing.2} {spacing.1} {spacing.2.5}",
  border: 0,
  bg: "transparent",
  textAlign: "left",
  cursor: "pointer",
  font: "700 13px/1.3 {fonts.body}",
  color: "fg.muted",
  _hover: { color: "fg" },
  "& > svg": { width: "16px", height: "16px", flex: "none", transition: "rotate {durations.base} {easings.out}" },
  "&[aria-expanded=false] > svg:first-child": { rotate: "-90deg" },
});
const callsHeadClass = css({ display: "flex", alignItems: "center", gap: "1.5", padding: "{spacing.2.5} {spacing.2} {spacing.1} {spacing.2.5}", font: "700 13px/1.3 {fonts.body}", color: "fg.muted" });
const sectionLabelClass = css({ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const callChipClass = css({
  display: "inline-flex",
  alignItems: "center",
  gap: "1",
  flex: "none",
  height: "22px",
  px: "2",
  borderRadius: "pill",
  bg: "live.subtle",
  color: "live.text",
  boxShadow: "inset 0 0 0 1px {colors.live}",
  font: "700 12px/1 {fonts.body}",
  fontVariantNumeric: "tabular-nums",
  "& svg": { width: "14px", height: "14px", color: "currentColor" },
});
const callActionClass = css({ display: "none", "@media (min-width: 1536px)": { display: "inline" } });
const callActionTagClass = cx(tag(), css({ display: "none", color: "live.text", borderColor: "live", xl: { display: "inline-flex" } }));
const callIconClass = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  width: "28px",
  height: "28px",
  flex: "none",
  borderRadius: "control",
  bg: "live.subtle",
  color: "live.text",
  "& svg": { width: "16px", height: "16px", color: "currentColor" },
});
const leaveClass = cx(iconbtn(), css({ color: "danger.text", _hover: { bg: "danger.subtle", color: "danger.text" } }));
const forumTagClass = tag();
const dotClass = badge({ dot: true });
const emptyGroupClass = css({ display: "flex", alignItems: "center", justifyContent: "space-between", gap: "2", padding: "{spacing.1.5} {spacing.2.5} {spacing.1.5} {spacing.8}", fontSize: "sm", color: "fg.muted" });
const createClass = css({
  display: "inline-flex",
  alignItems: "center",
  gap: "1",
  height: "32px",
  px: "2.5",
  border: 0,
  borderRadius: "pill",
  bg: "action.subtle",
  color: "action.text",
  font: "600 13px/1 {fonts.body}",
  cursor: "pointer",
  _hover: { bg: "action", color: "action.fg" },
  "& svg": { width: "14px", height: "14px" },
});
const emptyClass = css({ display: "flex", flexDirection: "column", alignItems: "center", gap: "3", padding: "{spacing.10} {spacing.4}", textAlign: "center", fontSize: "sm", color: "fg.muted" });
const newClass = css({
  display: "inline-flex",
  alignItems: "center",
  gap: "1.5",
  height: "36px",
  px: "3",
  borderRadius: "control",
  border: "1px solid {colors.border}",
  bg: "surface",
  color: "fg",
  font: "600 13px/1 {fonts.body}",
  cursor: "pointer",
  _hover: { bg: "state.hover" },
  "& svg": { width: "16px", height: "16px" },
});
const skelGroupClass = css({ display: "flex", flexDirection: "column", gap: "1" });
const skelHeadClass = css({ display: "flex", alignItems: "center", gap: "1.5", padding: "{spacing.2.5} {spacing.2} {spacing.1}" });
const skelRowClass = css({ display: "flex", alignItems: "center", gap: "2.5", minHeight: "rowH", px: "2.5" });
const footClass = css({ padding: "{spacing.2} {spacing.1} 0", borderTop: "1px solid {colors.border.soft}" });
const footRowClass = cx(rowClass, css({ "& svg:last-child": { width: "16px", height: "16px", transition: "translate {durations.base} {easings.out}" }, "&:hover svg:last-child": { translate: "2px 0" } }));

/**
 * Resolve the channel's MUC room JID and look up the live-call
 * count for it. Channel rows in the sidebar may store the JID
 * directly (`channel.jid`) or only the channel id; in the latter
 * case we combine the id with the managed MUC domain so localpart
 * fallback cannot cross domains.
 */
function callParticipantCount(channel: ChannelSummary): number {
  return callParticipantCountForChannel(
    channel,
    props.callParticipantCounts,
    props.activeChannelJids,
    props.managedMucDomain,
  );
}

function callRoomJid(channel: ChannelSummary): string {
  return callRoomJidForChannel(
    channel,
    props.callParticipantCounts,
    props.activeChannelJids,
    props.managedMucDomain,
  );
}

function channelCallAction(channel: ChannelSummary): "join" | "return" | "open" | null {
  if (callParticipantCount(channel) <= 0) return null;
  const roomJid = callRoomJid(channel);
  if (!roomJid) return null;
  return groupCallAction(roomJid);
}

function groupCallMedia(roomJid: string): CallMedia {
  return mucCallMediaForRoom(roomJid, props.callMediaByRoom);
}

function groupCallMediaLabel(roomJid: string): string {
  return groupCallMedia(roomJid).video ? "video call" : "call";
}

function groupCallAction(roomJid: string): "join" | "return" | "open" {
  const state = callState.value;
  if (
    (state.phase === "active" || state.phase === "muc-pending") &&
    state.kind === "muc" &&
    normalizeMucCallRoomJid(state.peer) === roomJid
  ) {
    return "return";
  }
  if (state.phase !== "idle" && state.phase !== "ended") return "open";
  return "join";
}

function groupCallActionLabel(roomJid: string): string {
  switch (groupCallAction(roomJid)) {
    case "join":
      return canLeaveRetainedGroupCall(roomJid) ? "Rejoin" : "Join";
    case "return":
      return "Return";
    case "open":
      return "Open";
  }
}

function channelCallActionLabel(channel: ChannelSummary): string {
  const roomJid = callRoomJid(channel);
  if (!roomJid || channelCallAction(channel) === null) return "";
  return groupCallActionLabel(roomJid);
}

function channelCallBadgeLabel(channel: ChannelSummary): string {
  const count = callParticipantCount(channel);
  const noun = count === 1 ? "person" : "people";
  const action = channelCallActionLabel(channel);
  const media = groupCallMediaLabel(callRoomJid(channel));
  const label = action
    ? `${action} live ${media}, ${count} ${noun}`
    : `Live ${media}, ${count} ${noun}`;
  const preview = callParticipantPreviewForRoom(callRoomJid(channel));
  return preview ? `${label}: ${preview}` : label;
}

function refreshedGroupCallBadgeLabel(row: RefreshedMucCallRoom): string {
  const noun = row.participantCount === 1 ? "person" : "people";
  const label = `${groupCallActionLabel(row.roomJid)} live ${groupCallMediaLabel(row.roomJid)}, ${row.participantCount} ${noun}`;
  const preview = callParticipantPreviewForRoom(row.roomJid);
  return preview ? `${label}: ${preview}` : label;
}

function refreshedGroupCallRowLabel(row: RefreshedMucCallRoom): string {
  const noun = row.participantCount === 1 ? "person" : "people";
  const action = groupCallAction(row.roomJid);
  const preview = callParticipantPreviewForRoom(row.roomJid);
  const base = [
    `${row.title}, group ${groupCallMediaLabel(row.roomJid)} syncing`,
    `${row.participantCount} ${noun}`,
    ...(preview ? [`${preview} in call`] : []),
  ].join(", ");
  if (action === "join") return `${base}, click to join call`;
  if (action === "return") return `${base}, click to return to call`;
  return `${base}, click to open call`;
}

function isActiveGroupCallRow(row: RefreshedMucCallRoom): boolean {
  return groupCallAction(row.roomJid) === "return";
}

function callParticipantPreviewForRoom(roomJid: string): string {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized) return "";
  return mucCallParticipantPreview(props.callParticipants?.[normalized]);
}

function refreshedGroupCallMeta(row: RefreshedMucCallRoom): string {
  const preview = callParticipantPreviewForRoom(row.roomJid);
  return preview ? `${preview} in call` : "Group call syncing";
}

function hasActivity(channelId: string): boolean {
  for (const jid of props.activeChannelJids) {
    if (jid.startsWith(channelId + "@") || jid.includes(channelId)) {
      return true;
    }
  }
  return false;
}

const channelGroups = computed(() =>
  groupChannelsBySpace(props.spaces, props.channels).map((group) => ({
    ...group,
    channels: group.channels.map((channel) => ({
      channel,
      unread: props.channelUnreadMap?.[channel.id] ?? { unread: 0, mentions: 0 },
    })),
  })),
);
const refreshedGroupCallRows = computed(() =>
  refreshedMucCallRooms({
    channels: props.channels,
    activeChannelJids: props.activeChannelJids,
    managedMucDomain: props.managedMucDomain,
    callParticipantCounts: props.callParticipantCounts,
    callMediaByRoom: props.callMediaByRoom,
  }),
);

function groupUnreadSummary(group: (typeof channelGroups.value)[number]) {
  return group.channels.reduce(
    (summary, { channel, unread }) => ({
      unread: summary.unread + unread.unread,
      mentions: summary.mentions + unread.mentions,
      hasActivity: summary.hasActivity || hasActivity(channel.id),
      callCount: summary.callCount + callParticipantCount(channel),
    }),
    { unread: 0, mentions: 0, hasActivity: false, callCount: 0 },
  );
}

function groupBadgeModel(group: (typeof channelGroups.value)[number]) {
  return collapsedGroupBadgeModel(groupUnreadSummary(group));
}

function groupToggleLabel(group: (typeof channelGroups.value)[number]) {
  const state = isGroupCollapsed(group.id) ? "collapsed" : "expanded";
  const channelCount = `${group.channels.length} ${group.channels.length === 1 ? "channel" : "channels"}`;
  const summary = groupUnreadSummary(group);
  const activity = activityLabel(summary);

  return `${group.name}, ${state}, ${channelCount}, ${activity}`;
}

function activityLabel(summary: { unread: number; mentions: number; hasActivity: boolean; callCount?: number }) {
  const parts: string[] = [];
  if (summary.callCount && summary.callCount > 0) {
    parts.push(`call ongoing with ${summary.callCount} ${summary.callCount === 1 ? "participant" : "participants"}`);
  }
  if (summary.mentions > 0) {
    parts.push(`${summary.mentions} ${summary.mentions === 1 ? "mention" : "mentions"}`);
  }
  if (summary.unread > 0) {
    parts.push(`${summary.unread} unread`);
  }
  if (summary.hasActivity) {
    parts.push("active");
  }
  return parts.length > 0 ? parts.join(", ") : "no unread activity";
}

function memberCountLabel() {
  if (props.memberCount !== null) return String(props.memberCount);
  if (props.memberState === "loading") return "syncing";
  if (props.memberState === "unavailable") return "unavailable";
  return "0";
}

function channelRowLabel(
  channel: ChannelSummary,
  unread: { unread: number; mentions: number },
  isActive: boolean,
) {
  const roomJid = callRoomJid(channel);
  const preview = callParticipantCount(channel) > 0
    ? callParticipantPreviewForRoom(roomJid)
    : "";
  // A room on another server says where it is: two servers can each have
  // a room of the same name.
  const name = channel.remoteHost ? `${channel.name} on ${channel.remoteHost}` : channel.name;
  const label = `${name}, ${isActive ? "selected" : "not selected"}, ${activityLabel({
    unread: unread.unread,
    mentions: unread.mentions,
    hasActivity: hasActivity(channel.id),
    callCount: callParticipantCount(channel),
  })}${preview ? `, ${preview} in call` : ""}`;
  const action = !isActive ? channelCallAction(channel) : null;
  if (action === "join") return `${label}, click to join call`;
  if (action === "return") return `${label}, click to return to call`;
  if (action === "open") return `${label}, click to open call`;
  return label;
}

function canLeaveRetainedGroupCall(roomJid: string): boolean {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized) return false;
  const state = callState.value;
  if (
    (state.phase === "active" || state.phase === "muc-pending") &&
    state.kind === "muc" &&
    normalizeMucCallRoomJid(state.peer) === normalized
  ) {
    return false;
  }
  return readRoomHasActiveCall(normalized).localResourceInCall;
}

function canLeaveChannelCall(channel: ChannelSummary): boolean {
  const roomJid = callRoomJid(channel);
  return !!roomJid && canLeaveRetainedGroupCall(roomJid);
}

function leaveChannelCallLabel(channel: ChannelSummary): string {
  return `Leave ${channel.name} call`;
}

function leaveRefreshedGroupCallLabel(row: RefreshedMucCallRoom): string {
  return `Leave ${row.title} call`;
}

function threadRowLabel(thread: ChannelThreadInboxEntry) {
  const summary = thread.unread > 0 ? `${thread.unread} unread` : "no unread activity";
  const replies = `${thread.replyCount} ${thread.replyCount === 1 ? "reply" : "replies"}`;
  return `${truncateTitle(thread.title)}, thread, ${replies}, ${summary}`;
}

function channelIcon(channel: ChannelSummary) {
  return detectForumChannel(channel) ? MessagesSquare : Hash;
}

function channelThreads(channel: ChannelSummary): ChannelThreadInboxEntry[] {
  if (!props.threadEntriesFn) return [];
  if (channel.jid) return props.threadEntriesFn(channel.jid);

  // Find the room JID from the active channel JIDs
  for (const jid of props.activeChannelJids) {
    if (jid.startsWith(channel.id + "@") || jid.includes(channel.id)) {
      return props.threadEntriesFn(jid);
    }
  }
  return [];
}

function isActiveChannelPage(channel: ChannelSummary): boolean {
  return props.activeChannelId === channel.id;
}

function truncateTitle(title: string | undefined, maxLen = 28): string {
  if (!title) return "Untitled thread";
  return title.length > maxLen ? title.slice(0, maxLen) + "…" : title;
}

const emit = defineEmits<{
  selectChannel: [id: string | null, roomJid?: string];
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  leaveChannelCall: [roomJid: string];
  selectThread: [channelId: string, threadId: string];
  selectCommunitySurface: [surface: "feed" | "events"];
  selectThreadsView: [];
  selectUnreadView: [];
  createChannel: [];
  createChannelInSpace: [spaceId: string | null];
  openSettings: [];
  openMembers: [];
  updateCollapsedGroupIds: [groupIds: Set<string>];
}>();

function selectChannelRow(channel: ChannelSummary): void {
  const roomJid = callRoomJid(channel);
  if (!isActiveChannelPage(channel) && channelCallAction(channel) === "join" && roomJid) {
    emit("joinChannelCall", channel.id, roomJid, groupCallMedia(roomJid));
    return;
  }
  emit("selectChannel", channel.id, roomJid || undefined);
}

function leaveChannelCall(channel: ChannelSummary): void {
  const roomJid = callRoomJid(channel);
  if (!roomJid || !canLeaveRetainedGroupCall(roomJid)) return;
  emit("leaveChannelCall", roomJid);
}

function selectRefreshedGroupCallRow(row: RefreshedMucCallRoom): void {
  if (groupCallAction(row.roomJid) === "join") {
    emit("joinChannelCall", null, row.roomJid, row.media);
    return;
  }
  emit("selectChannel", null, row.roomJid);
}

function leaveRefreshedGroupCall(row: RefreshedMucCallRoom): void {
  if (!canLeaveRetainedGroupCall(row.roomJid)) return;
  emit("leaveChannelCall", row.roomJid);
}

function isGroupCollapsed(groupId: string): boolean {
  return props.collapsedGroupIds.has(groupId);
}

function toggleGroup(groupId: string) {
  const next = new Set(props.collapsedGroupIds);
  if (next.has(groupId)) {
    next.delete(groupId);
  } else {
    next.add(groupId);
  }
  emit("updateCollapsedGroupIds", next);
}

watch(
  () => props.activeChannelId,
  (activeChannelId) => {
    if (!activeChannelId) return;
    const activeGroup = channelGroups.value.find((group) =>
      group.channels.some(({ channel }) => channel.id === activeChannelId),
    );
    if (!activeGroup || !props.collapsedGroupIds.has(activeGroup.id)) return;
    const next = new Set(props.collapsedGroupIds);
    next.delete(activeGroup.id);
    emit("updateCollapsedGroupIds", next);
  },
);
</script>

<template>
  <div :class="rootClass">
    <div :class="headClass">
      <h3>Spaces</h3>
      <button
        v-if="waddle && canManageCommunity"
        :class="smIconClass"
        title="Settings"
        aria-label="Waddle settings"
        type="button"
        @click="emit('openSettings')"
      >
        <Settings aria-hidden="true" />
      </button>
      <button
        v-if="canManageChannels"
        :class="smIconClass"
        title="New channel or space"
        aria-label="New channel or space"
        type="button"
        @click="emit('createChannel')"
      >
        <Plus aria-hidden="true" />
      </button>
    </div>

    <div :class="listClass">
      <section v-if="refreshedGroupCallRows.length > 0" :class="groupClass" aria-label="Active group calls">
        <div :class="callsHeadClass">
          <span :class="liveDot()" aria-hidden="true" />
          <span :class="sectionLabelClass">Active calls</span>
          <span :class="callChipClass" aria-hidden="true">{{ refreshedGroupCallRows.length }}</span>
        </div>
        <div v-for="row in refreshedGroupCallRows" :key="row.key" :class="rowWrapClass">
          <button
            :class="rowClass"
            type="button"
            :aria-current="isActiveGroupCallRow(row) ? 'page' : undefined"
            :aria-label="refreshedGroupCallRowLabel(row)"
            @click="selectRefreshedGroupCallRow(row)"
          >
            <span :class="callIconClass">
              <Video v-if="row.media.video" aria-hidden="true" />
              <Phone v-else aria-hidden="true" />
            </span>
            <span :class="nameStackClass">
              <span>{{ row.title }}</span>
              <span :class="metaClass" :title="row.roomJid">{{ refreshedGroupCallMeta(row) }}</span>
            </span>
            <span :class="callChipClass" :title="refreshedGroupCallBadgeLabel(row)" aria-hidden="true">
              <Video v-if="row.media.video" />
              <Phone v-else />
              {{ row.participantCount }}
            </span>
            <span :class="callActionTagClass" aria-hidden="true">{{ groupCallActionLabel(row.roomJid) }}</span>
          </button>
          <button
            v-if="canLeaveRetainedGroupCall(row.roomJid)"
            type="button"
            :class="leaveClass"
            :title="leaveRefreshedGroupCallLabel(row)"
            :aria-label="leaveRefreshedGroupCallLabel(row)"
            @click="leaveRefreshedGroupCall(row)"
          >
            <PhoneOff aria-hidden="true" />
          </button>
        </div>
      </section>

      <div v-if="isLoading" aria-hidden="true">
        <section v-for="i in 2" :key="`topics-skel-group-${i}`" :class="skelGroupClass">
          <div :class="skelHeadClass">
            <Skeleton width="0.625rem" height="0.625rem" radius="0.125rem" />
            <Skeleton :width="i === 1 ? '40%' : '55%'" height="0.55rem" />
          </div>
          <div v-for="j in i === 1 ? 4 : 3" :key="`topics-skel-row-${i}-${j}`" :class="skelRowClass">
            <Skeleton width="0.875rem" height="0.875rem" radius="0.25rem" />
            <Skeleton :width="`${30 + ((j * 13) % 45)}%`" height="0.6rem" />
          </div>
        </section>
      </div>

      <div v-else-if="channels.length === 0 && spaces.length === 0" :class="emptyClass">
        <span>There are no channels or spaces configured yet.</span>
        <button v-if="canManageChannels" :class="newClass" type="button" @click="emit('createChannel')">
          <Plus aria-hidden="true" />
          New channel or space
        </button>
      </div>

      <template v-else>
        <!-- Community pseudo-channels: community-wide surfaces (Feed,
             Events) and the Unread / Threads views, which are NOT real
             channels. Selecting one swaps the content area. -->
        <section :class="groupClass" aria-label="Community surfaces">
          <button
            type="button"
            :class="rowClass"
            :aria-current="isUnreadActive ? 'page' : undefined"
            @click="emit('selectUnreadView')"
          >
            <Inbox aria-hidden="true" />
            <span class="wd-room__name">Unread</span>
            <span v-if="(unreadTotalCount ?? 0) > 0" :class="badge()" aria-hidden="true">{{ unreadTotalCount }}</span>
          </button>
          <button
            type="button"
            :class="rowClass"
            :aria-current="isThreadsActive ? 'page' : undefined"
            @click="emit('selectThreadsView')"
          >
            <ListTree aria-hidden="true" />
            <span class="wd-room__name">Threads</span>
            <span v-if="(threadsUnreadCount ?? 0) > 0" :class="badge()" aria-hidden="true">{{ threadsUnreadCount }}</span>
          </button>
          <button
            type="button"
            :class="rowClass"
            :aria-current="activeCommunitySurface === 'feed' ? 'page' : undefined"
            @click="emit('selectCommunitySurface', 'feed')"
          >
            <MessageSquareText aria-hidden="true" />
            <span class="wd-room__name">Feed</span>
            <span v-if="((feedNewCount ?? 0) + (storiesActiveCount ?? 0)) > 0" :class="badge()" aria-hidden="true">{{ (feedNewCount ?? 0) + (storiesActiveCount ?? 0) }}</span>
          </button>
          <button
            type="button"
            :class="rowClass"
            :aria-current="activeCommunitySurface === 'events' ? 'page' : undefined"
            @click="emit('selectCommunitySurface', 'events')"
          >
            <CalendarDays aria-hidden="true" />
            <span class="wd-room__name">Events</span>
            <span v-if="(upcomingEventCount ?? 0) > 0" :class="badge({ variant: 'quiet' })" aria-hidden="true">{{ upcomingEventCount }}</span>
          </button>
        </section>

        <section v-for="group in channelGroups" :key="group.id" :class="groupClass">
          <div :class="sectionHeadClass">
            <button
              :class="sectionToggleClass"
              type="button"
              :aria-expanded="!isGroupCollapsed(group.id)"
              :aria-label="groupToggleLabel(group)"
              @click="toggleGroup(group.id)"
            >
              <ChevronDown aria-hidden="true" />
              <span :class="sectionLabelClass">{{ group.name }}</span>
              <!-- Rolled-up activity on a collapsed group. -->
              <template v-if="isGroupCollapsed(group.id)">
                <span v-if="groupBadgeModel(group).callCount > 0" :class="callChipClass" aria-hidden="true">
                  <Phone />
                  {{ groupBadgeModel(group).callCount }}
                </span>
                <span
                  v-if="groupBadgeModel(group).notification?.kind === 'mentions'"
                  :class="badge()"
                  aria-hidden="true"
                >{{ groupBadgeModel(group).notification?.count }}</span>
                <span
                  v-else-if="groupBadgeModel(group).notification?.kind === 'unread'"
                  :class="badge({ variant: 'quiet' })"
                  aria-hidden="true"
                >{{ groupBadgeModel(group).notification?.count }}</span>
                <span
                  v-else-if="groupBadgeModel(group).notification?.kind === 'activity'"
                  :class="dotClass"
                  aria-hidden="true"
                />
              </template>
              <span :class="countClass">{{ group.channels.length }}</span>
            </button>
            <button
              v-if="canManageChannels"
              :class="smIconClass"
              type="button"
              :aria-label="group.space ? `Create channel in ${group.name}` : 'Create standalone channel'"
              @click="emit('createChannelInSpace', group.space?.id ?? null)"
            >
              <Plus aria-hidden="true" />
            </button>
          </div>
          <template v-if="!isGroupCollapsed(group.id)">
            <div v-if="group.channels.length === 0" :class="emptyGroupClass">
              <span>No channels yet.</span>
              <button
                v-if="canManageChannels"
                :class="createClass"
                type="button"
                :aria-label="group.space ? `Create channel in ${group.name}` : 'Create standalone channel'"
                @click="emit('createChannelInSpace', group.space?.id ?? null)"
              >
                <Plus aria-hidden="true" />
                <span>Create</span>
              </button>
            </div>
            <template v-for="{ channel, unread } in group.channels" :key="channel.id">
              <div :class="rowWrapClass">
                <button
                  :class="rowClass"
                  :aria-current="isActiveChannelPage(channel) ? 'page' : undefined"
                  :aria-label="channelRowLabel(channel, unread, isActiveChannelPage(channel))"
                  :data-unread="!isActiveChannelPage(channel) && (unread.unread > 0 || unread.mentions > 0 || hasActivity(channel.id)) ? '' : undefined"
                  type="button"
                  @click="selectChannelRow(channel)"
                >
                  <component :is="channelIcon(channel)" aria-hidden="true" />
                  <span class="wd-room__name">{{ channel.name }}</span>
                  <span
                    v-if="channel.remoteHost"
                    :class="remoteHostClass"
                    :title="`Hosted on ${channel.remoteHost}`"
                  >{{ channel.remoteHost }}</span>
                  <span v-if="detectForumChannel(channel)" :class="forumTagClass">Forum</span>
                  <!-- XEP-0272 Muji "call ongoing in this channel" chip, in
                       the live colour. Suppressed on the active channel:
                       the room header's call button already shows it. -->
                  <span
                    v-if="callParticipantCount(channel) > 0 && !isActiveChannelPage(channel)"
                    :class="callChipClass"
                    :title="channelCallBadgeLabel(channel)"
                    aria-hidden="true"
                  >
                    <Video v-if="groupCallMedia(callRoomJid(channel)).video" />
                    <Phone v-else />
                    {{ callParticipantCount(channel) }}
                    <span v-if="channelCallActionLabel(channel)" :class="callActionClass">· {{ channelCallActionLabel(channel) }}</span>
                  </span>
                  <span
                    v-if="unread.mentions > 0 && !isActiveChannelPage(channel)"
                    :class="badge()"
                    aria-hidden="true"
                  >{{ unread.mentions }}</span>
                  <span
                    v-else-if="unread.unread > 0 && !isActiveChannelPage(channel)"
                    :class="badge({ variant: 'quiet' })"
                    aria-hidden="true"
                  >{{ unread.unread }}</span>
                  <span
                    v-else-if="hasActivity(channel.id) && !isActiveChannelPage(channel)"
                    :class="dotClass"
                    aria-hidden="true"
                  />
                </button>
                <button
                  v-if="canLeaveChannelCall(channel)"
                  type="button"
                  :class="leaveClass"
                  :title="leaveChannelCallLabel(channel)"
                  :aria-label="leaveChannelCallLabel(channel)"
                  @click="leaveChannelCall(channel)"
                >
                  <PhoneOff aria-hidden="true" />
                </button>
              </div>

              <!-- Active threads for this channel -->
              <button
                v-for="thread in channelThreads(channel)"
                :key="thread.threadId"
                :class="threadRowClass"
                type="button"
                :aria-label="threadRowLabel(thread)"
                :data-unread="thread.unread > 0 ? '' : undefined"
                @click="emit('selectThread', channel.id, thread.threadId)"
              >
                <MessageCircle aria-hidden="true" />
                <span class="wd-room__name">{{ truncateTitle(thread.title) }}</span>
                <span v-if="thread.replyCount > 0" :class="countClass">{{ thread.replyCount }}</span>
                <span v-if="thread.unread > 0" :class="badge()" aria-hidden="true">{{ thread.unread }}</span>
              </button>
            </template>
          </template>
        </section>
      </template>
    </div>

    <div v-if="waddle" :class="footClass">
      <button :class="footRowClass" type="button" @click="emit('openMembers')">
        <Users aria-hidden="true" />
        <span class="wd-room__name">Members</span>
        <span :class="countClass">{{ memberCountLabel() }}</span>
        <ChevronRight aria-hidden="true" />
      </button>
    </div>
  </div>
</template>
