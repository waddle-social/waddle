<script setup lang="ts">
import { Hash, MessagesSquare, Plus, Settings, Users, ChevronDown } from "lucide-vue-next";
import { isForumChannel as detectForumChannel } from "@/lib/channel-types";
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";

const props = defineProps<{
  waddle: WaddleSummary | null;
  channels: ChannelSummary[];
  activeChannelId: string | null;
  canManageChannels: boolean;
  canManageCommunity: boolean;
  isLoading: boolean;
  memberCount: number;
  activeChannelJids: Set<string>;
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

function channelIcon(channel: ChannelSummary) {
  return detectForumChannel(channel) ? MessagesSquare : Hash;
}

const emit = defineEmits<{
  selectChannel: [id: string];
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
        <button
          v-for="channel in channels"
          :key="channel.id"
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
              hasActivity(channel.id) && activeChannelId !== channel.id ? 'font-semibold text-sidebar-foreground' : '',
            ]"
          >{{ channel.name }}</span>
          <span
            v-if="detectForumChannel(channel)"
            class="rounded-full border border-primary/12 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-[0.12em] text-primary/70"
          >
            Forum
          </span>
          <span
            v-if="hasActivity(channel.id) && activeChannelId !== channel.id"
            class="w-2 h-2 bg-primary rounded-full flex-shrink-0 shadow-[0_0_6px_var(--glow-strong)]"
          />
        </button>
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
