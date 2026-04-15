<script setup lang="ts">
import { Hash, Plus, Settings, Users, ChevronDown } from "lucide-vue-next";
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

const emit = defineEmits<{
  selectChannel: [id: string];
  createChannel: [];
  openSettings: [];
  openMembers: [];
}>();
</script>

<template>
  <div class="w-[240px] border-r border-border bg-sidebar flex flex-col flex-shrink-0">
    <!-- Waddle header -->
    <div class="h-12 px-4 flex items-center justify-between flex-shrink-0 border-b border-sidebar-border">
      <button class="flex items-center gap-1.5 min-w-0 hover:text-sidebar-foreground transition-colors group">
        <span class="text-[13px] font-semibold truncate text-sidebar-foreground">{{ waddle?.name ?? "..." }}</span>
        <ChevronDown class="w-3 h-3 text-sidebar-muted flex-shrink-0" />
      </button>
      <div class="flex gap-0.5 flex-shrink-0">
        <button
          v-if="canManageCommunity"
          class="h-6 w-6 flex items-center justify-center rounded-md hover:bg-sidebar-accent transition-colors"
          title="Settings"
          @click="emit('openSettings')"
        >
          <Settings class="w-3.5 h-3.5 text-sidebar-muted" />
        </button>
        <button
          v-if="canManageChannels"
          class="h-6 w-6 flex items-center justify-center rounded-md hover:bg-sidebar-accent transition-colors"
          title="Add channel"
          @click="emit('createChannel')"
        >
          <Plus class="w-3.5 h-3.5 text-sidebar-muted" />
        </button>
      </div>
    </div>

    <!-- Channel list -->
    <div class="flex-1 overflow-auto py-2 px-2">
      <div class="px-2 mb-1.5">
        <span class="text-[11px] font-medium uppercase tracking-wider text-sidebar-muted">
          Channels
        </span>
      </div>

      <div v-if="isLoading" class="text-center py-6 text-[13px] text-sidebar-muted">
        Loading...
      </div>

      <div v-else-if="channels.length === 0" class="text-center py-6 text-[13px] text-sidebar-muted">
        No channels yet
      </div>

      <div v-else class="space-y-px">
        <button
          v-for="channel in channels"
          :key="channel.id"
          class="w-full flex items-center gap-2 px-2 py-1.5 rounded-md transition-colors text-left"
          :class="activeChannelId === channel.id
            ? 'bg-sidebar-accent text-sidebar-foreground'
            : 'text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'"
          @click="emit('selectChannel', channel.id)"
        >
          <Hash class="w-3.5 h-3.5 flex-shrink-0 opacity-60" />
          <span
            class="text-[13px] truncate flex-1"
            :class="[
              activeChannelId === channel.id ? 'font-medium' : '',
              hasActivity(channel.id) && activeChannelId !== channel.id ? 'font-semibold text-sidebar-foreground' : '',
            ]"
          >{{ channel.name }}</span>
          <span
            v-if="hasActivity(channel.id) && activeChannelId !== channel.id"
            class="w-1.5 h-1.5 bg-primary rounded-full flex-shrink-0"
          />
        </button>
      </div>
    </div>

    <!-- Members footer -->
    <div v-if="waddle" class="flex-shrink-0 px-2 py-2 border-t border-sidebar-border">
      <button
        class="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground transition-colors text-left"
        @click="emit('openMembers')"
      >
        <Users class="w-3.5 h-3.5 flex-shrink-0 opacity-60" />
        <span class="text-[13px] flex-1">Members</span>
        <span class="text-[11px] text-sidebar-muted font-mono">{{ memberCount }}</span>
      </button>
    </div>
  </div>
</template>
