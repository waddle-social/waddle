<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useStore } from "@nanostores/vue";
import {
  ArrowRight,
  Hash,
  MessageCircle,
  MessagesSquare,
  Phone,
  PhoneCall,
  PhoneIncoming,
  PhoneOff,
  PhoneOutgoing,
  Users,
  Video,
} from "lucide-vue-next";
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
import { $callState } from "@/lib/calls/call-store";
import { useInCallOverlays } from "@/presence/in-call-overlay-store";
import {
  dmCallActivitiesForPeer,
  dmCallResumeBlockReason,
  hasKnownDmCallMedia,
} from "@/lib/calls/dm-call-activity";
import {
  callRoomJidForChannel,
  callParticipantCountForChannel,
  mucCallParticipantPreview,
} from "@/lib/calls/muc-call-indicators";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { readRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";
import type { CallMedia } from "@/lib/calls/types";
import { groupChannelsBySpace } from "@/lib/channel-grouping";
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
  spaceHomeLabel,
  summarizeHomeActivity,
  type ChannelActivityState,
  type HomeActivitySummary,
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

const now = ref<Date>(new Date());
let heroClockHandle: ReturnType<typeof setInterval> | null = null;

onMounted(() => {
  heroClockHandle = setInterval(() => { now.value = new Date(); }, 60_000);
});

onBeforeUnmount(() => {
  if (heroClockHandle) clearInterval(heroClockHandle);
});

type HeroTimeOfDay = "morning" | "day" | "evening" | "night";

const heroTimeOfDay = computed<HeroTimeOfDay>(() => {
  const h = now.value.getHours();
  if (h >= 5 && h < 11) return "morning";
  if (h >= 11 && h < 17) return "day";
  if (h >= 17 && h < 22) return "evening";
  return "night";
});

const heroGreeting = computed(() => {
  switch (heroTimeOfDay.value) {
    case "morning": return "Good morning.";
    case "day":     return "Good afternoon.";
    case "evening": return "Good evening.";
    case "night":   return "Late one tonight.";
  }
});

const heroEyebrow = computed(() =>
  new Intl.DateTimeFormat(undefined, {
    weekday: "long",
    month: "short",
    day: "numeric",
  }).format(now.value),
);

interface HeroSummary {
  totalUnread: number;
  totalMentions: number;
  totalThreadUnread: number;
  dmUnread: number;
  activeCalls: number;
  onlineFriends: number;
  hasUnread: boolean;
}

interface HeroSummaryPart {
  count: number;
  label: string;
}

const groups = computed(() => groupChannelsBySpace(props.spaces, props.channels));
const visibleChannelGroups = computed(() => groups.value.filter((group) => group.channels.length > 0));
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
const channelsBySpace = computed(() =>
  new Map(groups.value.flatMap((group) => group.space ? [[group.space.id, group.channels.length] as const] : [])),
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
const activityByGroupId = computed(() =>
  new Map(groups.value.map((group) => [
    group.id,
    summarizeHomeActivity(group.channels, props.channelUnreadMap, activeChannelJids.value),
  ])),
);

function contactLabel(contact: RosterContact): string {
  return contact.name || contact.username || contact.jid;
}

function selectSpaceChannel(spaceId: string) {
  const channelId = spaceTargetChannel(spaceId)?.id;
  if (channelId) emit("selectChannel", channelId);
}

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

function endCallEntryLabel(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") return `Leave ${entry.title} call`;
  const media = entry.mediaKnown !== false
    ? `${entry.media.video ? "video" : "voice"} call`
    : "call";
  return `End ${entry.title} ${media}`;
}

function endCallEntryButtonText(entry: CallActivityDockEntry): string {
  return entry.kind === "channel" ? "Leave call" : "End call";
}

function canLeaveRetainedChannelCallEntry(entry: Extract<CallActivityDockEntry, { kind: "channel" }>): boolean {
  const roomJid = normalizeMucCallRoomJid(entry.roomJid);
  if (!roomJid || currentMucCallRoomJid() === roomJid) return false;
  return readRoomHasActiveCall(roomJid).localResourceInCall;
}

function currentMucCallRoomJid(): string {
  const current = callState.value;
  if (current.phase !== "active" && current.phase !== "muc-pending") return "";
  if (current.kind !== "muc") return "";
  return normalizeMucCallRoomJid(current.peer);
}

function isCurrentCallEntry(entry: CallActivityDockEntry): boolean {
  const current = callState.value;
  if (current.phase === "muc-pending" || (current.phase === "active" && current.kind === "muc")) {
    if (entry.kind !== "channel") return false;
    return normalizeMucCallRoomJid(entry.roomJid) === normalizeMucCallRoomJid(current.peer);
  }
  if (current.phase !== "active" || current.kind !== "dm" || entry.kind !== "dm") return false;
  return entry.peerJid.toLowerCase() === barePeerJid(current.peer).toLowerCase() &&
    entry.sid === current.sid;
}

function isSameCallEntry(target: CallActivityDockEntry): (entry: CallActivityDockEntry) => boolean {
  return (entry) => {
    if (target.kind !== entry.kind) return false;
    if (target.kind === "channel" && entry.kind === "channel") {
      return normalizeMucCallRoomJid(target.roomJid) === normalizeMucCallRoomJid(entry.roomJid);
    }
    if (target.kind === "dm" && entry.kind === "dm") {
      return target.peerJid.toLowerCase() === entry.peerJid.toLowerCase() &&
        target.sid === entry.sid;
    }
    return false;
  };
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

function channelsForSpace(spaceId: string): ChannelSummary[] {
  return groups.value.find((group) => group.space?.id === spaceId)?.channels ?? [];
}

function spaceTargetChannel(spaceId: string): ChannelSummary | undefined {
  return [...channelsForSpace(spaceId)]
    .sort((a, b) => compareChannelActivityPriority(channelActivity(a), channelActivity(b)))
    [0];
}

function spaceHasChannels(spaceId: string): boolean {
  return (channelsBySpace.value.get(spaceId) ?? 0) > 0;
}

function channelActivity(channel: ChannelSummary) {
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

function groupActivity(groupId: string): HomeActivitySummary {
  return activityByGroupId.value.get(groupId)
    ?? { channelCount: 0, unread: 0, mentions: 0, threadUnread: 0, hasActivity: false };
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

function hasChannelActivitySignal(activity: { unread: number; mentions: number; threadUnread?: number; hasActivity: boolean }): boolean {
  return activity.unread > 0 || activity.mentions > 0 || threadUnreadCount(activity) > 0 || activity.hasActivity;
}

function showLiveActivityDot(activity: { unread: number; mentions: number; threadUnread?: number; hasActivity: boolean }): boolean {
  return activity.hasActivity && activity.mentions === 0 && unreadBadgeCount(activity) === 0 && threadUnreadCount(activity) === 0;
}

function activityStamp(value?: number): string {
  if (!value) return "";
  const timestamp = value > 1_000_000_000_000 ? value : value * 1000;
  return formatTimelineStamp(new Date(timestamp).toISOString());
}

function spaceDetail(spaceId: string): string {
  const channelCount = channelsBySpace.value.get(spaceId) ?? 0;
  const count = `${channelCount} ${channelCount === 1 ? "channel" : "channels"}`;
  const target = spaceTargetChannel(spaceId);
  const preview = channelActivityPreview(groupActivity(`space:${spaceId}`));
  return [
    count,
    ...(target ? [`Opens ${target.name}`] : []),
    ...(preview ? [preview] : []),
  ].join(" · ");
}

function dotClass(show?: "available" | "away" | "xa" | "dnd" | "offline"): string {
  if (show === "away") return "bg-warning/75";
  if (show === "dnd") return "bg-destructive/75";
  if (show === "xa") return "bg-warning/55";
  if (show === "available") return "bg-success/75";
  return "bg-muted-foreground/25";
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

function callEntryStatus(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const noun = entry.participantCount === 1 ? "person" : "people";
    return `${entry.participantCount} ${noun}`;
  }
  if (entry.state === "accepted") return "Live";
  if (entry.direction === "outgoing") return "Calling";
  return "Ringing";
}

function callEntryKindLabel(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const media = entry.media.video ? "Group video call" : "Group call";
    return entry.isKnownChannel ? media : `${media} syncing`;
  }
  if (entry.mediaKnown === false) return "Call";
  return entry.media.video ? "Video call" : "Voice call";
}

type CallEntryVisualTone = "primary" | "success" | "warning";

function callEntryAcceptedAvailability(entry: CallActivityDockEntry): {
  eyebrow: string;
  meta: string;
  description: string;
  tone: CallEntryVisualTone;
} | null {
  if (entry.kind !== "dm" || entry.state !== "accepted") return null;
  const mediaLabel = entry.mediaKnown === false ? "call" : `${entry.media.video ? "video" : "voice"} call`;
  const mediaTitle = `${mediaLabel.charAt(0).toUpperCase()}${mediaLabel.slice(1)}`;
  const reason = dmCallResumeBlockReason(entry, props.selfFullJid ?? null);
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
    if (canEndRecoveredDmCallActivity(entry, callState.value, props.selfFullJid ?? null)) {
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

function callEntryVisualTone(entry: CallActivityDockEntry): CallEntryVisualTone {
  if (entry.kind === "dm" && entry.state === "ringing" && entry.direction === "incoming") return "warning";
  if (entry.kind === "dm" && entry.state === "ringing" && entry.direction === "outgoing") return "primary";
  return callEntryAcceptedAvailability(entry)?.tone ?? "success";
}

function callEntryLabel(entry: CallActivityDockEntry): string {
  return [
    callEntryActionPhrase(entry),
    callEntryKindLabel(entry),
    callEntryStatus(entry),
    callEntryEyebrow(entry),
    callEntryDescription(entry),
    callEntryDetail(entry),
  ].filter(Boolean).join(", ");
}

function callEntryEyebrow(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") return entry.isActive ? "You're here" : "Live now";
  if (entry.isActive) return "You're here";
  const availability = callEntryAcceptedAvailability(entry);
  if (availability) return availability.eyebrow;
  if (entry.state === "accepted") return "Live now";
  if (entry.direction === "incoming") return "Incoming call";
  if (entry.direction === "outgoing") return "Calling";
  return "Ringing";
}

function callEntryDescription(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const noun = entry.participantCount === 1 ? "person" : "people";
    const location = entry.isKnownChannel ? "this channel" : "the channel";
    const preview = callEntryParticipantPreview(entry);
    if (entry.media.video) {
      if (preview) return `${entry.participantCount} ${noun} connected to the video call in ${location}: ${preview}.`;
      return `${entry.participantCount} ${noun} connected to the video call in ${location}.`;
    }
    if (preview) return `${entry.participantCount} ${noun} connected in ${location}: ${preview}.`;
    return `${entry.participantCount} ${noun} connected in ${location}.`;
  }

  const availability = callEntryAcceptedAvailability(entry);
  if (availability) return availability.description;

  if (entry.mediaKnown === false) {
    if (entry.state === "accepted") return "Call details are still syncing.";
    if (entry.direction === "incoming") return "Incoming call details are still syncing.";
    if (entry.direction === "outgoing") return "Outgoing call details are still syncing.";
    return "Call details are still syncing.";
  }

  const media = entry.media.video ? "video" : "voice";
  if (entry.state === "accepted") return `The ${media} call is still live.`;
  if (entry.direction === "incoming") return `Incoming ${media} call from this direct message.`;
  if (entry.direction === "outgoing") return `Outgoing ${media} call is still ringing.`;
  return `${media.charAt(0).toUpperCase()}${media.slice(1)} call is ringing.`;
}

function callEntryDetail(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") return callEntryKindLabel(entry);
  const stamp = entry.updatedAt ? formatTimelineStamp(entry.updatedAt) : "";
  return [
    callEntryAcceptedAvailability(entry)?.meta ?? callEntryKindLabel(entry),
    ...(stamp ? [`Updated ${stamp}`] : []),
  ].join(" · ");
}

function callEntryParticipantPreview(entry: CallActivityDockEntry): string {
  if (entry.kind !== "channel") return "";
  return mucCallParticipantPreview(entry.participantLabels);
}

function callEntryVisibleParticipantLabels(entry: CallActivityDockEntry): string[] {
  if (entry.kind !== "channel") return [];
  return entry.participantLabels.slice(0, 3);
}

function callEntryParticipantInitial(label: string): string {
  return label.trim().charAt(0).toUpperCase() || "?";
}

function callEntryToneClass(entry: CallActivityDockEntry): string {
  switch (callEntryVisualTone(entry)) {
    case "warning":
      return "border-warning/25 bg-warning/10 hover:bg-warning/15";
    case "primary":
      return "border-primary/25 bg-primary/8 hover:bg-primary/12";
    case "success":
      return "border-success/20 bg-success/10 hover:bg-success/15";
  }
}

function callEntryAccentClass(entry: CallActivityDockEntry): string {
  switch (callEntryVisualTone(entry)) {
    case "warning":
      return "text-warning-foreground";
    case "primary":
      return "text-primary";
    case "success":
      return "text-success-foreground";
  }
}

function callEntryIconClass(entry: CallActivityDockEntry): string {
  switch (callEntryVisualTone(entry)) {
    case "warning":
      return "border-warning/25 bg-background/90 text-warning-foreground";
    case "primary":
      return "border-primary/25 bg-background/90 text-primary";
    case "success":
      return "border-success/25 bg-background/90 text-success-foreground";
  }
}

function callEntryDotClass(entry: CallActivityDockEntry): string {
  switch (callEntryVisualTone(entry)) {
    case "warning":
      return "bg-warning shadow-[0_0_6px_var(--warning)]";
    case "primary":
      return "bg-primary shadow-[0_0_6px_var(--primary)]";
    case "success":
      return "bg-success shadow-[0_0_6px_var(--success)]";
  }
}

function callEntryPillClass(entry: CallActivityDockEntry): string {
  switch (callEntryVisualTone(entry)) {
    case "warning":
      return "border-warning/25 bg-background/70 text-warning-foreground";
    case "primary":
      return "border-primary/25 bg-background/70 text-primary";
    case "success":
      return "border-success/25 bg-background/70 text-success-foreground";
  }
}

function callEntryActionLabel(entry: CallActivityDockEntry): string {
  switch (callActivityDockAction(entry, callState.value, props.selfFullJid ?? null)) {
    case "answer":
      return "Answer";
    case "join":
      return entry.kind === "channel" && canLeaveRetainedChannelCallEntry(entry) ? "Rejoin" : "Join";
    case "return":
      return "Return";
    case "reconnect":
      return "Reconnect";
    case "open":
      return "Open";
  }
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
  let onlineFriends = 0;
  for (const dm of directMessages.value) {
    if (dm.unreadCount > 0) dmUnread += dm.unreadCount;
    if (dm.presenceShow === "available") onlineFriends += 1;
  }
  for (const contact of props.contacts) {
    if (contact.presenceShow && contact.presenceShow !== "offline") {
      onlineFriends += 1;
    }
  }
  return {
    totalUnread,
    totalMentions,
    totalThreadUnread,
    dmUnread,
    activeCalls: activeCallSummaryCount.value,
    onlineFriends,
    hasUnread: totalUnread + totalMentions + totalThreadUnread + dmUnread > 0,
  };
});

const heroSummaryParts = computed<HeroSummaryPart[]>(() => {
  const s = heroSummary.value;
  const parts: HeroSummaryPart[] = [];
  if (s.totalMentions > 0) {
    parts.push({ count: s.totalMentions, label: s.totalMentions === 1 ? "mention" : "mentions" });
  }
  const unreadTotal = s.totalUnread + s.dmUnread;
  if (unreadTotal > 0) {
    parts.push({ count: unreadTotal, label: unreadTotal === 1 ? "unread message" : "unread messages" });
  }
  if (s.totalThreadUnread > 0) {
    parts.push({ count: s.totalThreadUnread, label: s.totalThreadUnread === 1 ? "thread reply" : "thread replies" });
  }
  if (s.activeCalls > 0) {
    parts.push({ count: s.activeCalls, label: s.activeCalls === 1 ? "active call" : "active calls" });
  }
  if (s.onlineFriends > 0) {
    parts.push({ count: s.onlineFriends, label: s.onlineFriends === 1 ? "friend online" : "friends online" });
  }
  return parts;
});

const heroQuietMessage = computed(() => {
  switch (heroTimeOfDay.value) {
    case "morning": return "Everything's quiet. A good moment to start something.";
    case "day":     return "All caught up. The room is yours.";
    case "evening": return "All caught up. Maybe say hi to someone.";
    case "night":   return "Quiet night. Sleep well — or send a long-form note.";
  }
});

const heroPrimaryChannel = computed<ChannelSummary | undefined>(() => {
  if (heroPrimaryCall.value) return undefined;
  let best: ChannelSummary | undefined;
  let bestActivity: ChannelActivityState | undefined;
  for (const channel of props.channels) {
    const a = channelActivity(channel);
    if (!hasChannelActivitySignal(a)) continue;
    if (!best || !bestActivity || compareChannelActivityPriority(a, bestActivity) < 0) {
      best = channel;
      bestActivity = a;
    }
  }
  return best;
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

function callEntryActionPhrase(entry: CallActivityDockEntry): string {
  const action = callEntryActionLabel(entry);
  if (action === "Open") {
    return entry.kind === "dm"
      ? `Open ${entry.title} conversation`
      : `Open ${entry.title} channel`;
  }
  return `${action} ${entry.title}`;
}
</script>

<template>
  <div class="chat-pane-scroll flex-1 min-h-0 bg-background px-[var(--chat-content-inline)] py-6">
    <div class="mx-auto grid w-full max-w-6xl gap-6">
      <section
        class="home-hero"
        :class="`home-hero--${heroTimeOfDay}`"
        :aria-label="`${heroGreeting} ${heroEyebrow}.`"
      >
        <div class="home-hero__body">
          <span class="home-hero__eyebrow">
            <span class="home-hero__pulse" aria-hidden="true"></span>
            {{ heroEyebrow }}
          </span>
          <h1 class="home-hero__greeting">{{ heroGreeting }}</h1>
          <p class="home-hero__summary">
            <template v-if="heroSummaryParts.length > 0">
              <template v-for="(part, idx) in heroSummaryParts" :key="`${part.label}-${idx}`">
                <span v-if="idx > 0" class="home-hero__separator"> · </span>
                <strong>{{ part.count }}</strong> {{ part.label }}
              </template>
            </template>
            <template v-else>{{ heroQuietMessage }}</template>
          </p>
          <button
            v-if="heroPrimaryCall || heroPrimaryChannel"
            type="button"
            class="home-hero__cta"
            :aria-label="heroCtaLabel"
            @click="onHeroCta"
          >
            {{ heroCtaLabel }}
            <ArrowRight class="home-hero__cta-arrow h-4 w-4" aria-hidden="true" />
          </button>
        </div>
        <div class="home-hero__mascot-wrap" aria-hidden="true">
          <span class="home-hero__mascot-halo"></span>
          <img class="home-hero__mascot" src="/waddle-logo.svg" alt="" />
        </div>
        <button
          class="home-hero__nav-button"
          type="button"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Hash class="h-4 w-4" />
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
        v-if="activeCallEntries.length > 0"
        class="grid gap-3"
        aria-label="Active calls"
      >
        <div class="flex items-end justify-between gap-3">
          <div class="min-w-0">
            <div class="flex items-center gap-2">
              <PhoneCall class="h-4 w-4 text-success-foreground" aria-hidden="true" />
              <h2 class="type-pane-title">Active calls</h2>
            </div>
            <p class="type-caption mt-0.5 text-muted-foreground">
              {{ activeCallEntries.length }} {{ activeCallEntries.length === 1 ? "live conversation" : "live conversations" }}
            </p>
          </div>
        </div>
        <div class="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
          <div
            v-for="entry in activeCallEntries"
            :key="entry.key"
            class="chat-list-row flex min-w-0 min-h-24 flex-col items-stretch justify-between gap-3 overflow-hidden rounded-lg border px-4 py-3 text-left text-foreground transition-colors"
            :class="callEntryToneClass(entry)"
          >
            <button
              class="flex min-w-0 flex-1 flex-col items-stretch justify-between gap-3 text-left"
              type="button"
              :aria-label="callEntryLabel(entry)"
              @click="selectCallEntry(entry)"
            >
              <span class="flex min-w-0 items-start gap-3">
                <span class="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg border shadow-sm" :class="callEntryIconClass(entry)">
                  <Video v-if="entry.kind === 'channel' && entry.media.video" class="h-4 w-4" aria-hidden="true" />
                  <Hash v-else-if="entry.kind === 'channel'" class="h-4 w-4" aria-hidden="true" />
                  <Video v-else-if="entry.mediaKnown !== false && entry.media.video" class="h-4 w-4" aria-hidden="true" />
                  <PhoneIncoming v-else-if="entry.state === 'ringing' && entry.direction === 'incoming'" class="h-4 w-4" aria-hidden="true" />
                  <PhoneOutgoing v-else-if="entry.state === 'ringing' && entry.direction === 'outgoing'" class="h-4 w-4" aria-hidden="true" />
                  <Phone v-else class="h-4 w-4" aria-hidden="true" />
                </span>
                <span class="min-w-0 flex-1">
                  <span class="type-meta flex items-center gap-1.5" :class="callEntryAccentClass(entry)">
                    <span class="h-1.5 w-1.5 rounded-full" :class="callEntryDotClass(entry)" aria-hidden="true" />
                    <span class="truncate">{{ callEntryEyebrow(entry) }}</span>
                  </span>
                  <span class="type-control type-strong mt-0.5 block truncate">{{ entry.title }}</span>
                  <span class="type-caption mt-0.5 block text-muted-foreground">
                    {{ callEntryDescription(entry) }}
                  </span>
                  <span
                    v-if="entry.kind === 'channel' && callEntryParticipantPreview(entry)"
                    class="mt-2 flex min-w-0 items-center gap-2 text-muted-foreground"
                    :title="`${callEntryParticipantPreview(entry)} in call`"
                  >
                    <span class="flex shrink-0 pl-0.5" aria-hidden="true">
                      <span
                        v-for="label in callEntryVisibleParticipantLabels(entry)"
                        :key="`${entry.key}:${label}`"
                        class="-ml-0.5 flex h-5 w-5 first:ml-0 items-center justify-center rounded-full border border-background bg-success/15 text-[0.625rem] font-bold leading-none text-success-foreground"
                      >
                        {{ callEntryParticipantInitial(label) }}
                      </span>
                    </span>
                    <span class="type-meta min-w-0 truncate">
                      {{ callEntryParticipantPreview(entry) }}
                    </span>
                  </span>
                </span>
              </span>
              <span class="flex min-w-0 items-center gap-2">
                <span class="type-meta min-w-0 flex-1 truncate text-muted-foreground">{{ callEntryDetail(entry) }}</span>
                <span
                  v-if="entry.kind === 'channel'"
                  class="type-count-badge inline-flex min-w-[18px] h-[18px] shrink-0 items-center justify-center rounded-full border px-1"
                  :class="callEntryPillClass(entry)"
                  aria-hidden="true"
                >
                  {{ entry.participantCount }}
                </span>
                <span
                  class="type-meta shrink-0 rounded-full border px-2 py-1"
                  :class="callEntryPillClass(entry)"
                  aria-hidden="true"
                >
                  {{ callEntryActionLabel(entry) }}
                </span>
                <ArrowRight class="h-4 w-4 shrink-0" :class="callEntryAccentClass(entry)" aria-hidden="true" />
              </span>
            </button>
            <button
              v-if="canEndCallEntry(entry)"
              type="button"
              class="type-meta inline-flex h-8 items-center justify-center gap-1 rounded-full border border-destructive/25 bg-background/70 px-3 text-destructive hover:bg-destructive/10"
              :title="endCallEntryLabel(entry)"
              :aria-label="endCallEntryLabel(entry)"
              @click="endCallEntry(entry)"
            >
              <PhoneOff class="h-3.5 w-3.5" aria-hidden="true" />
              <span>{{ endCallEntryButtonText(entry) }}</span>
            </button>
          </div>
        </div>
      </section>

      <section class="grid gap-3">
        <div class="flex items-center gap-2">
          <MessagesSquare class="h-4 w-4 text-primary" />
          <h2 class="type-pane-title">Spaces</h2>
        </div>
        <div class="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
          <button
            v-for="space in spaces"
            :key="space.id"
            class="chat-list-row flex min-w-0 min-h-16 items-center gap-3 overflow-hidden rounded-lg border border-border bg-card px-4 py-3 text-left hover:bg-muted/60"
            :class="[
              spaceHasChannels(space.id) ? '' : 'cursor-default hover:bg-card',
              groupActivity(`space:${space.id}`).mentions > 0
                ? 'chat-list-row--unread chat-list-row--mention'
                : unreadBadgeCount(groupActivity(`space:${space.id}`)) > 0
                  ? 'chat-list-row--unread'
                  : '',
            ]"
            type="button"
            :disabled="!spaceHasChannels(space.id)"
            :aria-label="spaceHomeLabel(space.name, groupActivity(`space:${space.id}`), spaceTargetChannel(space.id)?.name)"
            @click="selectSpaceChannel(space.id)"
          >
            <span class="flex h-9 w-9 items-center justify-center rounded-lg bg-primary/10 text-primary">
              {{ (space.name[0] ?? "S").toUpperCase() }}
            </span>
            <span class="min-w-0 flex-1">
              <span class="type-control block truncate text-foreground">{{ space.name }}</span>
              <span class="type-caption block truncate text-muted-foreground" :title="spaceDetail(space.id)">
                {{ spaceDetail(space.id) }}
              </span>
            </span>
            <span class="flex shrink-0 items-center gap-1">
              <span
                v-if="groupActivity(`space:${space.id}`).mentions > 0"
                class="chat-list-row--mention-badge type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
                aria-hidden="true"
              >@{{ groupActivity(`space:${space.id}`).mentions }}</span>
              <span
                v-if="unreadBadgeCount(groupActivity(`space:${space.id}`)) > 0"
                class="chat-list-row--unread-badge type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-primary text-primary-foreground"
                aria-hidden="true"
              >{{ unreadBadgeCount(groupActivity(`space:${space.id}`)) }}</span>
              <span
                v-if="threadUnreadCount(groupActivity(`space:${space.id}`)) > 0"
                class="type-count-badge inline-flex min-h-[18px] items-center justify-center whitespace-nowrap rounded-full border border-border bg-muted px-1.5 py-0.5 text-foreground"
                title="Unread thread replies"
                aria-hidden="true"
              >{{ threadUnreadBadgeLabel(groupActivity(`space:${space.id}`)) }}</span>
              <span
                v-if="showLiveActivityDot(groupActivity(`space:${space.id}`))"
                class="w-2 h-2 rounded-full bg-primary shadow-[0_0_6px_var(--glow-strong)]"
                title="Live activity"
                aria-hidden="true"
              />
            </span>
          </button>
          <template v-if="isLoading && spaces.length === 0">
            <div
              v-for="i in 3"
              :key="`space-skel-${i}`"
              class="flex min-h-16 items-center gap-3 rounded-lg border border-border bg-card px-4 py-3"
              aria-hidden="true"
            >
              <Skeleton width="2.25rem" height="2.25rem" radius="0.5rem" />
              <div class="flex min-w-0 flex-1 flex-col gap-1.5">
                <Skeleton width="55%" height="0.75rem" />
                <Skeleton width="80%" height="0.65rem" />
              </div>
            </div>
          </template>
          <div v-else-if="spaces.length === 0" class="type-caption rounded-lg border border-border px-4 py-6 text-muted-foreground">
            No spaces discovered.
          </div>
        </div>
      </section>

      <section class="grid gap-3">
        <div class="flex items-center gap-2">
          <Hash class="h-4 w-4 text-primary" />
          <h2 class="type-pane-title">Channels</h2>
        </div>
        <div class="grid gap-4 lg:grid-cols-2">
          <div
            v-for="group in visibleChannelGroups"
            :key="group.id"
            class="min-w-0 overflow-hidden rounded-lg border border-border bg-card p-3"
          >
            <h3 class="type-section-label px-1 pb-2 text-muted-foreground">{{ group.name }}</h3>
            <div class="grid gap-1">
              <button
                v-for="channel in group.channels"
                :key="channel.id"
                class="chat-list-row flex min-w-0 min-h-14 items-center gap-2 overflow-hidden rounded-md px-3 py-2 text-left text-muted-foreground hover:bg-muted hover:text-foreground"
                :class="channelActivity(channel).mentions > 0
                  ? 'chat-list-row--unread chat-list-row--mention'
                  : unreadBadgeCount(channelActivity(channel)) > 0
                    ? 'chat-list-row--unread'
                    : ''"
                type="button"
                :aria-label="channelHomeAriaLabel(channel)"
                @click="selectHomeChannel(channel)"
              >
                <component :is="isForumChannel(channel) ? MessagesSquare : Hash" class="h-3.5 w-3.5 text-primary/70" />
                <span class="min-w-0 flex-1">
                  <span
                    class="type-control block truncate"
                    :class="hasChannelActivitySignal(channelActivity(channel)) ? 'type-strong text-foreground' : ''"
                  >{{ channel.name }}</span>
                  <span
                    v-if="channelActivityPreview(channelActivity(channel))"
                    class="type-caption block truncate text-muted-foreground"
                    :title="channelActivityPreview(channelActivity(channel))"
                  >{{ channelActivityPreview(channelActivity(channel)) }}</span>
                </span>
                <span v-if="activityStamp(channelActivity(channel).lastUpdated)" class="type-meta type-numeric text-muted-foreground">
                  {{ activityStamp(channelActivity(channel).lastUpdated) }}
                </span>
                <span class="flex shrink-0 items-center gap-1">
                  <span
                    v-if="channelCallCount(channel) > 0"
                    class="type-meta inline-flex h-[18px] shrink-0 items-center gap-1 rounded-full border border-success/25 bg-success/10 px-1.5 text-success-foreground"
                    title="Active call"
                    aria-hidden="true"
                  >
                    <PhoneCall class="h-3 w-3" />
                    <span>{{ channelCallCount(channel) }}</span>
                  </span>
                  <span
                    v-if="channelActivity(channel).mentions > 0"
                    class="chat-list-row--mention-badge type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
                    aria-hidden="true"
                  >@{{ channelActivity(channel).mentions }}</span>
                  <span
                    v-if="unreadBadgeCount(channelActivity(channel)) > 0"
                    class="chat-list-row--unread-badge type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-primary text-primary-foreground"
                    aria-hidden="true"
                  >{{ unreadBadgeCount(channelActivity(channel)) }}</span>
                  <span
                    v-if="threadUnreadCount(channelActivity(channel)) > 0"
                    class="type-count-badge inline-flex min-h-[18px] items-center justify-center whitespace-nowrap rounded-full border border-border bg-muted px-1.5 py-0.5 text-foreground"
                    title="Unread thread replies"
                    aria-hidden="true"
                  >{{ threadUnreadBadgeLabel(channelActivity(channel)) }}</span>
                  <span
                    v-if="showLiveActivityDot(channelActivity(channel))"
                    class="w-2 h-2 rounded-full bg-primary shadow-[0_0_6px_var(--glow-strong)]"
                    title="Live activity"
                    aria-hidden="true"
                  />
                </span>
              </button>
            </div>
          </div>
          <template v-if="isLoading && visibleChannelGroups.length === 0">
            <div
              v-for="i in 2"
              :key="`channel-group-skel-${i}`"
              class="min-w-0 overflow-hidden rounded-lg border border-border bg-card p-3"
              aria-hidden="true"
            >
              <Skeleton width="35%" height="0.65rem" />
              <div class="mt-3 grid gap-2">
                <div
                  v-for="j in 3"
                  :key="`channel-skel-${i}-${j}`"
                  class="flex min-h-10 items-center gap-2 rounded-md px-3 py-2"
                >
                  <Skeleton width="0.875rem" height="0.875rem" radius="0.25rem" />
                  <div class="flex min-w-0 flex-1 flex-col gap-1">
                    <Skeleton width="45%" height="0.65rem" />
                    <Skeleton width="70%" height="0.55rem" />
                  </div>
                </div>
              </div>
            </div>
          </template>
          <div v-else-if="visibleChannelGroups.length === 0" class="type-caption rounded-lg border border-border px-4 py-6 text-muted-foreground">
            No channels discovered.
          </div>
        </div>
      </section>

      <section class="grid gap-3">
        <div class="flex items-center gap-2">
          <MessageCircle class="h-4 w-4 text-primary" />
          <h2 class="type-pane-title">Direct messages</h2>
        </div>
        <div class="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
          <button
            v-for="conversation in directMessages"
            :key="conversation.peerJid"
            class="chat-list-row flex min-w-0 min-h-14 items-center gap-3 overflow-hidden rounded-lg border border-border bg-card px-4 py-3 text-left hover:bg-muted/60"
            :class="conversation.unreadCount > 0 ? 'chat-list-row--unread' : ''"
            type="button"
            :aria-label="dmHomeAriaLabel(conversation)"
            @click="emit('selectContact', conversation.peerJid)"
          >
            <span class="relative">
              <AppAvatar :name="conversation.peerUsername" :src="conversation.peerAvatarUrl ?? null" size="sm" :in-call="peerInCall(conversation.peerJid)" />
              <span class="absolute -right-0.5 -bottom-0.5 w-2 h-2 rounded-full border border-background" :class="dotClass(conversation.presenceShow)" />
              <span class="sr-only">{{ dmPresenceLabel(conversation.presenceShow) }}</span>
            </span>
            <span class="min-w-0 flex-1">
              <span
                class="type-control block truncate text-foreground"
                :class="conversation.unreadCount > 0 ? 'type-strong' : ''"
              >{{ dmDisplayName(conversation) }}</span>
              <span class="type-caption block truncate text-muted-foreground" :title="dmSecondaryText(conversation)">
                {{ dmSecondaryText(conversation) }}
              </span>
              <span class="type-meta block text-muted-foreground">
                {{ dmPresenceLabel(conversation.presenceShow) }}
              </span>
            </span>
            <span
              v-if="dmCallActivityFor(conversation.peerJid)"
              class="type-meta inline-flex h-[18px] shrink-0 items-center gap-1 rounded-full border border-success/25 bg-success/10 px-1.5 text-success-foreground"
              :title="dmCallLabel(conversation.peerJid)"
              aria-hidden="true"
            >
              <PhoneCall class="h-3 w-3" />
              <span>{{ dmCallActivityFor(conversation.peerJid)?.state === 'accepted' ? 'Live' : 'Ringing' }}</span>
            </span>
            <span v-if="conversation.lastMessageAt" class="type-meta type-numeric text-muted-foreground">
              {{ formatTimelineStamp(conversation.lastMessageAt) }}
            </span>
            <span
              v-if="conversation.unreadCount > 0"
              class="chat-list-row--unread-badge type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-primary text-primary-foreground"
              aria-hidden="true"
            >{{ conversation.unreadCount }}</span>
          </button>
          <template v-if="isLoading && directMessages.length === 0">
            <div
              v-for="i in 3"
              :key="`dm-skel-${i}`"
              class="flex min-h-14 items-center gap-3 rounded-lg border border-border bg-card px-4 py-3"
              aria-hidden="true"
            >
              <Skeleton width="2rem" height="2rem" radius="9999px" />
              <div class="flex min-w-0 flex-1 flex-col gap-1.5">
                <Skeleton width="50%" height="0.7rem" />
                <Skeleton width="75%" height="0.6rem" />
              </div>
            </div>
          </template>
          <div v-else-if="directMessages.length === 0" class="type-caption rounded-lg border border-border px-4 py-6 text-muted-foreground">
            No direct messages yet.
          </div>
        </div>
      </section>

      <section class="grid gap-3">
        <div class="flex items-center gap-2">
          <Users class="h-4 w-4 text-primary" />
          <h2 class="type-pane-title">Members</h2>
        </div>
        <div class="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
          <button
            v-for="contact in contacts"
            :key="contact.jid"
            class="chat-list-row flex min-w-0 min-h-12 items-center gap-3 overflow-hidden rounded-lg border border-border bg-card px-4 py-3 text-left hover:bg-muted/60"
            type="button"
            @click="emit('selectContact', contact.jid)"
          >
            <span class="flex h-8 w-8 items-center justify-center rounded-lg bg-muted text-muted-foreground">
              <MessageCircle class="h-3.5 w-3.5" />
            </span>
            <span class="min-w-0 flex-1">
              <span class="type-control block truncate text-foreground">{{ contactLabel(contact) }}</span>
              <span class="type-caption block truncate text-muted-foreground" :title="contact.jid">{{ contact.jid }}</span>
            </span>
          </button>
          <template v-if="isLoading && contacts.length === 0">
            <div
              v-for="i in 4"
              :key="`contact-skel-${i}`"
              class="flex min-h-12 items-center gap-3 rounded-lg border border-border bg-card px-4 py-3"
              aria-hidden="true"
            >
              <Skeleton width="2rem" height="2rem" radius="0.5rem" />
              <div class="flex min-w-0 flex-1 flex-col gap-1.5">
                <Skeleton width="45%" height="0.7rem" />
                <Skeleton width="65%" height="0.55rem" />
              </div>
            </div>
          </template>
          <div v-else-if="contacts.length === 0" class="type-caption rounded-lg border border-border px-4 py-6 text-muted-foreground">
            No roster contacts yet.
          </div>
        </div>
      </section>
    </div>
  </div>
</template>
