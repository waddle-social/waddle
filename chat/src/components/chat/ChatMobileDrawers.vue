<script setup lang="ts">
import { computed } from "vue";
import WaddlesSidebar from "@/components/chat/WaddlesSidebar.vue";
import TopicsPanel from "@/components/chat/TopicsPanel.vue";
import DmPanel from "@/components/chat/DmPanel.vue";
import SettingsMobileHeader from "@/components/chat/SettingsMobileHeader.vue";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import { extensionRouteIconComponent, extensionRouteRailItems } from "./extension-route-rail-model";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import type { ChatAppController } from "@/shell/chat-app-controller";

const props = defineProps<{
  controller: ChatAppController;
}>();

const {
  connectionStore,
  ui,
  waddles,
  messaging,
  dmConversations,
  channelUnread,
  notifications,
  version,
  displayedMemberCount,
  displayedMemberState,
  memberCountLabel,
  computedChannelUnreadMap,
  channelExtensionRoutes,
  activeExtensionRouteKey,
  activeRightPanel,
  openUserSettings,
  handleLogout,
  handleRequestNotifications,
  handleToggleNotifications,
  selectChannel,
  onSelectThread,
  selectDm,
  selectExtensionRoute,
  openCreateChannelDialog,
  openChannelEdit,
} = props.controller;

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
        <div class="border-b border-border">
          <WaddlesSidebar
            :waddles="[]"
            :active-space-id="null"
            :active-sidebar-mode="ui.sidebarMode.value"
            :has-unread-dms="dmConversations.hasUnread.value"
            :session="null"
            horizontal
            @toggle-channels="ui.sidebarMode.value = 'channels'"
            @toggle-dms="ui.sidebarMode.value = 'dms'"
          />
        </div>
        <TopicsPanel
          v-if="ui.sidebarMode.value === 'channels'"
          :waddle="waddles.currentSpace.value"
          :spaces="waddles.sortedSpaces.value"
          :channels="waddles.sortedChannels.value"
          :active-channel-id="waddles.activeChannelId.value"
          :can-manage-channels="waddles.canManageChannels.value"
          :can-manage-community="waddles.canManageCommunity.value"
          :is-loading="waddles.isLoadingStructure.value"
          :member-count="displayedMemberCount"
          :member-state="displayedMemberState"
          :active-channel-jids="messaging.activeChannels.value"
          :collapsed-group-ids="ui.collapsedSpaceGroupIds.value"
          :channel-unread-map="computedChannelUnreadMap"
          :thread-entries-fn="(roomJid: string) => channelUnread.threadEntries(roomJid)"
          class="!w-full !border-r-0 !flex-1"
          @select-channel="selectChannel"
          @select-thread="onSelectThread"
          @create-channel="openCreateChannelDialog()"
          @create-channel-in-space="openCreateChannelDialog"
          @open-settings="ui.showWaddleSettings.value = true"
          @open-members="ui.showMembers.value = true"
          @update-collapsed-group-ids="ui.collapsedSpaceGroupIds.value = $event"
        />
        <DmPanel
          v-else
          :conversations="dmConversations.conversations.value"
          :active-peer-jid="dmConversations.activePeerJid.value"
          class="!w-full !border-r-0 !flex-1"
          @select-dm="selectDm"
          @new-dm="ui.showNewDm.value = true"
        />
        <ProfilePanel
          v-if="connectionStore.session"
          :session="connectionStore.session"
          :notification-permission="notifications.permissionState.value"
          :notifications-enabled="notifications.notificationsEnabled.value"
          :total-unread-count="channelUnread.totalUnreadCount.value"
          :total-mention-count="channelUnread.totalMentionCount.value"
          :web-commit-sha="version.webCommitSha.value"
          :server-version="version.serverVersion.value"
          @open-settings="openUserSettings"
          @logout="handleLogout"
          @request-notifications="handleRequestNotifications"
          @toggle-notifications="handleToggleNotifications"
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
