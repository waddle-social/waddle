<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $mucCallMedia,
  $mucCallParticipants,
  mucCallParticipantCounts,
} from "@/lib/calls/muc-call-presence";
import { $dmCallActivities } from "@/lib/calls/dm-call-activity";
import { $callState } from "@/lib/calls/call-store";
import {
  activeChannelRailCallCount,
  activeDmRailCallCount,
} from "@/lib/calls/call-rail-counts";
import PeopleRail from "@/components/community/PeopleRail.vue";
import SettingsMobileHeader from "@/components/chat/SettingsMobileHeader.vue";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import { extensionRouteIconComponent, extensionRouteRailItems } from "./extension-route-rail-model";
import { buildHref, type RouteMatch } from "@/router";
import type { CallMedia } from "@/lib/calls/types";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import type { ChatAppController } from "@/shell/chat-app-controller";

const props = defineProps<{
  controller: ChatAppController;
  activeChannelCallCount?: number;
  activeDmCallCount?: number;
  callParticipantCounts?: Record<string, number>;
  callParticipants?: Record<string, string[]>;
  callMediaByRoom?: Record<string, CallMedia>;
}>();

const {
  connectionStore,
  ui,
  waddles,
  channelUnread,
  notifications,
  version,
  memberCountLabel,
  channelExtensionRoutes,
  activeExtensionRouteKey,
  activeRightPanel,
  openUserSettings,
  openHome,
  openRooms,
  openMembers,
  openCommunitySurface,
  handleLogout,
  handleRequestNotifications,
  handleToggleNotifications,
  handleToggleMessageSounds,
  selectExtensionRoute,
  openChannelEdit,
  openThreads,
} = props.controller;

type DrawerNavId = "home" | "rooms" | "threads" | "events" | "members";

/** Compact list of the five community destinations for the drawer. */
const drawerNav: { id: DrawerNavId; label: string; href: string; go: () => void }[] = [
  { id: "home", label: "Home", href: buildHref({ id: "home" } as RouteMatch), go: () => openHome() },
  { id: "rooms", label: "Rooms", href: buildHref({ id: "rooms" } as RouteMatch), go: () => openRooms() },
  { id: "threads", label: "Discussions", href: buildHref({ id: "threads" } as RouteMatch), go: () => openThreads() },
  { id: "events", label: "Events", href: buildHref({ id: "events" } as RouteMatch), go: () => selectCommunitySurface("events") },
  { id: "members", label: "Members", href: buildHref({ id: "members" } as RouteMatch), go: () => openMembers() },
];

const activeDrawerNav = computed<DrawerNavId | null>(() => {
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

function selectCommunitySurface(surface: "feed" | "events") {
  openCommunitySurface(surface);
}

// XEP-0272 Muji participants keyed by room JID — the same retained
// reactive state ChatReadyShell derives, so the drawer's people rail
// shows the same huddle membership as the desktop rail.
const mucCallParticipantsStore = useStore($mucCallParticipants);
const mucCallMediaStore = useStore($mucCallMedia);
const dmCallActivitiesStore = useStore($dmCallActivities);
const callStateStore = useStore($callState);
const fallbackCallParticipantCounts = computed<Record<string, number>>(() => {
  return mucCallParticipantCounts(mucCallParticipantsStore.value);
});
const visibleCallParticipantCounts = computed<Record<string, number>>(() =>
  props.callParticipantCounts ?? fallbackCallParticipantCounts.value,
);
const visibleCallParticipants = computed<Record<string, string[]>>(() =>
  props.callParticipants ?? mucCallParticipantsStore.value,
);
const visibleCallMediaByRoom = computed<Record<string, CallMedia>>(() =>
  props.callMediaByRoom ?? mucCallMediaStore.value,
);
const visibleActiveChannelCallCount = computed(() => {
  return props.activeChannelCallCount ??
    activeChannelRailCallCount(visibleCallParticipantCounts.value, callStateStore.value);
});
const visibleActiveDmCallCount = computed(() => {
  return props.activeDmCallCount ??
    activeDmRailCallCount(dmCallActivitiesStore.value, callStateStore.value);
});
/** Live-call summary for the drawer header: the desktop header's
 * "Start a huddle" affordance is not in the drawer, so name what is live. */
const liveSummary = computed(() => {
  const parts: string[] = [];
  const rooms = visibleActiveChannelCallCount.value;
  const dms = visibleActiveDmCallCount.value;
  if (rooms > 0) parts.push(`${rooms} room huddle${rooms === 1 ? "" : "s"}`);
  if (dms > 0) parts.push(`${dms} call${dms === 1 ? "" : "s"}`);
  const media = Object.values(visibleCallMediaByRoom.value).some((entry) => entry.video) ? " with video" : "";
  return parts.length > 0 ? `${parts.join(" · ")}${media}` : "";
});

const drawerExtensionRoutes = computed(() =>
  extensionRouteRailItems(
    channelExtensionRoutes.value,
    activeExtensionRouteKey.value,
    activeRightPanel.value === "extension",
  ),
);

function openExtensionRoute(route: DiscoveredExtensionRoute) {
  const channel = waddles.currentChannel.value;
  if (!channel) return;
  ui.showMobileDetails.value = false;
  void selectExtensionRoute(channel.id, route);
}
</script>

<template>
    <!-- Mobile header (settings page only - chat pages render the consolidated header inside ContentArea) -->
    <SettingsMobileHeader
      v-if="ui.activePage.value === 'settings'"
      @open-nav="ui.showMobileNav.value = true"
    />

    <!-- Mobile nav drawer -->
    <AppDrawer v-model:open="ui.showMobileNav.value" side="left" label="Navigation drawer">
      <template #title>
        <span class="type-pane-title">Navigation</span>
      </template>
      <div class="chat-mobile-nav-body">
        <nav class="flex flex-col gap-0.5 border-b border-border p-2" aria-label="Community">
          <a
            v-for="item in drawerNav"
            :key="item.id"
            :href="item.href"
            class="community-nav__pill"
            :aria-current="activeDrawerNav === item.id ? 'page' : undefined"
            @click.prevent="item.go()"
          >
            {{ item.label }}
            <span
              v-if="item.id === 'threads' && channelUnread.totalThreadUnreadCount.value > 0"
              class="community-count text-live-text"
              :aria-label="`${channelUnread.totalThreadUnreadCount.value} unread`"
            >{{ channelUnread.totalThreadUnreadCount.value }}</span>
          </a>
          <span v-if="liveSummary" class="community-kicker community-kicker--live px-3 py-1.5">
            <span class="community-ember" aria-hidden="true" />
            {{ liveSummary }}
          </span>
        </nav>
        <PeopleRail
          :controller="controller"
          :call-participants="visibleCallParticipants"
        />
        <ProfilePanel
          v-if="connectionStore.session"
          :session="connectionStore.session"
          :notification-permission="notifications.permissionState.value"
          :notifications-enabled="notifications.notificationsEnabled.value"
          :message-sounds-enabled="notifications.messageSoundsEnabled.value"
          :total-unread-count="channelUnread.totalUnreadCount.value"
          :total-mention-count="channelUnread.totalMentionCount.value"
          :web-commit-sha="version.webCommitSha.value"
          :server-version="version.serverVersion.value"
          @open-settings="openUserSettings"
          @logout="handleLogout"
          @request-notifications="handleRequestNotifications"
          @toggle-notifications="handleToggleNotifications"
          @toggle-message-sounds="handleToggleMessageSounds"
        />
      </div>
    </AppDrawer>

    <!-- Mobile details drawer -->
    <AppDrawer v-model:open="ui.showMobileDetails.value" side="right" label="Details drawer">
      <template #title>
        <span class="type-pane-title">Details</span>
      </template>
      <div class="flex flex-col gap-4 p-4">
        <div v-if="waddles.currentSpace.value" class="flex flex-col gap-1.5">
          <h3 class="type-pane-title">{{ waddles.currentSpace.value.name }}</h3>
          <p v-if="waddles.currentSpace.value.description" class="type-field text-muted-foreground">
            {{ waddles.currentSpace.value.description }}
          </p>
        </div>

        <div class="flex flex-col gap-1.5">
          <button
            v-if="waddles.currentChannel.value && waddles.canManageChannels.value"
            class="type-control h-9 w-full rounded-lg border border-border px-3 hover:bg-muted transition-colors"
            type="button"
            @click="openChannelEdit(); ui.showMobileDetails.value = false"
          >
            Edit channel
          </button>
          <button
            class="type-control h-9 w-full rounded-lg border border-border px-3 hover:bg-muted transition-colors"
            type="button"
            @click="ui.showMobileDetails.value = false; ui.showMembers.value = true"
          >
            Members ({{ memberCountLabel }})
          </button>
        </div>

        <div
          v-if="drawerExtensionRoutes.length > 0"
          class="flex flex-col gap-1.5 border-t border-border pt-4"
        >
          <h3 class="type-section-label text-muted-foreground">Extensions</h3>
          <button
            v-for="item in drawerExtensionRoutes"
            :key="item.key"
            type="button"
            class="type-control flex h-9 w-full items-center gap-2 rounded-lg border border-border px-3 hover:bg-muted transition-colors"
            :class="item.isActive ? 'border-primary/30 bg-primary/10 text-primary' : ''"
            :aria-current="item.isActive ? 'page' : undefined"
            @click="openExtensionRoute(item.route)"
          >
            <component :is="extensionRouteIconComponent(item.icon)" class="h-4 w-4" aria-hidden="true" />
            <span class="truncate text-left">{{ item.label }}</span>
          </button>
        </div>
      </div>
    </AppDrawer>

</template>
