<script setup lang="ts">
import { computed } from "vue";
import { Hash, MessagesSquare, Plus, Settings, Users, ChevronDown, MessageCircle } from "lucide-vue-next";
import { isForumChannel as detectForumChannel } from "@/lib/channel-types";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { ThreadInboxEntry } from "@/composables/useChannelUnread";
import { groupChannelsBySpace } from "@/lib/channel-grouping";

const props = defineProps<{
  waddle: SpaceSummary | null;
  spaces: SpaceSummary[];
  channels: ChannelSummary[];
  activeChannelId: string | null;
  canManageChannels: boolean;
  canManageCommunity: boolean;
  isLoading: boolean;
  memberCount: number;
  activeChannelJids: Set<string>;
  channelUnreadMap?: Record<string, { unread: number; mentions: number }>;
  threadEntriesFn?: (roomJid: string) => ThreadInboxEntry[];
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

function channelIcon(channel: ChannelSummary) {
  return detectForumChannel(channel) ? MessagesSquare : Hash;
}

function channelThreads(channelId: string): ThreadInboxEntry[] {
  if (!props.threadEntriesFn) return [];
  // Find the room JID from the active channel JIDs
  for (const jid of props.activeChannelJids) {
    if (jid.startsWith(channelId + "@") || jid.includes(channelId)) {
      return props.threadEntriesFn(jid);
    }
  }
  return [];
}

function truncateTitle(title: string | undefined, maxLen = 28): string {
  if (!title) return "Untitled thread";
  return title.length > maxLen ? title.slice(0, maxLen) + "…" : title;
}

const emit = defineEmits<{
  selectChannel: [id: string];
  selectThread: [channelId: string, threadId: string];
  createChannel: [];
  openSettings: [];
  openMembers: [];
}>();
</script>

<template>
  <div class="chat-sidebar-pane glass-panel">
    <!-- Waddle header -->
    <div class="chat-sidebar-header">
      <button
        class="flex items-center gap-1.5 min-w-0 hover:text-primary transition-colors duration-200 group"
        type="button"
      >
        <span class="type-pane-title truncate text-sidebar-foreground">{{ waddle?.name ?? "Channels" }}</span>
        <ChevronDown class="w-3 h-3 text-sidebar-muted flex-shrink-0 group-hover:text-primary transition-colors" />
      </button>
      <div class="flex gap-0.5 flex-shrink-0">
        <button
          v-if="canManageCommunity"
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
        <span class="type-section-label px-2 text-sidebar-muted">
          Channels
        </span>

        <div v-if="isLoading" class="type-caption text-center py-10 text-sidebar-muted">
          <div class="flex items-center justify-center gap-1">
            <span class="typing-dot" />
            <span class="typing-dot" />
            <span class="typing-dot" />
          </div>
        </div>

        <div v-else-if="channels.length === 0" class="type-caption text-center py-10 text-sidebar-muted">
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
            <span class="type-section-label px-2 pt-2 text-sidebar-muted">
              {{ group.name }}
            </span>
            <template v-for="{ channel, unread } in group.channels" :key="channel.id">
              <button
                class="chat-list-row w-full min-h-10 flex items-center gap-2.5 px-3 py-2 text-left group"
                :class="activeChannelId === channel.id
                  ? 'bg-sidebar-accent text-sidebar-foreground'
                  : 'text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'"
                :aria-current="activeChannelId === channel.id ? 'page' : undefined"
                type="button"
                @click="emit('selectChannel', channel.id)"
              >
                <component
                  :is="channelIcon(channel)"
                  class="w-3.5 h-3.5 flex-shrink-0 transition-colors duration-200"
                  :class="activeChannelId === channel.id ? 'text-primary' : 'opacity-40 group-hover:opacity-70'"
                />
                <span
                  class="type-control truncate flex-1"
                  :class="[
                    activeChannelId === channel.id ? 'type-emphasis' : '',
                    (unread.unread > 0 || hasActivity(channel.id)) && activeChannelId !== channel.id ? 'type-strong text-sidebar-foreground' : '',
                  ]"
                >{{ channel.name }}</span>
                <span
                  v-if="detectForumChannel(channel)"
                  class="type-badge rounded-full border border-primary/12 px-1.5 py-0.5 text-primary/70"
                >
                  Forum
                </span>
                <span
                  v-if="unread.mentions > 0 && activeChannelId !== channel.id"
                  class="type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
                  aria-hidden="true"
                >{{ unread.mentions }}</span>
                <span
                  v-else-if="unread.unread > 0 && activeChannelId !== channel.id"
                  class="type-count-badge inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full bg-primary text-primary-foreground"
                  aria-hidden="true"
                >{{ unread.unread }}</span>
                <span
                  v-else-if="hasActivity(channel.id) && activeChannelId !== channel.id"
                  class="w-2 h-2 bg-primary rounded-full flex-shrink-0 shadow-[0_0_6px_var(--glow-strong)]"
                  aria-hidden="true"
                />
              </button>

              <!-- Active threads for this channel -->
              <button
                v-for="thread in channelThreads(channel.id)"
                :key="thread.threadId"
                class="chat-channel-thread-row chat-list-row flex items-center gap-2 px-2.5 py-2 text-left text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground group"
                type="button"
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
          </section>
        </div>
      </div>
    </div>

    <!-- Members footer -->
    <div v-if="waddle" class="chat-sidebar-footer">
      <button
        class="chat-sidebar-footer-action chat-list-row w-full flex items-center gap-2.5 bg-sidebar-accent/40 px-3 py-0 text-sidebar-muted hover:bg-sidebar-accent/70 hover:text-sidebar-foreground text-left group"
        type="button"
        @click="emit('openMembers')"
      >
        <Users class="w-3.5 h-3.5 flex-shrink-0 opacity-40 group-hover:opacity-70 transition-opacity" />
        <span class="type-control flex-1">Members</span>
        <span class="type-meta type-numeric text-sidebar-muted">{{ memberCount }}</span>
      </button>
    </div>
  </div>
</template>
