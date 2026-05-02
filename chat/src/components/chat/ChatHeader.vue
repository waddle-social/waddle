<script setup lang="ts">
import type { Component } from "vue";
import { ChevronRight, Hash, Menu, MessageCircle, MessagesSquare, Search, Settings, Users } from "lucide-vue-next";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { ConnectionNoticeCopy } from "@/lib/connection-notice";

interface ConnectionStatusClasses {
  banner: string;
  iconWrap: string;
  chip: string;
  body: string;
}

const props = defineProps<{
  waddle: SpaceSummary | null;
  channel: ChannelSummary | null;
  dmPeer?: { peerJid?: string; peerUsername: string; presenceShow?: string } | null;
  isForumChannel: boolean;
  canManageChannels: boolean;
  memberCount: number;
  connectionNotice: ConnectionNoticeCopy | null;
  connectionStatusClasses: ConnectionStatusClasses | null;
  connectionStatusIcon: Component;
}>();

const showSearch = defineModel<boolean>("showSearch", { required: true });

const emit = defineEmits<{
  openNav: [];
  openDetails: [];
  editChannel: [];
}>();

function presenceText(show?: string): string {
  if (show === "available") return "online";
  if (show === "away") return "away";
  if (show === "dnd") return "do not disturb";
  if (show === "xa") return "extended away";
  return "offline";
}
</script>

<template>
  <div class="chat-pane-header border-b border-border px-[var(--chat-content-inline)] py-0 flex flex-shrink-0 items-center justify-between gap-[var(--space-md)] glass-surface">
    <div class="chat-message-lane flex min-w-0 items-center gap-2">
      <button
        class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground lg:hidden"
        type="button"
        aria-label="Open navigation"
        @click="emit('openNav')"
      >
        <Menu class="w-4 h-4" />
      </button>
      <div class="chat-pane-title-group">
        <span class="chat-pane-title-icon rounded-lg bg-primary/8">
          <component :is="dmPeer ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-4 h-4 text-primary/70" />
        </span>
        <div class="min-w-0">
          <div class="flex min-w-0 items-center gap-2">
            <h1 class="type-chat-title truncate">
              {{ dmPeer ? dmPeer.peerUsername : channel?.name ?? "…" }}
            </h1>
            <span
              v-if="isForumChannel"
              class="type-badge rounded-full border border-primary/15 bg-primary/8 px-2 py-0.5 text-primary/80"
            >
              Forum
            </span>
            <span v-if="dmPeer" class="hidden lg:inline type-meta text-muted-foreground">
              · {{ presenceText(dmPeer.presenceShow) }}
            </span>
          </div>
          <div v-if="dmPeer" class="lg:hidden type-caption text-muted-foreground truncate">
            {{ presenceText(dmPeer.presenceShow) }}
          </div>
          <div v-else-if="channel && waddle" class="lg:hidden type-caption text-muted-foreground truncate">
            {{ waddle.name }}
          </div>
        </div>
      </div>
    </div>
    <div class="flex shrink-0 items-center gap-3">
      <div
        v-if="connectionNotice && connectionStatusClasses"
        class="type-caption type-emphasis hidden md:inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1"
        :class="connectionStatusClasses.chip"
      >
        <component
          :is="connectionStatusIcon"
          class="w-3.5 h-3.5"
          :class="{ 'motion-safe:animate-spin': connectionNotice.tone === 'reconnecting' }"
        />
        <span>{{ connectionNotice.shortLabel }}</span>
      </div>
      <div class="flex items-center gap-1.5">
        <button
          v-if="channel || dmPeer"
          class="chat-icon-button chat-icon-button--md transition-all duration-200"
          :class="showSearch ? 'bg-muted text-primary' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
          title="Search messages"
          aria-label="Search messages"
          :aria-pressed="showSearch"
          type="button"
          @click="showSearch = !showSearch"
        >
          <Search class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="canManageChannels && channel"
          class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground"
          title="Channel settings"
          aria-label="Channel settings"
          type="button"
          @click="emit('editChannel')"
        >
          <Settings class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="channel"
          class="chat-action-button chat-action-button--secondary text-muted-foreground hover:text-foreground"
          type="button"
          :title="`${memberCount} ${memberCount === 1 ? 'member' : 'members'}`"
          :aria-label="`Open details (${memberCount} ${memberCount === 1 ? 'member' : 'members'})`"
          @click="emit('openDetails')"
        >
          <Users class="w-3.5 h-3.5" />
          <span class="type-control">{{ memberCount }}</span>
          <span class="hidden lg:inline type-control">{{ memberCount === 1 ? "member" : "members" }}</span>
          <ChevronRight class="w-3.5 h-3.5 opacity-60" />
        </button>
      </div>
    </div>
  </div>
</template>
