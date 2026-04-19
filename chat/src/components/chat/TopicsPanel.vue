<script setup lang="ts">
import { computed } from "vue";
import { Hash, MessagesSquare, Plus, Settings, Users, ChevronDown, MessageCircle } from "lucide-vue-next";
import { isForumChannel as detectForumChannel } from "@/lib/channel-types";
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import type { ThreadInboxEntry } from "@/composables/useChannelUnread";

const props = defineProps<{
  waddle: WaddleSummary | null;
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

const channelsWithUnread = computed(() =>
  props.channels.map((channel) => ({
    channel,
    unread: props.channelUnreadMap?.[channel.id] ?? { unread: 0, mentions: 0 },
  }))
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
  <div class="w-[248px] border-r border-border glass-panel flex flex-col flex-shrink-0">
    <!-- Waddle header -->
    <div class="h-14 px-4 flex items-center justify-between flex-shrink-0 border-b border-border">
      <button class="flex items-center gap-1.5 min-w-0 hover:text-primary transition-colors duration-200 group">
        <span class="text-[14px] font-display font-bold tracking-tight truncate text-sidebar-foreground">{{ waddle?.name ?? "..." }}</span>
        <ChevronDown class="w-3 h-3 text-sidebar-muted flex-shrink-0 group-hover:text-primary transition-colors" />
      </button>
      <div class="flex gap-0.5 flex-shrink-0">
        <button
          v-if="canManageCommunity"
          class="h-7 w-7 flex items-center justify-center rounded-lg hover:bg-sidebar-accent transition-all duration-200"
          title="Settings"
          @click="emit('openSettings')"
        >
          <Settings class="w-3.5 h-3.5 text-sidebar-muted hover:text-primary" />
        </button>
        <button
          v-if="canManageChannels"
          class="h-7 w-7 flex items-center justify-center rounded-lg hover:bg-sidebar-accent transition-all duration-200"
          title="Add channel"
          @click="emit('createChannel')"
        >
          <Plus class="w-3.5 h-3.5 text-sidebar-muted hover:text-primary" />
        </button>
      </div>
    </div>

    <!-- Channel list -->
    <div class="flex-1 overflow-auto py-3 px-2">
      <div class="px-2.5 mb-2">
        <span class="text-[10px] font-semibold uppercase tracking-[0.12em] text-sidebar-muted">
          Channels
        </span>
      </div>

      <div v-if="isLoading" class="text-center py-8 text-[13px] text-sidebar-muted">
        <div class="flex items-center justify-center gap-1">
          <span class="typing-dot" />
          <span class="typing-dot" />
          <span class="typing-dot" />
        </div>
      </div>

      <div v-else-if="channels.length === 0" class="text-center py-8 text-[13px] text-sidebar-muted">
        No channels yet
      </div>

      <div v-else class="space-y-0.5">
        <template v-for="{ channel, unread } in channelsWithUnread" :key="channel.id">
          <button
            class="w-full flex items-center gap-2.5 px-2.5 py-2 rounded-lg transition-all duration-200 text-left group"
            :class="activeChannelId === channel.id
              ? 'bg-sidebar-accent text-sidebar-foreground'
              : 'text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'"
            @click="emit('selectChannel', channel.id)"
          >
            <component
              :is="channelIcon(channel)"
              class="w-3.5 h-3.5 flex-shrink-0 transition-colors duration-200"
              :class="activeChannelId === channel.id ? 'text-primary' : 'opacity-40 group-hover:opacity-70'"
            />
            <span
              class="text-[13px] truncate flex-1"
              :class="[
                activeChannelId === channel.id ? 'font-medium' : '',
                (unread.unread > 0 || hasActivity(channel.id)) && activeChannelId !== channel.id ? 'font-semibold text-sidebar-foreground' : '',
              ]"
            >{{ channel.name }}</span>
            <span
              v-if="detectForumChannel(channel)"
              class="rounded-full border border-primary/12 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-[0.12em] text-primary/70"
            >
              Forum
            </span>
            <span
              v-if="unread.mentions > 0 && activeChannelId !== channel.id"
              class="inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full text-[10px] font-semibold bg-destructive text-destructive-foreground"
              aria-hidden="true"
            >{{ unread.mentions }}</span>
            <span
              v-else-if="unread.unread > 0 && activeChannelId !== channel.id"
              class="inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full text-[10px] font-semibold bg-primary text-primary-foreground"
              aria-hidden="true"
            >{{ unread.unread }}</span>
            <span
              v-else-if="hasActivity(channel.id) && activeChannelId !== channel.id"
              class="w-2 h-2 bg-primary rounded-full flex-shrink-0 shadow-[0_0_6px_var(--glow-strong)]"
              aria-hidden="true"
            />
          </button>

          <!-- Active threads for this channel -->
          <div
            v-for="thread in channelThreads(channel.id)"
            :key="thread.threadId"
            class="ml-6 flex items-center gap-2 px-2.5 py-1.5 rounded-lg cursor-pointer transition-all duration-200 text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground group"
            @click="emit('selectThread', channel.id, thread.threadId)"
          >
            <MessageCircle class="w-3 h-3 flex-shrink-0 opacity-40 group-hover:opacity-70" />
            <span
              class="text-[12px] truncate flex-1"
              :class="thread.unread > 0 ? 'font-semibold text-sidebar-foreground' : ''"
            >{{ truncateTitle(thread.title) }}</span>
            <span
              v-if="thread.replyCount > 0"
              class="text-[10px] text-sidebar-muted tabular-nums"
            >{{ thread.replyCount }}</span>
            <span
              v-if="thread.unread > 0"
              class="inline-flex min-w-[16px] h-[16px] px-0.5 items-center justify-center rounded-full text-[9px] font-semibold bg-primary text-primary-foreground"
              aria-hidden="true"
            >{{ thread.unread }}</span>
          </div>
        </template>
      </div>
    </div>

    <!-- Members footer -->
    <div v-if="waddle" class="flex-shrink-0 px-2 py-2 border-t border-border">
      <button
        class="w-full flex items-center gap-2.5 px-2.5 py-2 rounded-lg text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground transition-all duration-200 text-left group"
        @click="emit('openMembers')"
      >
        <Users class="w-3.5 h-3.5 flex-shrink-0 opacity-40 group-hover:opacity-70 transition-opacity" />
        <span class="text-[13px] flex-1">Members</span>
        <span class="text-[11px] text-sidebar-muted font-mono tabular-nums">{{ memberCount }}</span>
      </button>
    </div>
  </div>
</template>
