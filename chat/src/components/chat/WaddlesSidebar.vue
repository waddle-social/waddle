<script setup lang="ts">
import { computed } from "vue";
import { Plus, Lock, Compass, MessageCircle } from "lucide-vue-next";
import type { WaddleSummary } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";
import type { ServerVersion } from "@/composables/useVersion";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";

const props = defineProps<{
  waddles: WaddleSummary[];
  activeWaddleId: string | null;
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
  selectWaddle: [id: string];
  createWaddle: [];
  browsePublicWaddles: [];
  toggleDms: [];
  openSettings: [];
  logout: [];
  "request-notifications": [];
  "toggle-notifications": [];
}>();

const publicWaddles = computed(() =>
  props.waddles.filter((w) => w.is_public),
);

const privateWaddles = computed(() =>
  props.waddles.filter((w) => !w.is_public),
);

function waddleInitial(waddle: WaddleSummary): string {
  return (waddle.name[0] ?? "W").toUpperCase();
}

function waddleColor(waddle: WaddleSummary): string {
  const colors = [
    "oklch(0.78 0.14 184)", "oklch(0.68 0.19 28)", "oklch(0.72 0.15 158)", "oklch(0.78 0.15 78)",
    "oklch(0.64 0.16 252)", "oklch(0.66 0.16 305)", "oklch(0.68 0.17 350)", "oklch(0.66 0.14 205)",
    "oklch(0.72 0.17 54)", "oklch(0.64 0.15 286)",
  ];
  let hash = 0;
  for (const char of waddle.id) hash = ((hash << 5) - hash + char.charCodeAt(0)) | 0;
  return colors[Math.abs(hash) % colors.length];
}

function waddleGlow(waddle: WaddleSummary): string {
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
        v-for="waddle in privateWaddles"
        :key="waddle.id"
        class="type-control relative w-10 h-10 rounded-lg flex items-center justify-center transition-all duration-200 group flex-shrink-0"
        :class="activeWaddleId === waddle.id
          ? 'text-primary-foreground shadow-lg'
          : 'text-rail-foreground hover:text-rail-active hover:scale-105'"
        :style="activeWaddleId === waddle.id
          ? { backgroundColor: waddleColor(waddle), boxShadow: waddleGlow(waddle) }
          : { backgroundColor: 'var(--rail-hover)' }"
        :title="waddle.name"
        :aria-label="`Open ${waddle.name}`"
        :aria-current="activeWaddleId === waddle.id ? 'page' : undefined"
        type="button"
        @click="emit('selectWaddle', waddle.id)"
      >
        <span class="type-rail-initial">{{ waddleInitial(waddle) }}</span>
        <!-- Active indicator -->
        <span
          v-if="activeWaddleId === waddle.id && !horizontal"
          class="chat-rail-indicator-vertical absolute -left-2 top-1/2 -translate-y-1/2 rounded-r-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
        <span
          v-if="activeWaddleId === waddle.id && horizontal"
          class="chat-rail-indicator-horizontal absolute bottom-0 left-1/2 -translate-x-1/2 rounded-t-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
        <!-- Lock badge -->
        <Lock class="absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 text-muted-foreground/50" />
      </button>

      <template v-if="publicWaddles.length > 0 && privateWaddles.length > 0">
        <div class="bg-border flex-shrink-0" :class="horizontal ? 'chat-rail-divider-horizontal' : 'chat-rail-divider-vertical'" />
      </template>

      <button
        v-for="waddle in publicWaddles"
        :key="waddle.id"
        class="type-control relative w-10 h-10 rounded-lg flex items-center justify-center transition-all duration-200 flex-shrink-0"
        :class="activeWaddleId === waddle.id
          ? 'text-primary-foreground shadow-lg'
          : 'text-rail-foreground hover:text-rail-active hover:scale-105'"
        :style="activeWaddleId === waddle.id
          ? { backgroundColor: waddleColor(waddle), boxShadow: waddleGlow(waddle) }
          : { backgroundColor: 'var(--rail-hover)' }"
        :title="waddle.name"
        :aria-label="`Open ${waddle.name}`"
        :aria-current="activeWaddleId === waddle.id ? 'page' : undefined"
        type="button"
        @click="emit('selectWaddle', waddle.id)"
      >
        <span class="type-rail-initial">{{ waddleInitial(waddle) }}</span>
        <span
          v-if="activeWaddleId === waddle.id && !horizontal"
          class="chat-rail-indicator-vertical absolute -left-2 top-1/2 -translate-y-1/2 rounded-r-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
        <span
          v-if="activeWaddleId === waddle.id && horizontal"
          class="chat-rail-indicator-horizontal absolute bottom-0 left-1/2 -translate-x-1/2 rounded-t-full"
          :style="{ backgroundColor: waddleColor(waddle) }"
        />
      </button>

      <div
        v-if="waddles.length === 0"
        class="type-meta text-muted-foreground/30 text-center"
        :class="horizontal ? 'flex-1 self-stretch flex items-center justify-center' : 'self-stretch px-2'"
      >
        No waddles
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
      <button
        class="w-10 h-10 flex items-center justify-center rounded-lg text-rail-foreground hover:bg-rail-hover hover:text-primary transition-all duration-200 hover:scale-105"
        title="Browse public spaces"
        aria-label="Browse public spaces"
        type="button"
        @click="emit('browsePublicWaddles')"
      >
        <Compass class="w-4 h-4" />
      </button>
      <button
        class="w-10 h-10 flex items-center justify-center rounded-lg text-rail-foreground hover:bg-rail-hover hover:text-primary transition-all duration-200 hover:scale-105"
        title="Create waddle"
        aria-label="Create waddle"
        type="button"
        @click="emit('createWaddle')"
      >
        <Plus class="w-4 h-4" />
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
