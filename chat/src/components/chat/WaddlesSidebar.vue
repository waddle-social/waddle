<script setup lang="ts">
import { Layers, MessageCircle } from "lucide-vue-next";
import type { SpaceSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import type { XmppServerVersion } from "@/shell/version";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";

defineProps<{
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
  serverVersion?: XmppServerVersion | null;
  /** Current top-level page so the Home/logo button can render an
   * "active" treatment when we're already on the dashboard. */
  activePage?: "dashboard" | "chat" | "settings";
}>();

const emit = defineEmits<{
  toggleChannels: [];
  toggleDms: [];
  openSettings: [];
  openHome: [];
  logout: [];
  "request-notifications": [];
  "toggle-notifications": [];
}>();
</script>

<template>
  <div
    class="chat-rail"
    :class="horizontal
      ? 'chat-rail-horizontal'
      : 'chat-rail-vertical'"
  >
    <!-- Logo / Home — clicking returns to the dashboard from anywhere.
         Was a passive img; now a real button so users in a channel have
         a route back to Home (channel header doesn't currently expose
         one). Active when `activePage === "dashboard"`. -->
    <div class="chat-rail-logo-slot flex flex-shrink-0 items-center justify-center">
      <button
        type="button"
        class="chat-rail-home-button"
        :class="activePage === 'dashboard' ? 'chat-rail-home-button--active' : ''"
        :aria-pressed="activePage === 'dashboard'"
        title="Home"
        aria-label="Go to home"
        @click="emit('openHome')"
      >
        <img
          src="/waddle-logo.svg"
          alt=""
          width="40"
          height="40"
          class="chat-rail-logo-mark rounded-lg object-contain"
        />
      </button>
    </div>

    <!-- Divider -->
    <div class="bg-border flex-shrink-0" :class="horizontal ? 'chat-rail-divider-horizontal' : 'chat-rail-divider-vertical'" />

    <!-- App spacer: room navigation lives in TopicsPanel as XEP-0503 groups. -->
    <div
      class="chat-pane-scroll chat-rail-list"
      :class="horizontal
        ? 'chat-rail-list-horizontal'
        : 'chat-rail-list-vertical'"
    />

    <!-- Bottom actions -->
    <div class="chat-rail-actions" :class="horizontal ? 'chat-rail-actions-horizontal' : 'chat-rail-actions-vertical'">
      <button
        class="relative w-10 h-10 flex items-center justify-center rounded-lg transition-all duration-200 hover:scale-105"
        :class="activeSidebarMode === 'channels'
          ? 'bg-rail-hover text-primary'
          : 'text-rail-foreground hover:bg-rail-hover hover:text-primary'"
        title="Spaces"
        aria-label="Spaces"
        :aria-pressed="activeSidebarMode === 'channels'"
        type="button"
        @click="emit('toggleChannels')"
      >
        <Layers class="w-4 h-4" />
      </button>
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
