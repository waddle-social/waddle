<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useStore } from "@nanostores/vue";
import {
  ArrowRight,
  Hash,
  MessagesSquare,
  Phone,
  PhoneCall,
  PhoneIncoming,
  PhoneOff,
  PhoneOutgoing,
  Video,
} from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { badge, btn, kicker } from "styled-system/recipes";
import SettingsMobileHeader from "@/components/chat/SettingsMobileHeader.vue";
import { pageClasses as pc } from "@/components/chat/inbox-classes";
import { appController } from "@/stores/app-controller";
import type { OccupantPresence } from "@/lib/xmpp/types";
import { cardClasses, liveKicker } from "@/ui/part-classes";
import type { ChannelSummary } from "@/lib/chat-types";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { conversationPeerAvatarJid } from "@/lib/avatars/author-jid";
import { useCallParticipantAvatarJid } from "@/lib/avatars/use-call-participant-avatar";
import Skeleton from "@/components/ui/Skeleton.vue";
import { isForumChannel } from "@/lib/channel-types";
import {
  callActivityDockAction,
  buildCallActivityDockEntries,
  canEndRecoveredDmCallActivity,
  callActivityDockSelection,
  sortCallActivityDockEntries,
  type CallActivityDockEntry,
} from "@/lib/calls/call-activity-dock";
import {
  callEntriesInHuddleCount,
  callEntryActionLabel as callEntryActionLabelFor,
  callEntryCardTone,
  callEntryDescription as callEntryDescriptionFor,
  callEntryDetail as callEntryDetailFor,
  callEntryEyebrow as callEntryEyebrowFor,
  callEntryLabel as callEntryLabelFor,
  callEntryParticipantPreview,
  callEntryVisibleParticipantLabels,
  callEntryVisualTone,
  canLeaveRetainedChannelCallEntry as canLeaveRetainedChannelCallEntryFor,
  endCallEntryButtonText,
  endCallEntryLabel,
  isSameCallEntry,
} from "./home-call-entry-presentation";
import {
  heroGreetingFor,
  heroKickerFor,
  heroQuietMessageFor,
  heroSummaryPartsFor,
  heroTimeOfDayFor,
  type HeroSummary,
  type HeroSummaryPart,
} from "./home-hero";
import { $callState } from "@/lib/calls/call-store";
import { useInCallOverlays } from "@/presence/in-call-overlay-store";
import { $livePresence, liveShow } from "@/presence/live-presence";
import {
  dmCallActivitiesForPeer,
  hasKnownDmCallMedia,
} from "@/lib/calls/dm-call-activity";
import {
  callRoomJidForChannel,
  callParticipantCountForChannel,
} from "@/lib/calls/muc-call-indicators";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { splitKnownPeople } from "@/shell/controllers/use-people-rail";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";
import type { CallMedia } from "@/lib/calls/types";
import { formatTimelineStamp } from "@/channels/timeline";
import type { HomeDashboardProps } from "@/home/dashboard-props";
import {
  channelActivityPreview,
  channelActivityState,
  channelHomeLabel,
  channelUnreadBadgeCount,
  compareChannelActivityPriority,
  dmHomeLabel,
  dmPresenceLabel,
  dmPreviewText,
  type ChannelActivityState,
} from "@/home/activity";

const props = defineProps<HomeDashboardProps>();

const emit = defineEmits<{
  selectChannel: [id: string, roomJid?: string];
  selectChannelRoom: [roomJid: string];
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  selectGroupDm: [roomJid: string];
  joinGroupDmCall: [roomJid: string, media: CallMedia];
  leaveChannelCall: [roomJid: string];
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  selectContact: [jid: string];
  reconnectDm: [peerJid: string, media: CallMedia];
  endDm: [peerJid: string, sid?: string];
  openNav: [];
}>();

// Recipe classes (Panda). Computed once; the variants are static.
const liveCard = cardClasses("live");
const warningCard = cardClasses("warning");
const actionCard = cardClasses("active");
const roomCard = cardClasses("quiet");
const kickerClass = kicker();
const liveKickerClass = liveKicker;
const countClass = badge();
const joinButtonClass = btn({ variant: "live", size: "sm" });
const enterButtonClass = btn({ size: "sm" });
const openPillClass = btn({ size: "sm" });
const leaveButtonClass = btn({ variant: "danger", size: "sm" });
const heroCtaClass = cx(btn({ variant: "primary" }), css({ textDecoration: "none" }));

// App-Home / Empty-Home boards: a page head (date kicker, greeting, summary,
// one primary action), then kicker-titled sections of floe cards and rows.
const pageClass = cx(pc.page, css({ md: { gap: "8" } }));
const greetingClass = css({ margin: 0, font: "700 24px/1.2 {fonts.display}", letterSpacing: "-0.01em", md: { fontSize: "30px" } });
// Phones: greeting takes the full row, the CTA wraps below it.
const heroTextClass = css({ flexBasis: "100%", md: { flexBasis: "0%" } });
const summaryClass = cx(pc.sub, css({ maxWidth: "52ch", "& > strong": { fontWeight: 600, color: "fg", fontVariantNumeric: "tabular-nums" } }));
const summarySepClass = css({ color: "fg.soft" });
const liveDotClass = css({ display: "inline-block", width: "8px", height: "8px", borderRadius: "pill", bg: "live", mr: "2", verticalAlign: "middle" });
const sectionClass = css({ display: "flex", flexDirection: "column", gap: "3" });
const sectionHeadClass = css({ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: "3" });
const headingClass = cx(kickerClass, css({ margin: 0 }));
const liveHeadingClass = cx(liveKickerClass, css({ margin: 0 }));
const captionClass = css({ margin: 0, textStyle: "caption", color: "fg.muted" });
const cardGridClass = css({ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))", gap: "3" });
const twoColClass = css({ display: "grid", gap: "2", md: { gridTemplateColumns: "repeat(2, minmax(0, 1fr))" } });
const listClass = css({ display: "flex", flexDirection: "column", gap: "2" });
const cardMinClass = css({ minWidth: 0 });
const kickerRowClass = css({ display: "flex", alignItems: "center", gap: "1.5", minWidth: 0, "& svg": { width: "14px", height: "14px", flex: "none" } });
const ellipsisClass = css({ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const roomTitleClass = cx(roomCard.title, css({ display: "flex", alignItems: "center", gap: "1.5", minWidth: 0, "& svg": { width: "16px", height: "16px", flex: "none", color: "fg.soft" } }));
const clampClass = css({ lineClamp: 2, overflowWrap: "anywhere" });
const participantsClass = css({ display: "flex", alignItems: "center", gap: "2", minWidth: 0 });
const faceStackClass = css({ display: "flex", flex: "none", "& > span": { ml: "-6px", borderRadius: "pill", boxShadow: "0 0 0 2px {colors.surface}" }, "& > span:first-child": { ml: 0 } });
const footerWrapClass = cx(liveCard.footer, css({ flexWrap: "wrap" }));
const metaClass = css({ flex: 1, minWidth: 0, fontSize: "xs", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const stampClass = css({ ml: "auto", fontSize: "xs", color: "fg.muted", fontVariantNumeric: "tabular-nums" });
// A floe row: one button per room or person.
const rowClass = css({
  display: "flex",
  alignItems: "center",
  gap: "3",
  minWidth: 0,
  minHeight: "56px",
  padding: "{spacing.2.5} {spacing.4}",
  borderRadius: "panel",
  border: "1px solid {colors.border.soft}",
  bg: "surface",
  color: "fg",
  textAlign: "left",
  cursor: "pointer",
  transition: "background-color {durations.fast} {easings.out}",
  _hover: { bgImage: "linear-gradient({colors.state.hover}, {colors.state.hover})" },
  _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
});
const mentionRowClass = cx(rowClass, css({ boxShadow: "inset 3px 0 0 {colors.gold}" }));
const quietRowClass = cx(rowClass, css({ bg: "transparent", color: "fg.muted", _hover: { color: "fg" } }));
const roomMarkClass = css({ display: "inline-flex", alignItems: "center", justifyContent: "center", width: "36px", height: "36px", flex: "none", borderRadius: "pill", bg: "action.subtle", color: "action.text", font: "700 14px/1 {fonts.display}" });
const rowTextClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5" });
const rowTitleLineClass = css({ display: "flex", alignItems: "center", gap: "2", minWidth: 0 });
const rowTitleClass = css({ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: "15px", fontWeight: 600, color: "fg" });
const rowNameClass = css({ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: "15px" });
const rowNameUnreadClass = cx(rowNameClass, css({ fontWeight: 700 }));
const rowLineClass = css({ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: "13px", color: "fg.muted" });
const rowLiveLineClass = cx(rowLineClass, css({ color: "live.text" }));
const pillClass = css({ display: "inline-flex", alignItems: "center", gap: "1", height: "20px", px: "1.5", flex: "none", borderRadius: "pill", border: "1px solid {colors.border}", fontSize: "xs", color: "fg.muted", whiteSpace: "nowrap", "& svg": { width: "12px", height: "12px" } });
const livePillClass = cx(pillClass, css({ borderColor: "live", color: "live.text" }));
const openPillShapeClass = cx(openPillClass, css({ flex: "none", borderRadius: "pill" }));
const noteClass = css({ margin: 0, padding: "{spacing.6} {spacing.4}", borderRadius: "panel", border: "1px dashed {colors.border}", textStyle: "caption", color: "fg.muted", md: { gridColumn: "1 / -1" } });
const skeletonRowClass = css({ display: "flex", alignItems: "center", gap: "3", padding: "{spacing.2.5} {spacing.4}", borderRadius: "panel", border: "1px solid {colors.border.soft}", bg: "surface" });
const skeletonTextClass = css({ display: "flex", flex: 1, minWidth: 0, flexDirection: "column", gap: "1.5" });
const bellyClass = css({ display: "inline-flex", alignItems: "center", justifyContent: "center", width: "120px", height: "120px", borderRadius: "pill", bg: "belly", "& img": { width: "88px", height: "88px", animation: "none" } });
const srOnlyClass = css({ srOnly: true });

const HAPPENING_NOW_LIMIT = 3;

const now = ref<Date>(new Date());
let heroClockHandle: ReturnType<typeof setInterval> | null = null;

onMounted(() => {
  heroClockHandle = setInterval(() => { now.value = new Date(); }, 60_000);
});

onBeforeUnmount(() => {
  if (heroClockHandle) clearInterval(heroClockHandle);
});

const heroTimeOfDay = computed(() => heroTimeOfDayFor(now.value));
const selfName = computed(() => {
  const jid = props.selfFullJid ? barePeerJid(props.selfFullJid) : "";
  return jid ? jidLocalpart(jid) : "";
});
const heroGreeting = computed(() => heroGreetingFor(heroTimeOfDay.value, selfName.value));

const activeChannelJids = computed(() => props.activeChannelJids ?? new Set<string>());
const callParticipantCounts = computed(() => props.callParticipantCounts ?? {});
const callParticipants = computed(() => props.callParticipants ?? {});
const callMediaByRoom = computed(() => props.callMediaByRoom ?? {});
const dmCallActivities = computed(() => props.dmCallActivities ?? {});
const callState = useStore($callState);
// Whether a DM peer is in a call (their XEP-0108 overlay, ADR-010 Phase 3).
const { peerInCall } = useInCallOverlays();
const livePresence = useStore($livePresence);
const callParticipantAvatarJid = useCallParticipantAvatarJid();
const currentActiveDmPeer = computed(() => {
  const current = callState.value;
  return current.phase === "active" && current.kind === "dm"
    ? barePeerJid(current.peer).toLowerCase()
    : "";
});
const activeDmCallPeers = computed(() => {
  const peers = new Set(Object.values(dmCallActivities.value).map((activity) => barePeerJid(activity.peerJid).toLowerCase()).filter(Boolean));
  if (currentActiveDmPeer.value) peers.add(currentActiveDmPeer.value);
  return peers;
});
// Each card's dot, caption and label read the person's one presence.
const directMessages = computed(() =>
  (props.dmConversations ?? [])
    .filter((conversation) => !activeDmCallPeers.value.has(barePeerJid(conversation.peerJid).toLowerCase()))
    .map((conversation) => ({
      ...conversation,
      presenceShow: liveShow(livePresence.value, conversation.peerJid) ?? conversation.presenceShow,
    })),
);
const discoveredCallEntries = computed<CallActivityDockEntry[]>(() =>
  sortCallActivityDockEntries(
    buildCallActivityDockEntries({
      channels: props.channels,
      groupDms: props.groupDms ?? [],
      conversations: props.dmConversations ?? [],
      activeChannelId: null,
      activeChannelRoomJid: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: activeChannelJids.value,
      managedMucDomain: props.managedMucDomain ?? null,
      callParticipantCounts: callParticipantCounts.value,
      callParticipants: callParticipants.value,
      callMediaByRoom: callMediaByRoom.value,
      dmCallActivities: dmCallActivities.value,
    }),
    callState.value,
    props.selfFullJid ?? null,
  )
);
const currentCallFallbackEntry = computed<CallActivityDockEntry | null>(() =>
  buildCurrentCallFallbackEntry(),
);
const activeCallEntries = computed<CallActivityDockEntry[]>(() => {
  const entries = discoveredCallEntries.value;
  const currentEntry = currentCallFallbackEntry.value;
  if (!currentEntry) return entries;
  return [currentEntry, ...entries.filter((entry) => !isSameCallEntry(currentEntry)(entry))];
});
const activeCallSummaryCount = computed(() => activeCallEntries.value.length);
const activeCallStatusMessage = computed(() => {
  const count = activeCallEntries.value.length;
  if (count === 0) return "No active calls.";
  return `${count} active ${count === 1 ? "call" : "calls"}.`;
});
const activityByChannelId = computed(() =>
  new Map(props.channels.map((channel) => [
    channel.id,
    channelActivityState(channel, props.channelUnreadMap, activeChannelJids.value),
  ])),
);

/** People counted "in a huddle": every known participant of every live call. */
const inHuddleCount = computed(() => callEntriesInHuddleCount(activeCallEntries.value));

/** Roster contacts and DM peers, one per bare JID, split by the same
 * "around" rule as the people rail and the context column, so the hero
 * kicker, the summary line and the "Around" section below all agree. */
const knownPeople = computed(() => splitKnownPeople(props.contacts, props.dmConversations ?? [], livePresence.value));
const aroundPeople = computed(() => knownPeople.value.around);
const awayPeople = computed(() => knownPeople.value.awayAndOffline);

const heroKicker = computed(() => heroKickerFor(now.value, aroundPeople.value.length, inHuddleCount.value));

function selectCallEntry(entry: CallActivityDockEntry) {
  const selection = callActivityDockSelection(entry, callState.value, props.selfFullJid ?? null);
  switch (selection.kind) {
    case "dm-answer":
      emit("answerDm", selection.peerJid, selection.remoteFullJid, selection.sid, selection.media);
      return;
    case "channel-join":
      emit("joinChannelCall", selection.channelId, selection.roomJid, selection.media);
      return;
    case "group-dm-join":
      emit("joinGroupDmCall", selection.roomJid, selection.media);
      return;
    case "channel":
      if (selection.channelId) {
        emit("selectChannel", selection.channelId, selection.roomJid);
        return;
      }
      if (selection.roomJid) emit("selectChannelRoom", selection.roomJid);
      return;
    case "group-dm":
      emit("selectGroupDm", selection.roomJid);
      return;
    case "dm-reconnect":
      emit("reconnectDm", selection.peerJid, selection.media);
      return;
    case "dm-open":
      emit("selectContact", selection.peerJid);
      return;
  }
}

function canEndCallEntry(entry: CallActivityDockEntry): boolean {
  if (entry.kind === "channel") return canLeaveRetainedChannelCallEntry(entry);
  return canEndRecoveredDmCallActivity(entry, callState.value, props.selfFullJid ?? null);
}

function endCallEntry(entry: CallActivityDockEntry): void {
  if (!canEndCallEntry(entry)) return;
  if (entry.kind === "channel") {
    emit("leaveChannelCall", entry.roomJid);
    return;
  }
  emit("endDm", entry.peerJid, entry.sid);
}

function canLeaveRetainedChannelCallEntry(entry: Extract<CallActivityDockEntry, { kind: "channel" }>): boolean {
  return canLeaveRetainedChannelCallEntryFor(entry, callState.value);
}

function buildCurrentCallFallbackEntry(): CallActivityDockEntry | null {
  const current = callState.value;
  if (current.phase === "muc-pending" || (current.phase === "active" && current.kind === "muc")) {
    const roomJid = normalizeMucCallRoomJid(current.peer);
    if (!roomJid) return null;
    const channel = props.channels.find((candidate) =>
      normalizeMucCallRoomJid(candidate.jid ?? "") === roomJid
    );
    const participantLabels = participantLabelsForRoom(roomJid);
    const fallbackLabels = participantLabels.length > 0
      ? participantLabels
      : current.selfNick
        ? [current.selfNick]
        : [];
    return {
      kind: "channel",
      key: `current-channel:${roomJid}:${current.sid}`,
      channelId: channel?.id ?? null,
      roomJid,
      title: channel?.name ?? jidLocalpart(roomJid),
      participantCount: Math.max(fallbackLabels.length, 1),
      participantLabels: fallbackLabels,
      media: current.media,
      isKnownChannel: Boolean(channel),
      isActive: false,
    };
  }

  if (current.phase === "active" && current.kind === "dm") {
    const peerJid = barePeerJid(current.peer).toLowerCase();
    if (!peerJid) return null;
    const conversation = props.dmConversations?.find((candidate) =>
      barePeerJid(candidate.peerJid).toLowerCase() === peerJid
    );
    return {
      kind: "dm",
      key: `current-dm:${peerJid}:${current.sid}`,
      peerJid,
      sid: current.sid,
      remoteFullJid: current.peer,
      join: current.join,
      title: conversation?.peerUsername ?? jidLocalpart(peerJid),
      media: current.media,
      state: "accepted",
      direction: "unknown",
      updatedAt: "",
      isActive: false,
    };
  }

  return null;
}

function participantLabelsForRoom(roomJid: string): string[] {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized) return [];
  const labels = new Set<string>();
  for (const [candidate, nicks] of Object.entries(callParticipants.value)) {
    if (normalizeMucCallRoomJid(candidate) !== normalized) continue;
    for (const nick of nicks) {
      const label = nick.trim();
      if (label) labels.add(label);
    }
  }
  return [...labels];
}

function channelActivity(channel: ChannelSummary): ChannelActivityState {
  return activityByChannelId.value.get(channel.id)
    ?? { unread: 0, mentions: 0, threadUnread: 0, hasActivity: false };
}

function channelCallCount(channel: ChannelSummary): number {
  return callParticipantCountForChannel(
    channel,
    callParticipantCounts.value,
    activeChannelJids.value,
    props.managedMucDomain ?? null,
  );
}

function channelCallRoomJid(channel: ChannelSummary): string {
  return callRoomJidForChannel(
    channel,
    callParticipantCounts.value,
    activeChannelJids.value,
    props.managedMucDomain ?? null,
  );
}

function channelCallEntry(channel: ChannelSummary): Extract<CallActivityDockEntry, { kind: "channel" }> | null {
  const roomJid = channelCallRoomJid(channel);
  if (!roomJid) return null;
  return activeCallEntries.value.find((entry): entry is Extract<CallActivityDockEntry, { kind: "channel" }> =>
    entry.kind === "channel" &&
    (
      entry.channelId === channel.id ||
      normalizeMucCallRoomJid(entry.roomJid) === roomJid
    )
  ) ?? null;
}

function selectHomeChannel(channel: ChannelSummary): void {
  const callEntry = channelCallEntry(channel);
  if (callEntry) {
    selectCallEntry(callEntry);
    return;
  }
  emit("selectChannel", channel.id);
}

function dmCallActivityFor(peerJid: string) {
  return dmCallActivitiesForPeer(dmCallActivities.value, peerJid, props.selfFullJid ?? null)[0] ?? null;
}

function unreadBadgeCount(activity: { unread: number; threadUnread?: number }): number {
  return channelUnreadBadgeCount(activity);
}

function threadUnreadCount(activity: { threadUnread?: number }): number {
  return activity.threadUnread ?? 0;
}

function threadUnreadBadgeLabel(activity: { threadUnread?: number }): string {
  const count = threadUnreadCount(activity);
  return `${count} ${count === 1 ? "reply" : "replies"}`;
}

function hasChannelActivitySignal(activity: ChannelActivityState): boolean {
  return activity.unread > 0 || activity.mentions > 0 || threadUnreadCount(activity) > 0 || activity.hasActivity;
}

/** A channel where someone is expected: a mention, or a thread reply waiting. */
function channelNeedsSomeone(activity: ChannelActivityState): boolean {
  return activity.mentions > 0 || threadUnreadCount(activity) > 0;
}

function activityStamp(value?: number): string {
  if (!value) return "";
  const timestamp = value > 1_000_000_000_000 ? value : value * 1000;
  return formatTimelineStamp(new Date(timestamp).toISOString());
}

function channelKindLabel(channel: ChannelSummary): string {
  return isForumChannel(channel) ? "Forum" : "Room";
}

/** Honest card kicker: "Room · 4 unread", "Room · 2 mentions", "Room · Active". */
function channelCardKicker(channel: ChannelSummary): string {
  const activity = channelActivity(channel);
  const kind = channelKindLabel(channel);
  if (activity.mentions > 0) {
    return `${kind} · ${activity.mentions} ${activity.mentions === 1 ? "mention" : "mentions"}`;
  }
  const unread = unreadBadgeCount(activity);
  if (unread > 0) return `${kind} · ${unread} unread`;
  const replies = threadUnreadCount(activity);
  if (replies > 0) return `${kind} · ${threadUnreadBadgeLabel(activity)}`;
  if (activity.hasActivity) return `${kind} · Active`;
  return kind;
}

/** "2 mentions waiting", "3 thread replies waiting", or both. */
function needsSomeoneLabel(channel: ChannelSummary): string {
  const activity = channelActivity(channel);
  const parts: string[] = [];
  if (activity.mentions > 0) {
    parts.push(`${activity.mentions} ${activity.mentions === 1 ? "mention" : "mentions"}`);
  }
  const replies = threadUnreadCount(activity);
  if (replies > 0) {
    parts.push(`${replies} thread ${replies === 1 ? "reply" : "replies"}`);
  }
  return parts.length > 0 ? `${parts.join(" and ")} waiting` : "";
}

function channelInitial(channel: ChannelSummary): string {
  return (channel.name.trim()[0] ?? "#").toUpperCase();
}

function channelsByActivity(): ChannelSummary[] {
  return [...props.channels]
    .filter((channel) => hasChannelActivitySignal(channelActivity(channel)))
    .sort((a, b) => compareChannelActivityPriority(channelActivity(a), channelActivity(b)));
}

/** Channels with a mention or a thread reply waiting, busiest first. */
const needsSomeoneChannels = computed(() =>
  channelsByActivity().filter((channel) => channelNeedsSomeone(channelActivity(channel))),
);

/**
 * "Happening now": live calls first, then the busiest rooms by unread and
 * recency. Rooms already shown as a call card or under "Needs someone like
 * you" are not repeated. At most three cards.
 */
const happeningNowChannels = computed(() => {
  const remaining = HAPPENING_NOW_LIMIT - activeCallEntries.value.length;
  if (remaining <= 0) return [];
  return channelsByActivity()
    .filter((channel) => !channelNeedsSomeone(channelActivity(channel)))
    .filter((channel) => !channelCallEntry(channel))
    .slice(0, remaining);
});

const happeningNowCount = computed(() => activeCallEntries.value.length + happeningNowChannels.value.length);

/** XMPP show → the avatar's presence dot (xa reads as away). */
function avatarPresence(show?: "available" | "away" | "xa" | "dnd" | "offline"): OccupantPresence {
  if (show === "available") return "online";
  if (show === "away" || show === "xa") return "away";
  if (show === "dnd") return "dnd";
  return "offline";
}

/** Empty-Home board: with no rooms yet, Home's one action is Browse rooms. */
function browseRooms() {
  appController.value?.openRooms();
}

function dmDisplayName(conversation: { peerUsername?: string; peerJid: string }): string {
  return conversation.peerUsername || conversation.peerJid;
}

function dmSecondaryText(conversation: { peerUsername?: string; peerJid: string; lastMessageBody?: string }): string {
  const preview = dmPreviewText(conversation.lastMessageBody);
  if (preview && conversation.peerUsername && conversation.peerUsername !== conversation.peerJid) {
    return `${conversation.peerJid} · ${preview}`;
  }
  return preview || conversation.peerJid;
}

function dmCallLabel(peerJid: string): string {
  const activity = dmCallActivityFor(peerJid);
  if (!activity) return "";
  if (!hasKnownDmCallMedia(activity)) {
    if (activity.state === "accepted") return "Call live";
    if (activity.direction === "incoming") return "Incoming call";
    if (activity.direction === "outgoing") return "Calling";
    return "Call ringing";
  }
  const media = activity.media.video ? "Video" : "Voice";
  if (activity.state === "accepted") return `${media} call live`;
  if (activity.direction === "incoming") return `Incoming ${media.toLowerCase()} call`;
  if (activity.direction === "outgoing") return `Calling ${media.toLowerCase()} call`;
  return `${media} call ringing`;
}

function channelHomeAriaLabel(channel: ChannelSummary): string {
  const base = channelHomeLabel(channel, channelActivity(channel), activityStamp(channelActivity(channel).lastUpdated));
  const callEntry = channelCallEntry(channel);
  const count = callEntry?.participantCount ?? channelCallCount(channel);
  if (count <= 0) return base;
  const noun = count === 1 ? "person" : "people";
  const action = callEntry ? channelCallActionHint(callEntry) : "click to open call";
  return `${base}, active call with ${count} ${noun}, ${action}`;
}

function channelCallActionHint(entry: Extract<CallActivityDockEntry, { kind: "channel" }>): string {
  switch (callActivityDockAction(entry, callState.value, props.selfFullJid ?? null)) {
    case "join":
      return canLeaveRetainedChannelCallEntry(entry) ? "click to rejoin call" : "click to join call";
    case "return":
      return "click to return to call";
    default:
      return "click to open call";
  }
}

function dmHomeAriaLabel(conversation: {
  peerUsername?: string;
  peerJid: string;
  lastMessageBody?: string;
  lastMessageAt?: string;
  unreadCount: number;
  presenceShow?: "available" | "away" | "xa" | "dnd" | "offline";
}): string {
  const base = dmHomeLabel(
    conversation,
    conversation.lastMessageAt ? formatTimelineStamp(conversation.lastMessageAt) : "",
  );
  const call = dmCallLabel(conversation.peerJid);
  return call ? `${base}, ${call}` : base;
}

function callEntryLabel(entry: CallActivityDockEntry): string {
  return callEntryLabelFor(entry, callState.value, props.selfFullJid ?? null);
}

function callEntryEyebrow(entry: CallActivityDockEntry): string {
  return callEntryEyebrowFor(entry, callState.value, props.selfFullJid ?? null);
}

function callEntryDescription(entry: CallActivityDockEntry): string {
  return callEntryDescriptionFor(entry, callState.value, props.selfFullJid ?? null);
}

function callEntryDetail(entry: CallActivityDockEntry): string {
  return callEntryDetailFor(entry, callState.value, props.selfFullJid ?? null);
}

function callEntryTone(entry: CallActivityDockEntry) {
  return callEntryVisualTone(entry, callState.value, props.selfFullJid ?? null);
}

function callEntryIsLive(entry: CallActivityDockEntry): boolean {
  return callEntryTone(entry) === "success";
}

/** Recipe classes for a call card: the tone lives on the recipe, not on
 * a competing Tailwind utility (see `callEntryCardTone`). */
function callEntryCard(entry: CallActivityDockEntry): { root: string; kicker: string } {
  switch (callEntryCardTone(callEntryTone(entry))) {
    case "live":
      return liveCard;
    case "warning":
      return warningCard;
    case "active":
      return actionCard;
  }
}

function callEntryActionLabel(entry: CallActivityDockEntry): string {
  return callEntryActionLabelFor(entry, callState.value, props.selfFullJid ?? null);
}

const heroSummary = computed<HeroSummary>(() => {
  let totalUnread = 0;
  let totalMentions = 0;
  let totalThreadUnread = 0;
  for (const channel of props.channels) {
    const a = channelActivity(channel);
    totalUnread += unreadBadgeCount(a);
    totalMentions += a.mentions;
    totalThreadUnread += threadUnreadCount(a);
  }
  let dmUnread = 0;
  for (const dm of directMessages.value) {
    if (dm.unreadCount > 0) dmUnread += dm.unreadCount;
  }
  return {
    totalUnread,
    totalMentions,
    totalThreadUnread,
    dmUnread,
    activeCalls: activeCallSummaryCount.value,
    onlineFriends: aroundPeople.value.length,
    hasUnread: totalUnread + totalMentions + totalThreadUnread + dmUnread > 0,
  };
});

const heroSummaryParts = computed<HeroSummaryPart[]>(() => heroSummaryPartsFor(heroSummary.value));

const heroQuietMessage = computed(() => heroQuietMessageFor(heroTimeOfDay.value));

/** The mascot only shows when nothing is happening: quiet is an invitation. */
const isQuiet = computed(() => heroSummaryParts.value.length === 0 && activeCallEntries.value.length === 0);

// The call you are in already has its Happening-now card and the sidebar
// panel; the hero points somewhere new instead of a third Return, and so
// never at the room that call lives in either.
const heroPrimaryChannel = computed<ChannelSummary | undefined>(() => {
  if (heroPrimaryCall.value) return undefined;
  const own = currentCallFallbackEntry.value;
  const ownChannelId = own?.kind === "channel" ? own.channelId : null;
  return channelsByActivity().find((channel) => channel.id !== ownChannelId);
});

const heroPrimaryCall = computed<CallActivityDockEntry | null>(() =>
  activeCallEntries.value.find((entry) => callEntryActionLabel(entry) !== "Return") ?? null,
);

const heroCtaLabel = computed(() => {
  const call = heroPrimaryCall.value;
  if (call) return heroCallCtaLabel(call);
  const channel = heroPrimaryChannel.value;
  return channel ? `Jump into ${channel.name}` : "Browse rooms";
});

function onHeroCta() {
  const call = heroPrimaryCall.value;
  if (call) {
    selectCallEntry(call);
    return;
  }
  const channel = heroPrimaryChannel.value;
  if (channel) emit("selectChannel", channel.id);
}

function heroCallCtaLabel(entry: CallActivityDockEntry): string {
  const action = callEntryActionLabel(entry);
  if (action === "Open") {
    return entry.kind === "dm"
      ? `Open ${entry.title} conversation`
      : `Open ${entry.title} channel`;
  }
  return `${action} ${entry.title} call`;
}
</script>

<template>
  <SettingsMobileHeader title="Home" @open-nav="emit('openNav')" />
  <div :class="pc.scroll">
    <div :class="pageClass">
      <header :class="pc.head" :aria-label="`${heroGreeting} ${heroKicker}.`">
        <div :class="cx(pc.headText, heroTextClass)">
          <span :class="kickerClass">
            <span v-if="activeCallEntries.length > 0" :class="liveDotClass" aria-hidden="true"></span>{{ heroKicker }}
          </span>
          <h1 :class="greetingClass">{{ heroGreeting }}</h1>
          <p :class="summaryClass">
            <template v-if="heroSummaryParts.length > 0">
              <template v-for="(part, idx) in heroSummaryParts" :key="`${part.label}-${idx}`">
                <span v-if="idx > 0" :class="summarySepClass"> · </span>
                <strong>{{ part.count }}</strong> {{ part.label }}
              </template>
            </template>
            <template v-else>{{ heroQuietMessage }}</template>
          </p>
        </div>
        <button
          v-if="heroPrimaryCall || heroPrimaryChannel"
          type="button"
          :class="heroCtaClass"
          :aria-label="heroCtaLabel"
          @click="onHeroCta"
        >
          {{ heroCtaLabel }}
          <ArrowRight aria-hidden="true" />
        </button>
        <button v-else type="button" :class="heroCtaClass" @click="browseRooms">Browse rooms</button>
      </header>

      <div v-if="isQuiet && channels.length === 0" :class="pc.empty" role="status">
        <span :class="bellyClass" aria-hidden="true"><img src="/waddle-logo.svg" alt="" /></span>
        <h2 :class="pc.emptyTitle">Nothing here yet</h2>
        <p>Join a room and your mentions, events and people show up here.</p>
      </div>

      <section
        :class="srOnlyClass"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        {{ activeCallStatusMessage }}
      </section>

      <section
        v-if="happeningNowCount > 0"
        :class="sectionClass"
        aria-label="Happening now"
      >
        <div :class="sectionHeadClass">
          <h2 :class="liveHeadingClass">Happening now</h2>
          <p v-if="activeCallEntries.length > 0" :class="captionClass">
            {{ activeCallEntries.length }} {{ activeCallEntries.length === 1 ? "live conversation" : "live conversations" }}
          </p>
        </div>
        <div :class="cardGridClass">
          <article
            v-for="entry in activeCallEntries"
            :key="entry.key"
            :class="[callEntryCard(entry).root, cardMinClass]"
          >
            <span :class="[callEntryCard(entry).kicker, kickerRowClass]">
              <Video v-if="entry.kind === 'channel' && entry.media.video" aria-hidden="true" />
              <PhoneCall v-else-if="entry.kind === 'channel'" aria-hidden="true" />
              <Video v-else-if="entry.mediaKnown !== false && entry.media.video" aria-hidden="true" />
              <PhoneIncoming v-else-if="entry.state === 'ringing' && entry.direction === 'incoming'" aria-hidden="true" />
              <PhoneOutgoing v-else-if="entry.state === 'ringing' && entry.direction === 'outgoing'" aria-hidden="true" />
              <Phone v-else aria-hidden="true" />
              <span :class="ellipsisClass">{{ callEntryEyebrow(entry) }}</span>
            </span>
            <h3 :class="[liveCard.title, ellipsisClass]">{{ entry.title }}</h3>
            <p :class="liveCard.body">{{ callEntryDescription(entry) }}</p>
            <div
              v-if="entry.kind === 'channel' && callEntryParticipantPreview(entry)"
              :class="participantsClass"
            >
              <span :class="faceStackClass" aria-hidden="true">
                <span
                  v-for="label in callEntryVisibleParticipantLabels(entry)"
                  :key="`${entry.key}:${label}`"
                >
                  <UserAvatar :name="label" :jid="callParticipantAvatarJid(entry.roomJid, label)" size="xs" :in-call="false" />
                </span>
              </span>
              <span :class="[captionClass, ellipsisClass]">
                {{ callEntryParticipantPreview(entry) }}
              </span>
            </div>
            <div :class="footerWrapClass">
              <button
                type="button"
                :class="callEntryIsLive(entry) ? joinButtonClass : enterButtonClass"
                :aria-label="callEntryLabel(entry)"
                @click="selectCallEntry(entry)"
              >
                {{ callEntryActionLabel(entry) }}
                <ArrowRight aria-hidden="true" />
              </button>
              <span
                v-if="entry.kind === 'channel'"
                :class="countClass"
                :aria-label="`${entry.participantCount} in this call`"
              >{{ entry.participantCount }}</span>
              <span :class="metaClass">{{ callEntryDetail(entry) }}</span>
              <button
                v-if="canEndCallEntry(entry)"
                type="button"
                :class="leaveButtonClass"
                :aria-label="endCallEntryLabel(entry)"
                @click="endCallEntry(entry)"
              >
                <PhoneOff aria-hidden="true" />
                <span>{{ endCallEntryButtonText(entry) }}</span>
              </button>
            </div>
          </article>

          <article
            v-for="channel in happeningNowChannels"
            :key="channel.id"
            :class="[roomCard.root, cardMinClass]"
          >
            <span :class="channelActivity(channel).hasActivity ? liveCard.kicker : roomCard.kicker">
              {{ channelCardKicker(channel) }}
            </span>
            <h3 :class="roomTitleClass">
              <component :is="isForumChannel(channel) ? MessagesSquare : Hash" aria-hidden="true" />
              <span :class="ellipsisClass">{{ channel.name }}</span>
            </h3>
            <p v-if="channelActivityPreview(channelActivity(channel))" :class="[roomCard.body, clampClass]">
              {{ channelActivityPreview(channelActivity(channel)) }}
            </p>
            <div :class="roomCard.footer">
              <button
                type="button"
                :class="enterButtonClass"
                :aria-label="channelHomeAriaLabel(channel)"
                @click="selectHomeChannel(channel)"
              >
                Enter
                <ArrowRight aria-hidden="true" />
              </button>
              <span v-if="activityStamp(channelActivity(channel).lastUpdated)" :class="stampClass">
                {{ activityStamp(channelActivity(channel).lastUpdated) }}
              </span>
            </div>
          </article>
        </div>
      </section>

      <section
        v-if="needsSomeoneChannels.length > 0"
        :class="sectionClass"
        aria-label="Needs someone like you"
      >
        <h2 :class="headingClass">Needs someone like you</h2>
        <div :class="listClass">
          <button
            v-for="channel in needsSomeoneChannels"
            :key="channel.id"
            type="button"
            :class="mentionRowClass"
            :aria-label="channelHomeAriaLabel(channel)"
            @click="selectHomeChannel(channel)"
          >
            <span :class="roomMarkClass" aria-hidden="true">{{ channelInitial(channel) }}</span>
            <span :class="rowTextClass">
              <span :class="rowTitleLineClass">
                <span :class="rowTitleClass">{{ channel.name }}</span>
                <span
                  v-if="channelActivity(channel).mentions > 0"
                  :class="countClass"
                  aria-hidden="true"
                >@{{ channelActivity(channel).mentions }}</span>
                <span
                  v-if="threadUnreadCount(channelActivity(channel)) > 0"
                  :class="pillClass"
                  aria-hidden="true"
                >{{ threadUnreadBadgeLabel(channelActivity(channel)) }}</span>
              </span>
              <span
                v-if="channelActivityPreview(channelActivity(channel))"
                :class="rowLineClass"
              >{{ channelActivityPreview(channelActivity(channel)) }}</span>
              <span :class="rowLiveLineClass">{{ needsSomeoneLabel(channel) }}</span>
            </span>
            <span :class="openPillShapeClass" aria-hidden="true">Open</span>
          </button>
        </div>
      </section>

      <section :class="sectionClass" aria-label="Direct messages">
        <h2 :class="headingClass">Direct messages</h2>
        <div :class="twoColClass">
          <button
            v-for="conversation in directMessages"
            :key="conversation.peerJid"
            :class="rowClass"
            type="button"
            :aria-label="dmHomeAriaLabel(conversation)"
            @click="emit('selectContact', conversation.peerJid)"
          >
            <UserAvatar
              :name="conversation.peerUsername"
              :jid="conversationPeerAvatarJid(conversation.peerJid)"
              size="md"
              :presence="avatarPresence(conversation.presenceShow)"
              :in-call="peerInCall(conversation.peerJid)"
            />
            <span :class="rowTextClass">
              <span :class="conversation.unreadCount > 0 ? rowNameUnreadClass : rowNameClass">{{ dmDisplayName(conversation) }}</span>
              <span :class="rowLineClass">{{ dmSecondaryText(conversation) }}</span>
              <span :class="rowLineClass">{{ dmPresenceLabel(conversation.presenceShow) }}</span>
            </span>
            <span
              v-if="dmCallActivityFor(conversation.peerJid)"
              :class="livePillClass"
              aria-hidden="true"
            >
              <PhoneCall />
              <span>{{ dmCallActivityFor(conversation.peerJid)?.state === 'accepted' ? 'Live' : 'Ringing' }}</span>
            </span>
            <span v-if="conversation.lastMessageAt" :class="stampClass">
              {{ formatTimelineStamp(conversation.lastMessageAt) }}
            </span>
            <span
              v-if="conversation.unreadCount > 0"
              :class="countClass"
              aria-hidden="true"
            >{{ conversation.unreadCount }}</span>
          </button>
          <template v-if="isLoading && directMessages.length === 0">
            <div
              v-for="i in 2"
              :key="`dm-skel-${i}`"
              :class="skeletonRowClass"
              aria-hidden="true"
            >
              <Skeleton width="2rem" height="2rem" radius="9999px" />
              <div :class="skeletonTextClass">
                <Skeleton width="50%" height="0.7rem" />
                <Skeleton width="75%" height="0.6rem" />
              </div>
            </div>
          </template>
          <p v-else-if="directMessages.length === 0" :class="noteClass">
            No direct messages yet.
          </p>
        </div>
      </section>

      <section :class="sectionClass" aria-label="Around">
        <h2 :class="headingClass">
          Around<template v-if="aroundPeople.length > 0"> · {{ aroundPeople.length }}</template>
        </h2>
        <div :class="twoColClass">
          <button
            v-for="person in aroundPeople"
            :key="person.jid"
            :class="rowClass"
            type="button"
            :aria-label="`${person.name}, ${dmPresenceLabel(person.presenceShow)}, open direct message`"
            @click="emit('selectContact', person.jid)"
          >
            <UserAvatar :name="person.name" :jid="person.jid" size="md" :presence="person.presence" :in-call="peerInCall(person.jid)" />
            <span :class="rowTextClass">
              <span :class="rowNameClass">{{ person.name }}</span>
              <span :class="rowLineClass">{{ dmPresenceLabel(person.presenceShow) }}</span>
            </span>
          </button>
          <template v-if="isLoading && contacts.length === 0">
            <div
              v-for="i in 2"
              :key="`contact-skel-${i}`"
              :class="skeletonRowClass"
              aria-hidden="true"
            >
              <Skeleton width="2rem" height="2rem" radius="9999px" />
              <div :class="skeletonTextClass">
                <Skeleton width="45%" height="0.7rem" />
                <Skeleton width="30%" height="0.55rem" />
              </div>
            </div>
          </template>
          <p v-else-if="aroundPeople.length === 0 && awayPeople.length === 0" :class="noteClass">
            No contacts yet.
          </p>
          <p v-else-if="aroundPeople.length === 0" :class="noteClass">
            Nobody is around right now. Be the one who is here first.
          </p>
        </div>
        <template v-if="awayPeople.length > 0">
          <h3 :class="headingClass">Away and offline · {{ awayPeople.length }}</h3>
          <div :class="twoColClass">
            <button
              v-for="person in awayPeople"
              :key="person.jid"
              :class="quietRowClass"
              type="button"
              :aria-label="`${person.name}, ${dmPresenceLabel(person.presenceShow)}, open direct message`"
              @click="emit('selectContact', person.jid)"
            >
              <UserAvatar :name="person.name" :jid="person.jid" size="sm" :presence="person.presence" />
              <span :class="rowTextClass">
                <span :class="rowNameClass">{{ person.name }}</span>
                <span :class="rowLineClass">{{ dmPresenceLabel(person.presenceShow) }}</span>
              </span>
            </button>
          </div>
        </template>
      </section>
    </div>
  </div>
</template>
