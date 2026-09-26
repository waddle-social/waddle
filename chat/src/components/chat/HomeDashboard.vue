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
import { button, card, count, kicker } from "styled-system/recipes";
import type { ChannelSummary } from "@/lib/chat-types";
import type { RosterContact } from "@/lib/xmpp/types";
import AppAvatar from "@/components/ui/AppAvatar.vue";
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
  callEntryAccentClass as accentClassForTone,
  callEntryActionLabel as callEntryActionLabelFor,
  callEntryDescription as callEntryDescriptionFor,
  callEntryDetail as callEntryDetailFor,
  callEntryEyebrow as callEntryEyebrowFor,
  callEntryLabel as callEntryLabelFor,
  callEntryParticipantPreview,
  callEntryToneClass as toneClassForTone,
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
import {
  dmCallActivitiesForPeer,
  hasKnownDmCallMedia,
} from "@/lib/calls/dm-call-activity";
import {
  callRoomJidForChannel,
  callParticipantCountForChannel,
} from "@/lib/calls/muc-call-indicators";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
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
const liveCard = card({ tone: "live" });
const roomCard = card({ tone: "quiet" });
const kickerClass = kicker();
const liveKickerClass = kicker({ tone: "live" });
const countClass = count();
const joinButtonClass = button({ variant: "live", size: "sm" });
const enterButtonClass = button({ variant: "quiet", size: "sm" });
const openPillClass = button({ variant: "secondary", size: "sm" });
const leaveButtonClass = button({ variant: "danger", size: "sm" });
const heroCtaClass = button({ variant: "primary", size: "md" });

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
const directMessages = computed(() =>
  (props.dmConversations ?? []).filter((conversation) =>
    !activeDmCallPeers.value.has(barePeerJid(conversation.peerJid).toLowerCase())
  ),
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
const inHuddleCount = computed(() =>
  activeCallEntries.value.reduce((total, entry) =>
    total + (entry.kind === "channel" ? entry.participantCount : 1), 0),
);

/** Distinct people who are around: online roster contacts plus online DM peers. */
const aroundJids = computed(() => {
  const jids = new Set<string>();
  for (const contact of props.contacts) {
    if (contact.presenceShow && contact.presenceShow !== "offline") {
      jids.add(barePeerJid(contact.jid).toLowerCase());
    }
  }
  for (const dm of directMessages.value) {
    if (dm.presenceShow === "available") jids.add(barePeerJid(dm.peerJid).toLowerCase());
  }
  return jids;
});

const heroKicker = computed(() => heroKickerFor(now.value, aroundJids.value.size, inHuddleCount.value));

function contactLabel(contact: RosterContact): string {
  return contact.name || contact.username || contact.jid;
}

// Same rule as the people rail: available (and chat) or do-not-disturb
// count as around; away and extended-away sit with offline.
function contactIsAround(contact: RosterContact): boolean {
  return contact.presenceShow === "available" || contact.presenceShow === "dnd";
}

const aroundContacts = computed(() => props.contacts.filter(contactIsAround));
const awayContacts = computed(() => props.contacts.filter((contact) => !contactIsAround(contact)));

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

function dotClass(show?: "available" | "away" | "xa" | "dnd" | "offline"): string {
  if (show === "away") return "bg-warning";
  if (show === "dnd") return "bg-destructive";
  if (show === "xa") return "bg-warning/70";
  if (show === "available") return "bg-success";
  return "bg-transparent border border-muted-foreground/60";
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

function callEntryToneClass(entry: CallActivityDockEntry): string {
  return toneClassForTone(callEntryTone(entry));
}

function callEntryAccentClass(entry: CallActivityDockEntry): string {
  return accentClassForTone(callEntryTone(entry));
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
    onlineFriends: aroundJids.value.size,
    hasUnread: totalUnread + totalMentions + totalThreadUnread + dmUnread > 0,
  };
});

const heroSummaryParts = computed<HeroSummaryPart[]>(() => heroSummaryPartsFor(heroSummary.value));

const heroQuietMessage = computed(() => heroQuietMessageFor(heroTimeOfDay.value));

/** The mascot only shows when nothing is happening: quiet is an invitation. */
const isQuiet = computed(() => heroSummaryParts.value.length === 0 && activeCallEntries.value.length === 0);

const heroPrimaryChannel = computed<ChannelSummary | undefined>(() => {
  if (heroPrimaryCall.value) return undefined;
  return channelsByActivity()[0];
});

const heroPrimaryCall = computed<CallActivityDockEntry | null>(() =>
  activeCallEntries.value[0] ?? null,
);

const heroCtaLabel = computed(() => {
  const call = heroPrimaryCall.value;
  if (call) return heroCallCtaLabel(call);
  const channel = heroPrimaryChannel.value;
  return channel ? `Jump into ${channel.name}` : "Browse channels";
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
  if (action === "Return") return `Return to ${entry.title} call`;
  if (action === "Open") {
    return entry.kind === "dm"
      ? `Open ${entry.title} conversation`
      : `Open ${entry.title} channel`;
  }
  return `${action} ${entry.title} call`;
}
</script>

<template>
  <div class="chat-pane-scroll flex-1 min-h-0 bg-background px-[var(--chat-content-inline)] py-6">
    <div class="mx-auto grid w-full max-w-5xl gap-8">
      <section
        class="relative grid gap-5 overflow-hidden rounded-2xl border border-border bg-card p-6 md:grid-cols-[minmax(0,1fr)_auto] md:items-center md:p-8"
        :aria-label="`${heroGreeting} ${heroKicker}.`"
      >
        <div class="flex min-w-0 flex-col gap-3">
          <span :class="[kickerClass, 'inline-flex items-center gap-2']">
            <span
              v-if="activeCallEntries.length > 0"
              class="h-2 w-2 rounded-full bg-live shadow-[0_0_8px_var(--glow-live)]"
              aria-hidden="true"
            ></span>
            {{ heroKicker }}
          </span>
          <h1 class="font-display text-[38px] font-bold leading-[1.05] tracking-[-0.03em] text-foreground">{{ heroGreeting }}</h1>
          <p class="max-w-[52ch] text-[15px] leading-relaxed text-muted-foreground [&>strong]:font-semibold [&>strong]:tabular-nums [&>strong]:text-foreground">
            <template v-if="heroSummaryParts.length > 0">
              <template v-for="(part, idx) in heroSummaryParts" :key="`${part.label}-${idx}`">
                <span v-if="idx > 0" class="text-muted-foreground/60"> · </span>
                <strong>{{ part.count }}</strong> {{ part.label }}
              </template>
            </template>
            <template v-else>{{ heroQuietMessage }}</template>
          </p>
          <button
            v-if="heroPrimaryCall || heroPrimaryChannel"
            type="button"
            :class="[heroCtaClass, 'mt-2 self-start']"
            :aria-label="heroCtaLabel"
            @click="onHeroCta"
          >
            {{ heroCtaLabel }}
            <ArrowRight class="h-4 w-4" aria-hidden="true" />
          </button>
        </div>
        <div v-if="isQuiet" class="flex h-20 w-20 items-center justify-center justify-self-end md:h-28 md:w-28" aria-hidden="true">
          <img class="h-full w-full" src="/waddle-logo.svg" alt="" />
        </div>
        <button
          class="absolute right-3 top-3 inline-flex h-8 w-8 items-center justify-center rounded-md border border-border bg-card text-muted-foreground hover:bg-muted hover:text-foreground md:hidden"
          type="button"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Hash class="h-4 w-4" aria-hidden="true" />
        </button>
      </section>

      <section
        class="sr-only"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        {{ activeCallStatusMessage }}
      </section>

      <section
        v-if="happeningNowCount > 0"
        class="grid gap-3"
        aria-label="Happening now"
      >
        <div class="flex items-baseline justify-between gap-3">
          <h2 :class="liveKickerClass">Happening now</h2>
          <p v-if="activeCallEntries.length > 0" class="type-caption text-muted-foreground">
            {{ activeCallEntries.length }} {{ activeCallEntries.length === 1 ? "live conversation" : "live conversations" }}
          </p>
        </div>
        <div class="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
          <article
            v-for="entry in activeCallEntries"
            :key="entry.key"
            :class="[callEntryIsLive(entry) ? liveCard.root : roomCard.root, 'min-w-0', callEntryToneClass(entry)]"
          >
            <span :class="[liveCard.kicker, 'flex items-center gap-1.5', callEntryAccentClass(entry)]">
              <Video v-if="entry.kind === 'channel' && entry.media.video" class="h-3.5 w-3.5" aria-hidden="true" />
              <PhoneCall v-else-if="entry.kind === 'channel'" class="h-3.5 w-3.5" aria-hidden="true" />
              <Video v-else-if="entry.mediaKnown !== false && entry.media.video" class="h-3.5 w-3.5" aria-hidden="true" />
              <PhoneIncoming v-else-if="entry.state === 'ringing' && entry.direction === 'incoming'" class="h-3.5 w-3.5" aria-hidden="true" />
              <PhoneOutgoing v-else-if="entry.state === 'ringing' && entry.direction === 'outgoing'" class="h-3.5 w-3.5" aria-hidden="true" />
              <Phone v-else class="h-3.5 w-3.5" aria-hidden="true" />
              <span class="truncate">{{ callEntryEyebrow(entry) }}</span>
            </span>
            <h3 :class="[liveCard.title, 'truncate']">{{ entry.title }}</h3>
            <p :class="liveCard.body">{{ callEntryDescription(entry) }}</p>
            <div
              v-if="entry.kind === 'channel' && callEntryParticipantPreview(entry)"
              class="flex min-w-0 items-center gap-2"
            >
              <span class="flex shrink-0 items-center" aria-hidden="true">
                <span
                  v-for="label in callEntryVisibleParticipantLabels(entry)"
                  :key="`${entry.key}:${label}`"
                  class="-ml-1.5 first:ml-0 rounded-full ring-2 ring-card"
                >
                  <AppAvatar :name="label" size="xs" />
                </span>
              </span>
              <span class="type-caption min-w-0 truncate text-muted-foreground">
                {{ callEntryParticipantPreview(entry) }}
              </span>
            </div>
            <div :class="[liveCard.footer, 'flex-wrap']">
              <button
                type="button"
                :class="callEntryIsLive(entry) ? joinButtonClass : enterButtonClass"
                :aria-label="callEntryLabel(entry)"
                @click="selectCallEntry(entry)"
              >
                {{ callEntryActionLabel(entry) }}
                <ArrowRight class="h-3.5 w-3.5" aria-hidden="true" />
              </button>
              <span
                v-if="entry.kind === 'channel'"
                :class="countClass"
                :aria-label="`${entry.participantCount} in this call`"
              >{{ entry.participantCount }}</span>
              <span class="type-meta min-w-0 flex-1 truncate text-muted-foreground">{{ callEntryDetail(entry) }}</span>
              <button
                v-if="canEndCallEntry(entry)"
                type="button"
                :class="leaveButtonClass"
                :aria-label="endCallEntryLabel(entry)"
                @click="endCallEntry(entry)"
              >
                <PhoneOff class="h-3.5 w-3.5" aria-hidden="true" />
                <span>{{ endCallEntryButtonText(entry) }}</span>
              </button>
            </div>
          </article>

          <article
            v-for="channel in happeningNowChannels"
            :key="channel.id"
            :class="[roomCard.root, 'min-w-0']"
          >
            <span :class="[roomCard.kicker, channelActivity(channel).hasActivity ? 'text-live-text' : '']">
              {{ channelCardKicker(channel) }}
            </span>
            <h3 :class="[roomCard.title, 'flex min-w-0 items-center gap-1.5']">
              <component :is="isForumChannel(channel) ? MessagesSquare : Hash" class="h-4 w-4 shrink-0 text-primary" aria-hidden="true" />
              <span class="truncate">{{ channel.name }}</span>
            </h3>
            <p v-if="channelActivityPreview(channelActivity(channel))" :class="[roomCard.body, 'line-clamp-2 break-words']">
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
                <ArrowRight class="h-3.5 w-3.5" aria-hidden="true" />
              </button>
              <span v-if="activityStamp(channelActivity(channel).lastUpdated)" class="type-meta ml-auto tabular-nums text-muted-foreground">
                {{ activityStamp(channelActivity(channel).lastUpdated) }}
              </span>
            </div>
          </article>
        </div>
      </section>

      <section
        v-if="needsSomeoneChannels.length > 0"
        class="grid gap-3"
        aria-label="Needs someone like you"
      >
        <h2 :class="kickerClass">Needs someone like you</h2>
        <div class="grid gap-2">
          <button
            v-for="channel in needsSomeoneChannels"
            :key="channel.id"
            type="button"
            class="chat-list-row chat-list-row--unread chat-list-row--mention flex min-w-0 items-center gap-3 rounded-xl border border-border bg-card px-4 py-3 text-left transition-colors hover:bg-muted"
            :aria-label="channelHomeAriaLabel(channel)"
            @click="selectHomeChannel(channel)"
          >
            <span class="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-primary/10 font-display text-sm font-bold text-primary" aria-hidden="true">
              {{ channelInitial(channel) }}
            </span>
            <span class="min-w-0 flex-1">
              <span class="flex min-w-0 items-center gap-2">
                <span class="type-control truncate font-semibold text-foreground">{{ channel.name }}</span>
                <span
                  v-if="channelActivity(channel).mentions > 0"
                  :class="countClass"
                  aria-hidden="true"
                >@{{ channelActivity(channel).mentions }}</span>
                <span
                  v-if="threadUnreadCount(channelActivity(channel)) > 0"
                  class="type-meta inline-flex h-[18px] items-center whitespace-nowrap rounded-full border border-border px-1.5 text-muted-foreground"
                  aria-hidden="true"
                >{{ threadUnreadBadgeLabel(channelActivity(channel)) }}</span>
              </span>
              <span
                v-if="channelActivityPreview(channelActivity(channel))"
                class="type-caption block truncate text-muted-foreground"
              >{{ channelActivityPreview(channelActivity(channel)) }}</span>
              <span class="type-meta block text-live-text">{{ needsSomeoneLabel(channel) }}</span>
            </span>
            <span :class="[openPillClass, 'shrink-0 rounded-full']" aria-hidden="true">Open</span>
          </button>
        </div>
      </section>

      <section class="grid gap-3" aria-label="Direct messages">
        <h2 :class="kickerClass">Direct messages</h2>
        <div class="grid gap-2 md:grid-cols-2">
          <button
            v-for="conversation in directMessages"
            :key="conversation.peerJid"
            class="chat-list-row flex min-w-0 items-center gap-3 overflow-hidden rounded-xl border border-border bg-card px-4 py-3 text-left transition-colors hover:bg-muted"
            :class="conversation.unreadCount > 0 ? 'chat-list-row--unread' : ''"
            type="button"
            :aria-label="dmHomeAriaLabel(conversation)"
            @click="emit('selectContact', conversation.peerJid)"
          >
            <span class="relative shrink-0">
              <AppAvatar :name="conversation.peerUsername" :src="conversation.peerAvatarUrl ?? null" size="md" :in-call="peerInCall(conversation.peerJid)" />
              <span class="absolute -right-0.5 -bottom-0.5 h-2.5 w-2.5 rounded-full border-2 border-card" :class="dotClass(conversation.presenceShow)" />
              <span class="sr-only">{{ dmPresenceLabel(conversation.presenceShow) }}</span>
            </span>
            <span class="min-w-0 flex-1">
              <span
                class="type-control block truncate text-foreground"
                :class="conversation.unreadCount > 0 ? 'font-semibold' : ''"
              >{{ dmDisplayName(conversation) }}</span>
              <span class="type-caption block truncate text-muted-foreground">
                {{ dmSecondaryText(conversation) }}
              </span>
              <span class="type-meta block text-muted-foreground">
                {{ dmPresenceLabel(conversation.presenceShow) }}
              </span>
            </span>
            <span
              v-if="dmCallActivityFor(conversation.peerJid)"
              class="type-meta inline-flex h-[18px] shrink-0 items-center gap-1 rounded-full border border-live-text px-1.5 text-live-text"
              aria-hidden="true"
            >
              <PhoneCall class="h-3 w-3" />
              <span>{{ dmCallActivityFor(conversation.peerJid)?.state === 'accepted' ? 'Live' : 'Ringing' }}</span>
            </span>
            <span v-if="conversation.lastMessageAt" class="type-meta shrink-0 tabular-nums text-muted-foreground">
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
              class="flex items-center gap-3 rounded-xl border border-border bg-card px-4 py-3"
              aria-hidden="true"
            >
              <Skeleton width="2rem" height="2rem" radius="9999px" />
              <div class="flex min-w-0 flex-1 flex-col gap-1.5">
                <Skeleton width="50%" height="0.7rem" />
                <Skeleton width="75%" height="0.6rem" />
              </div>
            </div>
          </template>
          <p v-else-if="directMessages.length === 0" class="type-caption rounded-xl border border-dashed border-border px-4 py-6 text-muted-foreground md:col-span-2">
            No direct messages yet.
          </p>
        </div>
      </section>

      <section class="grid gap-3" aria-label="Around">
        <h2 :class="kickerClass">
          Around<template v-if="aroundContacts.length > 0"> · {{ aroundContacts.length }}</template>
        </h2>
        <div class="grid gap-2 md:grid-cols-2">
          <button
            v-for="contact in aroundContacts"
            :key="contact.jid"
            class="flex min-w-0 items-center gap-3 rounded-xl border border-border bg-card px-4 py-2.5 text-left transition-colors hover:bg-muted"
            type="button"
            :aria-label="`${contactLabel(contact)}, ${dmPresenceLabel(contact.presenceShow)}, open direct message`"
            @click="emit('selectContact', contact.jid)"
          >
            <AppAvatar :name="contactLabel(contact)" size="md" :presence="contact.presenceShow" :in-call="peerInCall(contact.jid)" />
            <span class="min-w-0 flex-1">
              <span class="type-control block truncate text-foreground">{{ contactLabel(contact) }}</span>
              <span class="type-meta block truncate text-muted-foreground">{{ dmPresenceLabel(contact.presenceShow) }}</span>
            </span>
          </button>
          <template v-if="isLoading && contacts.length === 0">
            <div
              v-for="i in 2"
              :key="`contact-skel-${i}`"
              class="flex items-center gap-3 rounded-xl border border-border bg-card px-4 py-2.5"
              aria-hidden="true"
            >
              <Skeleton width="2rem" height="2rem" radius="9999px" />
              <div class="flex min-w-0 flex-1 flex-col gap-1.5">
                <Skeleton width="45%" height="0.7rem" />
                <Skeleton width="30%" height="0.55rem" />
              </div>
            </div>
          </template>
          <p v-else-if="contacts.length === 0" class="type-caption rounded-xl border border-dashed border-border px-4 py-6 text-muted-foreground md:col-span-2">
            No roster contacts yet.
          </p>
          <p v-else-if="aroundContacts.length === 0" class="type-caption rounded-xl border border-dashed border-border px-4 py-6 text-muted-foreground md:col-span-2">
            Nobody is around right now. Be the one who is here first.
          </p>
        </div>
        <template v-if="awayContacts.length > 0">
          <h3 :class="kickerClass">Away and offline · {{ awayContacts.length }}</h3>
          <div class="grid gap-2 md:grid-cols-2">
            <button
              v-for="contact in awayContacts"
              :key="contact.jid"
              class="flex min-w-0 items-center gap-3 rounded-xl border border-border/60 px-4 py-2.5 text-left text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              type="button"
              :aria-label="`${contactLabel(contact)}, ${dmPresenceLabel(contact.presenceShow)}, open direct message`"
              @click="emit('selectContact', contact.jid)"
            >
              <AppAvatar :name="contactLabel(contact)" size="sm" :presence="contact.presenceShow" />
              <span class="min-w-0 flex-1">
                <span class="type-control block truncate">{{ contactLabel(contact) }}</span>
                <span class="type-meta block truncate">{{ dmPresenceLabel(contact.presenceShow) }}</span>
              </span>
            </button>
          </div>
        </template>
      </section>
    </div>
  </div>
</template>
