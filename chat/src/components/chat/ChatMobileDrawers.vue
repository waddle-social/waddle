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
import { normalizeMucServiceDomain } from "@/lib/calls/muc-call-indicators";
import WaddlesSidebar from "@/components/chat/WaddlesSidebar.vue";
import TopicsPanel from "@/components/chat/TopicsPanel.vue";
import DmPanel from "@/components/chat/DmPanel.vue";
import SettingsMobileHeader from "@/components/chat/SettingsMobileHeader.vue";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import { extensionRouteIconComponent, extensionRouteRailItems } from "./extension-route-rail-model";
import { isEventUpcomingOrOngoing } from "@/lib/xmpp-client";
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
  managedMucDomain?: string | null;
  selfFullJid?: string | null;
  joinChannelCall?: (channelId: string | null, roomJid: string, media: CallMedia) => void;
  leaveChannelCall?: (roomJid: string) => void;
  answerDm?: (peerJid: string, remoteFullJid: string, sid: string, media: CallMedia) => void;
  reconnectDm?: (peerJid: string, media: CallMedia) => void;
  endDm?: (peerJid: string, sid?: string) => void;
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
  groupDmConversations,
  activeChannelRoomJid,
  channelExtensionRoutes,
  activeExtensionRouteKey,
  activeRightPanel,
  selfDomain,
  openUserSettings,
  openHome,
  openUnread,
  openDmList,
  openCommunitySurface,
  handleLogout,
  handleRequestNotifications,
  handleToggleNotifications,
  handleToggleMessageSounds,
  selectChannel,
  selectChannelByRoomJid,
  selectGroupDm,
  onSelectThread,
  selectDm,
  handleNewGroupDm,
  handleAddPeopleToDm,
  selectExtensionRoute,
  openCreateChannelDialog,
  openChannelEdit,
  openThreads,
  stories,
  communityEvents,
} = props.controller;

function selectCommunitySurface(surface: "feed" | "events") {
  openCommunitySurface(surface);
}

function selectChannelFromMobile(id: string | null, roomJid?: string) {
  ui.activeCommunitySurface.value = null;
  if (id) {
    void selectChannel(id, roomJid ? { roomJid } : undefined);
    return;
  }
  if (roomJid) void selectChannelByRoomJid(roomJid);
}

function joinChannelCallFromMobile(channelId: string | null, roomJid: string, media: CallMedia) {
  props.joinChannelCall?.(channelId, roomJid, media);
}

function leaveChannelCallFromMobile(roomJid: string) {
  props.leaveChannelCall?.(roomJid);
}

function answerDmFromMobile(peerJid: string, remoteFullJid: string, sid: string, media: CallMedia) {
  props.answerDm?.(peerJid, remoteFullJid, sid, media);
}

function reconnectDmFromMobile(peerJid: string, media: CallMedia) {
  props.reconnectDm?.(peerJid, media);
}

function endDmFromMobile(peerJid: string, sid?: string) {
  props.endDm?.(peerJid, sid);
}

// XEP-0272 Muji participant counts keyed by room JID — same derived
// reactive state as ChatReadyShell uses, so the mobile sidebar shows
// the same "call ongoing" chip.
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
const visibleManagedMucDomain = computed(() =>
  props.managedMucDomain ??
  (normalizeMucServiceDomain(waddles.mucServiceJid.value) || (selfDomain.value ? `muc.${selfDomain.value}` : "")),
);
const visibleSelfFullJid = computed(() =>
  props.selfFullJid ??
  connectionStore.selfFullJid ??
  (connectionStore.client as unknown as { fullJid?: string } | null)?.fullJid ??
  null
);

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
            :active-page="ui.activePage.value"
            :has-unread-dms="dmConversations.hasUnread.value"
            :active-channel-call-count="visibleActiveChannelCallCount"
            :active-dm-call-count="visibleActiveDmCallCount"
            :session="null"
            horizontal
            @open-home="openHome"
            @toggle-channels="ui.sidebarMode.value = 'channels'"
            @toggle-dms="openDmList"
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
          :call-participant-counts="visibleCallParticipantCounts"
          :call-participants="visibleCallParticipants"
          :call-media-by-room="visibleCallMediaByRoom"
          :managed-muc-domain="visibleManagedMucDomain"
          :thread-entries-fn="(roomJid: string) => channelUnread.threadEntries(roomJid)"
          :active-community-surface="ui.activeCommunitySurface.value"
          :stories-active-count="stories.activeStories.value.length"
          :upcoming-event-count="communityEvents.events.value.filter((event) => isEventUpcomingOrOngoing(event)).length"
          :is-threads-active="ui.activePage.value === 'threads'"
          :is-unread-active="ui.activePage.value === 'unread'"
          :unread-total-count="channelUnread.totalUnreadCount.value + channelUnread.totalThreadUnreadCount.value"
          class="!w-full !border-r-0 !flex-1"
          @select-channel="selectChannelFromMobile"
          @join-channel-call="joinChannelCallFromMobile"
          @leave-channel-call="leaveChannelCallFromMobile"
          @select-thread="onSelectThread"
          @select-community-surface="selectCommunitySurface"
          @select-threads-view="openThreads"
          @select-unread-view="openUnread"
          @create-channel="openCreateChannelDialog()"
          @create-channel-in-space="openCreateChannelDialog"
          @open-settings="ui.showWaddleSettings.value = true"
          @open-members="ui.showMembers.value = true"
          @update-collapsed-group-ids="ui.collapsedSpaceGroupIds.value = $event"
        />
        <DmPanel
          v-else
          :conversations="dmConversations.conversations.value"
          :group-dms="groupDmConversations"
          :active-peer-jid="dmConversations.activePeerJid.value"
          :active-group-dm-room-jid="ui.sidebarMode.value === 'dms' && !dmConversations.activePeerJid.value ? activeChannelRoomJid : null"
          :self-full-jid="visibleSelfFullJid"
          hide-current-call
          class="!w-full !border-r-0 !flex-1"
          @answer-dm="answerDmFromMobile"
          @select-dm="selectDm"
          @select-group-dm="selectGroupDm"
          @reconnect-dm="reconnectDmFromMobile"
          @end-dm="endDmFromMobile"
          @new-dm="ui.showNewDm.value = true"
          @new-group-dm="handleNewGroupDm"
          @add-people-to-dm="handleAddPeopleToDm"
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
