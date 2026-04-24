<script setup lang="ts">
import { MessageCircle } from "lucide-vue-next";
import type { SpaceSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import type { ServerVersion } from "@/composables/useVersion";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";

const props = defineProps<{
  waddles: SpaceSummary[];
  activeSpaceId: string | null;
  activeSidebarMode?: "channels" | "dms";
  hasUnreadDms?: boolean;
  session: WaddleSession | null;
  notificationPermission?: NotificationPermission;
  notificationsEnabled?: boolean;
  totalUnreadCount?: number;
  totalMentionCount?: number;
  horizontal?: boolean;
  webCommitSha?: string;
  serverVersion?: ServerVersion | null;
}>();

const emit = defineEmits<{
  selectSpace: [id: string];
  toggleDms: [];
  openSettings: [];
  logout: [];
  "request-notifications": [];
  "toggle-notifications": [];
}>();

function waddleInitial(waddle: SpaceSummary): string {
  return (waddle.name[0] ?? "W").toUpperCase();
}

function waddleColor(waddle: SpaceSummary): string {
  const colors = [
    "oklch(0.78 0.14 184)", "oklch(0.68 0.19 28)", "oklch(0.72 0.15 158)", "oklch(0.78 0.15 78)",
    "oklch(0.64 0.16 252)", "oklch(0.66 0.16 305)", "oklch(0.68 0.17 350)", "oklch(0.66 0.14 205)",
    "oklch(0.72 0.17 54)", "oklch(0.64 0.15 286)",
  ];
  let hash = 0;
  for (const char of waddle.name) hash = ((hash << 5) - hash + char.charCodeAt(0)) | 0;
  return colors[Math.abs(hash) % colors.length];
}

function waddleKey(waddle: SpaceSummary): string {
  return waddle.name.toLowerCase() || "space";
}

function waddleGlow(waddle: SpaceSummary): string {
  return `0 0 16px color-mix(in oklab, ${waddleColor(waddle)} 30%, transparent)`;
}
</script>

<template>
  <div
    class="chat-rail"
    :class="horizontal
      ? 'chat-rail-horizontal'
      : 'chat-rail-vertical'"
  >
    <!-- Logo -->
    <div class="chat-rail-logo-slot flex flex-shrink-0 items-center justify-center">
      <img
        src="/waddle-logo.svg"
        alt="Waddle"
        width="40"
        height="40"
        class="chat-rail-logo-mark rounded-lg object-contain"
      />
    </div>

    <!-- Divider -->
    <div class="bg-border flex-shrink-0" :class="horizontal ? 'chat-rail-divider-horizontal' : 'chat-rail-divider-vertical'" />

    <!-- Waddle icons -->
    <div
      class="chat-pane-scroll chat-rail-list"
      :class="horizontal
        ? 'chat-rail-list-horizontal'
        : 'chat-rail-list-vertical'"
    >
      <button
        v-for="waddle in waddles"
        :key="waddleKey(waddle)"
        class="type-control relative w-10 h-10 rounded-lg flex items-center justify-center transition-all duration-200 group flex-shrink-0"
        :class="activeSpaceId
          ? 'text-primary-foreground shadow-lg'
          : 'text-rail-foreground hover:text-rail-active hover:scale-105'"
        :style="activeSpaceId
          ? { backgroundColor: waddleColor(waddle), boxShadow: waddleGlow(waddle) }
          : { backgroundColor: 'var(--rail-hover)' }"
        :title="waddle.name"
        :aria-label="`Open ${waddle.name}`"
        :aria-current="activeSpaceId ? 'page' : undefined"
        type="button"
        @click="emit('selectSpace', activeSpaceId ?? 'space')"
      >
        <span class="type-rail-initial">{{ waddleInitial(waddle) }}</span>
        <span
          v-if="activeSpaceId && !horizontal"
          class="chat-rail-indicator-vertical absolute -left-2 top-1/2 -translate-y-1/2 rounded-r-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
        <span
          v-if="activeSpaceId && horizontal"
          class="chat-rail-indicator-horizontal absolute bottom-0 left-1/2 -translate-x-1/2 rounded-t-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
      </button>

      <div
        v-if="waddles.length === 0"
        class="type-meta text-muted-foreground/30 text-center"
        :class="horizontal ? 'flex-1 self-stretch flex items-center justify-center' : 'self-stretch px-2'"
      >
        No space
      </div>
    </div>

    <!-- Bottom actions -->
    <div class="chat-rail-actions" :class="horizontal ? 'chat-rail-actions-horizontal' : 'chat-rail-actions-vertical'">
      <button
        class="relative w-10 h-10 flex items-center justify-center rounded-lg transition-all duration-200 hover:scale-105"
        :class="activeSidebarMode === 'dms'
          ? 'bg-rail-hover text-primary'
          : 'text-rail-foreground hover:bg-rail-hover hover:text-primary'"
        title="Direct messages"
        aria-label="Direct messages"
        :aria-pressed="activeSidebarMode === 'dms'"
        type="button"
        @click="emit('toggleDms')"
      >
        <MessageCircle class="w-4 h-4" />
        <span
          v-if="hasUnreadDms"
          class="absolute -top-0.5 -right-0.5 w-2 h-2 rounded-full bg-primary shadow-[0_0_6px_var(--glow-strong)]"
        />
      </button>
    </div>

    <!-- Profile footer (vertical only) -->
    <ProfilePanel
      v-if="session && !horizontal"
      :session="session"
      :notification-permission="notificationPermission"
      :notifications-enabled="notificationsEnabled"
      :total-unread-count="totalUnreadCount"
      :total-mention-count="totalMentionCount"
      :web-commit-sha="webCommitSha"
      :server-version="serverVersion"
      compact
      @open-settings="emit('openSettings')"
      @logout="emit('logout')"
      @request-notifications="emit('request-notifications')"
      @toggle-notifications="emit('toggle-notifications')"
    />
  </div>
</template>
