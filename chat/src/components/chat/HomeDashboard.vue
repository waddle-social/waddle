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
  callActivityDockSelection,
  type CallActivityDockEntry,
} from "@/lib/calls/call-activity-dock";
import { $callState } from "@/lib/calls/call-store";
import { hasKnownDmCallMedia } from "@/lib/calls/dm-call-activity";
import { callParticipantCountForChannel } from "@/lib/calls/muc-call-indicators";
import { barePeerJid } from "@/lib/xmpp/jid";
import type { CallMedia } from "@/lib/calls/types";
import { groupChannelsBySpace } from "@/lib/channel-grouping";
import { formatTimelineStamp } from "@/channels/timeline";
import type { ChannelActivityState, HomeDashboardProps } from "@/home/dashboard-props";
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
  type HomeActivitySummary,
} from "@/home/activity";

const props = defineProps<HomeDashboardProps>();

const emit = defineEmits<{
  selectChannel: [id: string, roomJid?: string];
  selectChannelRoom: [roomJid: string];
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  selectContact: [jid: string];
  reconnectDm: [peerJid: string, media: CallMedia];
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
const dmCallActivities = computed(() => props.dmCallActivities ?? {});
const activeDmCallPeers = computed(() =>
  new Set(Object.values(dmCallActivities.value).map((activity) => barePeerJid(activity.peerJid).toLowerCase()).filter(Boolean)),
);
const directMessages = computed(() =>
  (props.dmConversations ?? []).filter((conversation) =>
    !activeDmCallPeers.value.has(barePeerJid(conversation.peerJid).toLowerCase())
  ),
);
const callState = useStore($callState);
const channelsBySpace = computed(() =>
  new Map(groups.value.flatMap((group) => group.space ? [[group.space.id, group.channels.length] as const] : [])),
);
const activeCallEntries = computed<CallActivityDockEntry[]>(() => buildCallActivityDockEntries({
  channels: props.channels,
  conversations: props.dmConversations ?? [],
  activeChannelId: null,
  activeChannelRoomJid: null,
  activePeerJid: null,
  sidebarMode: "channels",
  activeChannelJids: activeChannelJids.value,
  managedMucDomain: props.managedMucDomain ?? null,
  callParticipantCounts: callParticipantCounts.value,
  dmCallActivities: dmCallActivities.value,
}));
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
  const selection = callActivityDockSelection(entry, callState.value);
  switch (selection.kind) {
    case "dm-answer":
      emit("answerDm", selection.peerJid, selection.remoteFullJid, selection.sid, selection.media);
      return;
    case "channel-join":
      emit("joinChannelCall", selection.channelId, selection.roomJid, selection.media);
      return;
    case "channel":
      if (selection.channelId) {
        emit("selectChannel", selection.channelId, selection.roomJid);
        return;
      }
      if (selection.roomJid) emit("selectChannelRoom", selection.roomJid);
      return;
    case "dm-reconnect":
      emit("reconnectDm", selection.peerJid, selection.media);
      return;
    case "dm-open":
      emit("selectContact", selection.peerJid);
      return;
  }
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

function dmCallActivityFor(peerJid: string) {
  const normalized = barePeerJid(peerJid).toLowerCase();
  return normalized ? dmCallActivities.value[normalized] ?? null : null;
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
  const count = channelCallCount(channel);
  if (count <= 0) return base;
  const noun = count === 1 ? "person" : "people";
  return `${base}, active call with ${count} ${noun}`;
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
  if (entry.kind === "channel") return entry.isKnownChannel ? "Group call" : "Group call syncing";
  if (entry.mediaKnown === false) return "Call";
  return entry.media.video ? "Video call" : "Voice call";
}

function callEntryLabel(entry: CallActivityDockEntry): string {
  const action = callEntryActionLabel(entry);
  return `${action} ${entry.title}, ${callEntryKindLabel(entry)}, ${callEntryStatus(entry)}`;
}

function callEntryActionLabel(entry: CallActivityDockEntry): string {
  switch (callActivityDockAction(entry, callState.value)) {
    case "answer":
      return "Answer";
    case "join":
      return "Join";
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
    activeCalls: activeCallEntries.value.length,
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

const heroCtaLabel = computed(() => {
  const channel = heroPrimaryChannel.value;
  return channel ? `Jump into ${channel.name}` : "Browse channels";
});

function onHeroCta() {
  const channel = heroPrimaryChannel.value;
  if (channel) emit("selectChannel", channel.id);
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
            v-if="heroPrimaryChannel"
            type="button"
            class="home-hero__cta"
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
        v-if="activeCallEntries.length > 0"
        class="grid gap-3"
        aria-label="Active calls"
      >
        <div class="flex items-center gap-2">
          <PhoneCall class="h-4 w-4 text-success" />
          <h2 class="type-pane-title">Active calls</h2>
        </div>
        <div class="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
          <button
            v-for="entry in activeCallEntries"
            :key="entry.key"
            class="chat-list-row flex min-w-0 min-h-16 items-center gap-3 overflow-hidden rounded-lg border border-success/20 bg-success/10 px-4 py-3 text-left text-foreground hover:bg-success/20"
            type="button"
            :aria-label="callEntryLabel(entry)"
            @click="selectCallEntry(entry)"
          >
            <span class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-success/10 text-success">
              <Hash v-if="entry.kind === 'channel'" class="h-4 w-4" />
              <Video v-else-if="entry.mediaKnown !== false && entry.media.video" class="h-4 w-4" />
              <PhoneIncoming v-else-if="entry.state === 'ringing' && entry.direction === 'incoming'" class="h-4 w-4" />
              <PhoneOutgoing v-else-if="entry.state === 'ringing' && entry.direction === 'outgoing'" class="h-4 w-4" />
              <Phone v-else class="h-4 w-4" />
            </span>
            <span class="min-w-0 flex-1">
              <span class="type-control block truncate">{{ entry.title }}</span>
              <span class="type-caption block truncate text-muted-foreground">
                {{ callEntryKindLabel(entry) }} · {{ callEntryStatus(entry) }}
              </span>
            </span>
            <span
              v-if="entry.kind === 'channel'"
              class="type-count-badge inline-flex min-w-[18px] h-[18px] shrink-0 items-center justify-center rounded-full border border-success/25 bg-success/10 px-1 text-success"
              aria-hidden="true"
            >
              {{ entry.participantCount }}
            </span>
            <span
              class="type-meta shrink-0 rounded-md border border-success/25 bg-background/70 px-2 py-1 text-success"
              aria-hidden="true"
            >
              {{ callEntryActionLabel(entry) }}
            </span>
            <ArrowRight class="h-4 w-4 shrink-0 text-success" aria-hidden="true" />
          </button>
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
                @click="emit('selectChannel', channel.id)"
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
                    class="type-meta inline-flex h-[18px] shrink-0 items-center gap-1 rounded-full border border-success/25 bg-success/10 px-1.5 text-success"
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
              <AppAvatar :name="conversation.peerUsername" :src="conversation.peerAvatarUrl ?? null" size="sm" />
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
              class="type-meta inline-flex h-[18px] shrink-0 items-center gap-1 rounded-full border border-success/25 bg-success/10 px-1.5 text-success"
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
