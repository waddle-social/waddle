<script setup lang="ts">
import { Hash, Plus, Settings, Users, UserPlus } from "lucide-vue-next";
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import { consistentColor } from "@/lib/chat-ui";

const props = defineProps<{
  waddle: WaddleSummary | null;
  channels: ChannelSummary[];
  activeChannelId: string | null;
  canManageChannels: boolean;
  canManageCommunity: boolean;
  isLoading: boolean;
  memberCount: number;
  activeChannelJids: Set<string>;
  /** XEP-0486: Map of room JID → avatar hash */
  roomAvatarHashes?: Record<string, string>;
}>();

/** Get avatar color for a channel that has a room avatar. */
function channelAvatarColor(channelName: string): string {
  return consistentColor(channelName, 55, 40);
}

/** Get initial letter for channel avatar. */
function channelInitial(name: string): string {
  return (name[0] ?? "#").toUpperCase();
}

function hasActivity(channelId: string): boolean {
  // Check if any JID in the active set contains this channel ID
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
  <div class="w-80 border-r border-foreground bg-background flex flex-col flex-shrink-0">
    <div class="h-20 px-6 border-b border-foreground flex items-center justify-between">
      <div>
        <h2 class="text-xs font-mono uppercase tracking-widest text-muted-foreground mb-1">
          Channels
        </h2>
        <div class="text-2xl font-mono font-bold">{{ waddle?.name ?? "..." }}</div>
      </div>
      <div class="flex gap-1">
        <button
          v-if="canManageCommunity"
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
          title="Settings"
          @click="emit('openSettings')"
        >
          <Settings class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="canManageChannels"
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
          title="Add channel"
          @click="emit('createChannel')"
        >
          <Plus class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>

    <div class="flex-1 overflow-auto p-4">
      <div v-if="isLoading" class="text-center py-8 text-sm font-mono text-muted-foreground">
        Loading...
      </div>

      <div v-else-if="channels.length === 0" class="text-center py-8 text-sm font-mono text-muted-foreground">
        No channels yet
      </div>

      <div v-else class="space-y-1">
        <button
          v-for="channel in channels"
          :key="channel.id"
          class="w-full flex items-center gap-3 px-4 py-3 transition-colors"
          :class="activeChannelId === channel.id ? 'bg-foreground text-background' : 'hover:bg-muted'"
          @click="emit('selectChannel', channel.id)"
        >
          <div class="flex items-center gap-2 flex-1 min-w-0">
            <!-- XEP-0486: Channel avatar (colored initial) or Hash icon -->
            <span
              class="w-5 h-5 rounded text-[10px] font-bold flex items-center justify-center flex-shrink-0 text-white"
              :style="{ backgroundColor: channelAvatarColor(channel.name) }"
            >{{ channelInitial(channel.name) }}</span>
            <span class="text-sm font-mono truncate" :class="hasActivity(channel.id) && activeChannelId !== channel.id ? 'font-bold' : ''">{{ channel.name }}</span>
          </div>
          <span
            v-if="hasActivity(channel.id) && activeChannelId !== channel.id"
            class="w-2 h-2 bg-blue-500 rounded-full flex-shrink-0"
          />
          <span
            v-else-if="channel.is_default"
            class="text-[10px] font-mono uppercase tracking-wider opacity-60"
          >
            default
          </span>
        </button>
      </div>
    </div>

    <!-- Members / Invite footer -->
    <div v-if="waddle" class="h-16 border-t border-foreground flex-shrink-0">
      <button
        class="w-full h-full flex items-center gap-3 px-6 hover:bg-muted transition-colors text-left"
        @click="emit('openMembers')"
      >
        <Users class="w-3.5 h-3.5 flex-shrink-0" />
        <span class="text-sm font-mono flex-1">Members</span>
        <span class="text-xs font-mono text-muted-foreground">{{ memberCount }}</span>
      </button>
    </div>
  </div>
</template>
