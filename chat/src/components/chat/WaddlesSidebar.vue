<script setup lang="ts">
import { Layers, MessageCircle, PhoneCall } from "lucide-vue-next";
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
  activeChannelCallCount?: number;
  activeDmCallCount?: number;
  horizontal?: boolean;
  webCommitSha?: string;
  serverVersion?: XmppServerVersion | null;
  /** Current top-level page so the Home/logo button can render an
   * "active" treatment when we're already on the dashboard. */
  activePage?: "dashboard" | "chat" | "settings" | "admin" | "threads";
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

function activeCallLabel(count: number | undefined, noun = "call"): string {
  const value = count ?? 0;
  if (value <= 0) return "";
  return `${value} active ${noun}${value === 1 ? "" : "s"}`;
}

function spacesLabel(activeCallCount?: number): string {
  const call = activeCallLabel(activeCallCount);
  return call ? `Spaces, ${call}` : "Spaces";
}

function directMessagesLabel(hasUnread?: boolean, activeCallCount?: number): string {
  const parts = ["Direct messages"];
  if (hasUnread) parts.push("unread messages");
  const call = activeCallLabel(activeCallCount);
  if (call) parts.push(call);
  return parts.join(", ");
}
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
      <!-- Spaces / DMs rail toggles — active state picks up the same
           brand-armed treatment as the iter-61 channel-header toggles,
           iter-54 bell badge, and iter-64 extension-route rail:
           primary ring + glow halo so "you are currently in this
           mode" reads distinct from "you are hovering this button". -->
      <button
        class="relative w-10 h-10 flex items-center justify-center rounded-lg transition-all duration-200 hover:scale-105"
        :class="activeSidebarMode === 'channels'
          ? 'bg-rail-hover text-primary ring-1 ring-primary/40 shadow-[0_0_10px_var(--glow)]'
          : 'text-rail-foreground hover:bg-rail-hover hover:text-primary'"
        :title="spacesLabel(activeChannelCallCount)"
        :aria-label="spacesLabel(activeChannelCallCount)"
        :aria-pressed="activeSidebarMode === 'channels'"
        type="button"
        @click="emit('toggleChannels')"
      >
        <Layers class="w-4 h-4" />
        <span
          v-if="(activeChannelCallCount ?? 0) > 0"
          class="absolute -bottom-0.5 -right-0.5 flex h-4 w-4 items-center justify-center rounded-full border border-rail bg-background text-success shadow-[0_0_8px_var(--success)]"
          :title="activeCallLabel(activeChannelCallCount)"
          aria-hidden="true"
        >
          <PhoneCall class="h-2.5 w-2.5" />
        </span>
      </button>
      <button
        class="relative w-10 h-10 flex items-center justify-center rounded-lg transition-all duration-200 hover:scale-105"
        :class="activeSidebarMode === 'dms'
          ? 'bg-rail-hover text-primary ring-1 ring-primary/40 shadow-[0_0_10px_var(--glow)]'
          : 'text-rail-foreground hover:bg-rail-hover hover:text-primary'"
        :title="directMessagesLabel(hasUnreadDms, activeDmCallCount)"
        :aria-label="directMessagesLabel(hasUnreadDms, activeDmCallCount)"
        :aria-pressed="activeSidebarMode === 'dms'"
        type="button"
        @click="emit('toggleDms')"
      >
        <MessageCircle class="w-4 h-4" />
        <span
          v-if="hasUnreadDms"
          class="absolute -top-0.5 -right-0.5 w-2 h-2 rounded-full bg-primary shadow-[0_0_6px_var(--glow-strong)]"
        />
        <span
          v-if="(activeDmCallCount ?? 0) > 0"
          class="absolute -bottom-0.5 -right-0.5 flex h-4 w-4 items-center justify-center rounded-full border border-rail bg-background text-success shadow-[0_0_8px_var(--success)]"
          :title="activeCallLabel(activeDmCallCount)"
          aria-hidden="true"
        >
          <PhoneCall class="h-2.5 w-2.5" />
        </span>
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
