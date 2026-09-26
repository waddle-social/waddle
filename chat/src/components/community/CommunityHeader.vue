<script setup lang="ts">
import { computed } from "vue";
import { ChevronDown, Radio, Search } from "lucide-vue-next";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";
import { buildHref, type RouteMatch } from "@/router";
import type { ChatAppController } from "@/shell/chat-app-controller";

const props = defineProps<{
  controller: ChatAppController;
  /** True while a room is active and the message search can be opened. */
  canSearch?: boolean;
  openSearch?: () => void;
  startHuddle: () => void;
}>();

const {
  ui,
  waddles,
  connectionStore,
  channelUnread,
  notifications,
  version,
  openHome,
  openRooms,
  openThreads,
  openCommunitySurface,
  openMembers,
  openUserSettings,
  handleLogout,
  handleRequestNotifications,
  handleToggleNotifications,
  handleToggleMessageSounds,
} = props.controller;

type NavId = "home" | "rooms" | "threads" | "events" | "members";

interface NavItem {
  id: NavId;
  label: string;
  href: string;
  go: () => void;
}

const navItems: NavItem[] = [
  { id: "home", label: "Home", href: buildHref({ id: "home" } as RouteMatch), go: () => openHome() },
  { id: "rooms", label: "Rooms", href: buildHref({ id: "rooms" } as RouteMatch), go: () => openRooms() },
  { id: "threads", label: "Discussions", href: buildHref({ id: "threads" } as RouteMatch), go: () => openThreads() },
  { id: "events", label: "Events", href: buildHref({ id: "events" } as RouteMatch), go: () => openCommunitySurface("events") },
  { id: "members", label: "Members", href: buildHref({ id: "members" } as RouteMatch), go: () => openMembers() },
];

const communityName = computed(() =>
  waddles.currentSpace.value?.name ?? waddles.sortedSpaces.value[0]?.name ?? "Waddle",
);

const activeNav = computed<NavId | null>(() => {
  if (ui.activeCommunitySurface.value === "events") return "events";
  if (ui.activeCommunitySurface.value === "feed") return null;
  switch (ui.activePage.value) {
    case "dashboard":
      return "home";
    case "rooms":
    case "chat":
      return "rooms";
    case "threads":
    case "unread":
      return "threads";
    case "members":
      return "members";
    default:
      return null;
  }
});

const threadsUnread = computed(() => channelUnread.totalThreadUnreadCount.value);
</script>

<template>
  <header class="community-header">
    <button
      type="button"
      class="community-header__brand"
      :aria-label="`${communityName}, go to Home`"
      @click="openHome()"
    >
      <img src="/waddle-logo.svg" alt="" width="30" height="30" class="community-header__logo" />
      <span class="community-header__name">{{ communityName }}</span>
      <ChevronDown class="community-header__chevron" aria-hidden="true" />
    </button>

    <nav class="community-nav" aria-label="Primary">
      <a
        v-for="item in navItems"
        :key="item.id"
        :href="item.href"
        class="community-nav__pill"
        :aria-current="activeNav === item.id ? 'page' : undefined"
        @click.prevent="item.go()"
      >
        {{ item.label }}
        <span
          v-if="item.id === 'threads' && threadsUnread > 0"
          class="community-count text-live-text"
          :aria-label="`${threadsUnread} unread`"
        >{{ threadsUnread }}</span>
      </a>
    </nav>

    <div class="community-header__actions">
      <button
        v-if="canSearch"
        type="button"
        class="community-header__action community-header__action--icon"
        aria-label="Search"
        @click="openSearch?.()"
      >
        <Search class="h-4 w-4" aria-hidden="true" />
      </button>
      <button
        type="button"
        class="community-header__action community-header__action--primary"
        aria-label="Start a huddle"
        @click="startHuddle()"
      >
        <Radio class="h-4 w-4" aria-hidden="true" />
        <span class="community-header__action-label">Start a huddle</span>
      </button>
      <div v-if="connectionStore.session" class="community-header__account">
        <ProfilePanel
          :session="connectionStore.session"
          :notification-permission="notifications.permissionState.value"
          :notifications-enabled="notifications.notificationsEnabled.value"
          :message-sounds-enabled="notifications.messageSoundsEnabled.value"
          :total-unread-count="channelUnread.totalUnreadCount.value"
          :total-mention-count="channelUnread.totalMentionCount.value"
          :web-commit-sha="version.webCommitSha.value"
          :server-version="version.serverVersion.value"
          compact
          placement="bottom-end"
          @open-settings="openUserSettings"
          @logout="handleLogout"
          @request-notifications="handleRequestNotifications"
          @toggle-notifications="handleToggleNotifications"
          @toggle-message-sounds="handleToggleMessageSounds"
        />
      </div>
    </div>
  </header>
</template>
