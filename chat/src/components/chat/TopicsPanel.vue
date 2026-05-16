<script setup lang="ts">
import { computed, watch } from "vue";
import { Hash, MessagesSquare, Plus, Settings, Users, ChevronDown, ChevronRight, MessageCircle } from "lucide-vue-next";
import { isForumChannel as detectForumChannel } from "@/lib/channel-types";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { ChannelThreadInboxEntry } from "@/channels/inbox";
import { groupChannelsBySpace } from "@/lib/channel-grouping";
import type { MemberLoadState } from "@/waddles/directory";
import Skeleton from "@/components/ui/Skeleton.vue";

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
}>();

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

function groupUnreadSummary(group: (typeof channelGroups.value)[number]) {
  return group.channels.reduce(
    (summary, { channel, unread }) => ({
      unread: summary.unread + unread.unread,
      mentions: summary.mentions + unread.mentions,
      hasActivity: summary.hasActivity || hasActivity(channel.id),
    }),
    { unread: 0, mentions: 0, hasActivity: false },
  );
}

function groupToggleLabel(group: (typeof channelGroups.value)[number]) {
  const state = isGroupCollapsed(group.id) ? "collapsed" : "expanded";
  const channelCount = `${group.channels.length} ${group.channels.length === 1 ? "channel" : "channels"}`;
  const summary = groupUnreadSummary(group);
  const activity = activityLabel(summary);

  return `${group.name}, ${state}, ${channelCount}, ${activity}`;
}

function activityLabel(summary: { unread: number; mentions: number; hasActivity: boolean }) {
  const parts: string[] = [];
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
  return `${channel.name}, ${isActive ? "selected" : "not selected"}, ${activityLabel({
    unread: unread.unread,
    mentions: unread.mentions,
    hasActivity: hasActivity(channel.id),
  })}`;
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
  selectChannel: [id: string];
  selectThread: [channelId: string, threadId: string];
  createChannel: [];
  createChannelInSpace: [spaceId: string | null];
  openSettings: [];
  openMembers: [];
  updateCollapsedGroupIds: [groupIds: Set<string>];
}>();

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
  <div class="chat-sidebar-pane glass-panel">
    <!-- Waddle header -->
    <div class="chat-sidebar-header">
      <div class="min-w-0 flex-1">
        <span class="type-pane-title truncate text-sidebar-foreground">Spaces</span>
      </div>
      <div class="flex gap-0.5 flex-shrink-0">
        <button
          v-if="waddle && canManageCommunity"
          class="chat-icon-button chat-icon-button--sm hover:bg-sidebar-accent"
          title="Settings"
          aria-label="Waddle settings"
          type="button"
          @click="emit('openSettings')"
        >
          <Settings class="w-3.5 h-3.5 text-sidebar-muted hover:text-primary" />
        </button>
        <button
          v-if="canManageChannels"
          class="chat-icon-button chat-icon-button--sm hover:bg-sidebar-accent"
          title="New channel or space"
          aria-label="New channel or space"
          type="button"
          @click="emit('createChannel')"
        >
          <Plus class="w-3.5 h-3.5 text-sidebar-muted hover:text-primary" />
        </button>
      </div>
    </div>

    <!-- Channel list -->
    <div class="chat-pane-scroll chat-sidebar-scroll">
      <div class="chat-panel-stack">
        <div v-if="isLoading" class="chat-list-stack" aria-hidden="true">
          <section
            v-for="i in 2"
            :key="`topics-skel-group-${i}`"
            class="grid gap-1"
          >
            <div class="flex items-center gap-1.5 px-2 pt-2 pb-1">
              <Skeleton width="0.625rem" height="0.625rem" radius="0.125rem" />
              <Skeleton :width="i === 1 ? '40%' : '55%'" height="0.55rem" />
            </div>
            <div
              v-for="j in i === 1 ? 4 : 3"
              :key="`topics-skel-row-${i}-${j}`"
              class="flex w-full min-h-10 items-center gap-2.5 px-3 py-2"
            >
              <Skeleton width="0.875rem" height="0.875rem" radius="0.25rem" />
              <Skeleton :width="`${30 + ((j * 13) % 45)}%`" height="0.6rem" />
            </div>
          </section>
        </div>

        <div v-else-if="channels.length === 0 && spaces.length === 0" class="type-caption text-center py-10 text-sidebar-muted">
          <div class="flex flex-col items-center gap-3 px-4">
            <span>There are no channels or spaces configured yet.</span>
            <button
              v-if="canManageChannels"
              class="type-control inline-flex h-8 items-center gap-1.5 rounded-md border border-border px-3 text-sidebar-foreground hover:bg-sidebar-accent"
              type="button"
              @click="emit('createChannel')"
            >
              <Plus class="h-3.5 w-3.5" />
              New channel or space
            </button>
          </div>
        </div>

        <div v-else class="chat-list-stack">
          <section v-for="group in channelGroups" :key="group.id" class="grid gap-1">
            <div class="flex items-center gap-1 rounded-md hover:bg-sidebar-accent/35">
              <button
                class="type-section-label flex min-w-0 flex-1 items-center gap-1.5 px-2 pt-2 pb-1 text-left text-sidebar-muted hover:text-sidebar-foreground"
                type="button"
                :aria-expanded="!isGroupCollapsed(group.id)"
                :aria-label="groupToggleLabel(group)"
                @click="toggleGroup(group.id)"
              >
                <ChevronDown
                  class="h-3 w-3 flex-shrink-0 transition-transform"
                  :class="isGroupCollapsed(group.id) ? '-rotate-90' : ''"
                />
                <span class="flex-1 truncate">{{ group.name }}</span>
                <!-- Rolled-up activity badges on a collapsed group —
                     match the iter-54 .chat-badge-glow halo so they
                     pull the eye the same way the rail bell badge
                     and sidebar unread badges do. Activity dot
                     already had a glow; mention / unread counts now
                     get one too. -->
                <template v-if="isGroupCollapsed(group.id)">
                  <span
                    v-if="groupUnreadSummary(group).mentions > 0"
                    class="chat-badge-glow--mention type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
                    aria-hidden="true"
                  >{{ groupUnreadSummary(group).mentions }}</span>
                  <span
                    v-else-if="groupUnreadSummary(group).unread > 0"
                    class="chat-badge-glow--primary type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-primary text-primary-foreground"
                    aria-hidden="true"
                  >{{ groupUnreadSummary(group).unread }}</span>
                  <span
                    v-else-if="groupUnreadSummary(group).hasActivity"
                    class="w-2 h-2 rounded-full bg-primary shadow-[0_0_6px_var(--glow-strong)]"
                    aria-hidden="true"
                  />
                </template>
                <span class="type-meta type-numeric text-sidebar-muted">{{ group.channels.length }}</span>
              </button>
              <button
                v-if="canManageChannels"
                class="chat-icon-button chat-icon-button--sm mr-1 mt-1"
                type="button"
                :aria-label="group.space ? `Create channel in ${group.name}` : 'Create standalone channel'"
                @click="emit('createChannelInSpace', group.space?.id ?? null)"
              >
                <Plus class="h-3.5 w-3.5" />
              </button>
            </div>
            <template v-if="!isGroupCollapsed(group.id)">
              <div
                v-if="group.channels.length === 0"
                class="type-caption flex items-center justify-between gap-2 px-6 py-2 text-sidebar-muted"
              >
                <span>No channels yet.</span>
                <button
                  v-if="canManageChannels"
                  class="type-control rounded-md border border-border px-2 py-1 text-sidebar-foreground hover:bg-sidebar-accent"
                  type="button"
                  :aria-label="group.space ? `Create channel in ${group.name}` : 'Create standalone channel'"
                  @click="emit('createChannelInSpace', group.space?.id ?? null)"
                >
                  Create
                </button>
              </div>
              <template v-for="{ channel, unread } in group.channels" :key="channel.id">
              <button
                class="chat-list-row w-full min-h-10 flex items-center gap-2.5 px-3 py-2 text-left group"
                :class="[
                  isActiveChannelPage(channel)
                    ? 'chat-list-row--active bg-sidebar-accent text-sidebar-foreground'
                    : 'text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground',
                  !isActiveChannelPage(channel) && unread.mentions > 0
                    ? 'chat-list-row--unread chat-list-row--mention'
                    : !isActiveChannelPage(channel) && unread.unread > 0
                      ? 'chat-list-row--unread'
                      : '',
                ]"
                :aria-current="isActiveChannelPage(channel) ? 'page' : undefined"
                :aria-label="channelRowLabel(channel, unread, isActiveChannelPage(channel))"
                type="button"
                @click="emit('selectChannel', channel.id)"
              >
                <component
                  :is="channelIcon(channel)"
                  class="w-3.5 h-3.5 flex-shrink-0 transition-colors duration-200"
                  :class="isActiveChannelPage(channel) ? 'text-primary' : 'opacity-40 group-hover:opacity-70'"
                />
                <span
                  class="type-control truncate flex-1"
                  :class="[
                    isActiveChannelPage(channel) ? 'type-emphasis' : '',
                    (unread.unread > 0 || hasActivity(channel.id)) && !isActiveChannelPage(channel) ? 'type-strong text-sidebar-foreground' : '',
                  ]"
                >{{ channel.name }}</span>
                <span
                  v-if="detectForumChannel(channel)"
                  class="type-badge rounded-full border border-primary/12 px-1.5 py-0.5 text-primary/70"
                >
                  Forum
                </span>
                <span
                  v-if="unread.mentions > 0 && !isActiveChannelPage(channel)"
                  class="chat-list-row--mention-badge type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
                  aria-hidden="true"
                >{{ unread.mentions }}</span>
                <span
                  v-else-if="unread.unread > 0 && !isActiveChannelPage(channel)"
                  class="chat-list-row--unread-badge type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-primary text-primary-foreground"
                  aria-hidden="true"
                >{{ unread.unread }}</span>
                <span
                  v-else-if="hasActivity(channel.id) && !isActiveChannelPage(channel)"
                  class="w-2 h-2 bg-primary rounded-full flex-shrink-0 shadow-[0_0_6px_var(--glow-strong)]"
                  aria-hidden="true"
                />
              </button>

              <!-- Active threads for this channel -->
              <button
                v-for="thread in channelThreads(channel)"
                :key="thread.threadId"
                class="chat-channel-thread-row chat-list-row flex items-center gap-2 px-2.5 py-2 text-left text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground group"
                type="button"
                :aria-label="threadRowLabel(thread)"
                @click="emit('selectThread', channel.id, thread.threadId)"
              >
                <MessageCircle class="w-3 h-3 flex-shrink-0 opacity-40 group-hover:opacity-70" />
                <span
                  class="type-caption truncate flex-1"
                  :class="thread.unread > 0 ? 'type-strong text-sidebar-foreground' : ''"
                >{{ truncateTitle(thread.title) }}</span>
                <span
                  v-if="thread.replyCount > 0"
                  class="type-meta type-numeric text-sidebar-muted"
                >{{ thread.replyCount }}</span>
                <span
                  v-if="thread.unread > 0"
                  class="type-count-badge inline-flex min-w-[16px] h-[16px] px-0.5 items-center justify-center rounded-full bg-primary text-primary-foreground"
                  aria-hidden="true"
                >{{ thread.unread }}</span>
              </button>
              </template>
            </template>
          </section>
        </div>
      </div>
    </div>

    <!-- Members footer — promoted from a flat "Members N" row to a
         clearly affordant button: the trailing ChevronRight signals
         "this opens a panel" (matches the iconography of other
         disclosure actions in the app) and the row lifts on hover.
         The Users glyph wakes up alongside the chevron so the whole
         row reads as one armed control. -->
    <div v-if="waddle" class="chat-sidebar-footer">
      <button
        class="chat-sidebar-footer-action chat-list-row w-full flex items-center gap-2.5 bg-sidebar-accent/40 px-3 py-0 text-sidebar-muted hover:bg-sidebar-accent/70 hover:text-sidebar-foreground text-left group"
        type="button"
        @click="emit('openMembers')"
      >
        <Users class="w-3.5 h-3.5 flex-shrink-0 opacity-40 group-hover:opacity-70 transition-opacity" />
        <span class="type-control flex-1">Members</span>
        <span class="type-meta text-sidebar-muted">{{ memberCountLabel() }}</span>
        <ChevronRight class="w-3.5 h-3.5 flex-shrink-0 opacity-30 group-hover:opacity-70 group-hover:translate-x-0.5 transition-all" aria-hidden="true" />
      </button>
    </div>
  </div>
</template>
