<script setup lang="ts">
import { computed } from "vue";
import { Hash, MessageCircle, MessagesSquare, Users } from "lucide-vue-next";
import type { ChannelSummary } from "@/lib/chat-types";
import type { RosterContact } from "@/lib/xmpp/types";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import Skeleton from "@/components/ui/Skeleton.vue";
import { isForumChannel } from "@/lib/channel-types";
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
  type HomeActivitySummary,
} from "@/home/activity";

const props = defineProps<HomeDashboardProps>();

const emit = defineEmits<{
  selectChannel: [id: string];
  selectContact: [jid: string];
  openNav: [];
}>();

const groups = computed(() => groupChannelsBySpace(props.spaces, props.channels));
const visibleChannelGroups = computed(() => groups.value.filter((group) => group.channels.length > 0));
const activeChannelJids = computed(() => props.activeChannelJids ?? new Set<string>());
const directMessages = computed(() => props.dmConversations ?? []);
const channelsBySpace = computed(() =>
  new Map(groups.value.flatMap((group) => group.space ? [[group.space.id, group.channels.length] as const] : [])),
);
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
</script>

<template>
  <div class="chat-pane-scroll flex-1 min-h-0 bg-background px-[var(--chat-content-inline)] py-6">
    <div class="mx-auto grid w-full max-w-6xl gap-6">
      <header class="flex items-center justify-between gap-4">
        <div>
          <h1 class="type-display-title">Home</h1>
          <p class="type-caption text-muted-foreground">Activity across spaces, channels, direct messages, and contacts.</p>
        </div>
        <button
          class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground lg:hidden"
          type="button"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Hash class="h-4 w-4" />
        </button>
      </header>

      <section class="grid gap-3">
        <div class="flex items-center gap-2">
          <MessagesSquare class="h-4 w-4 text-primary" />
          <h2 class="type-pane-title">Spaces</h2>
        </div>
        <div class="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
          <button
            v-for="space in spaces"
            :key="space.id"
            class="chat-list-row flex min-h-16 items-center gap-3 rounded-lg border border-border bg-card px-4 py-3 text-left hover:bg-muted/60"
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
            class="rounded-lg border border-border bg-card p-3"
          >
            <h3 class="type-section-label px-1 pb-2 text-muted-foreground">{{ group.name }}</h3>
            <div class="grid gap-1">
              <button
                v-for="channel in group.channels"
                :key="channel.id"
                class="chat-list-row flex min-h-14 items-center gap-2 rounded-md px-3 py-2 text-left text-muted-foreground hover:bg-muted hover:text-foreground"
                :class="channelActivity(channel).mentions > 0
                  ? 'chat-list-row--unread chat-list-row--mention'
                  : unreadBadgeCount(channelActivity(channel)) > 0
                    ? 'chat-list-row--unread'
                    : ''"
                type="button"
                :aria-label="channelHomeLabel(channel, channelActivity(channel), activityStamp(channelActivity(channel).lastUpdated))"
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
              class="rounded-lg border border-border bg-card p-3"
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
            class="chat-list-row flex min-h-14 items-center gap-3 rounded-lg border border-border bg-card px-4 py-3 text-left hover:bg-muted/60"
            :class="conversation.unreadCount > 0 ? 'chat-list-row--unread' : ''"
            type="button"
            :aria-label="dmHomeLabel(conversation, conversation.lastMessageAt ? formatTimelineStamp(conversation.lastMessageAt) : '')"
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
            class="chat-list-row flex min-h-12 items-center gap-3 rounded-lg border border-border bg-card px-4 py-3 text-left hover:bg-muted/60"
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
