<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $mucCallParticipants,
  mucCallParticipantCounts,
} from "@/lib/calls/muc-call-presence";
import HomeDashboard from "@/components/chat/HomeDashboard.vue";
import ThreadsView from "@/components/chat/ThreadsView.vue";
import FeedPane from "@/components/community/FeedPane.vue";
import StoriesPane from "@/components/community/StoriesPane.vue";
import EventsPane from "@/components/community/EventsPane.vue";
import ChatAppModals from "@/components/chat/ChatAppModals.vue";
import ChatMobileDrawers from "@/components/chat/ChatMobileDrawers.vue";
import CallReturnPill from "@/components/calls/CallReturnPill.vue";
import ContentArea from "@/components/chat/ContentArea.vue";
import DmPanel from "@/components/chat/DmPanel.vue";
import ExtensionRouteRail from "@/components/chat/ExtensionRouteRail.vue";
import ExtensionRouteView from "@/components/chat/ExtensionRouteView.vue";
import ThreadPanel from "@/components/chat/ThreadPanel.vue";
import TopicsPanel from "@/components/chat/TopicsPanel.vue";
import PinnedPanel from "@/components/chat/PinnedPanel.vue";
import UserSettingsPage from "@/components/chat/UserSettingsPage.vue";
import WaddlesSidebar from "@/components/chat/WaddlesSidebar.vue";
import AdminView from "@/components/admin/AdminView.vue";
import { navigate, useRouteMatch, type AdminMatch, type AdminPanel } from "@/router";
import { buildHomeDashboardProps } from "@/home/dashboard-props";
import type { ChatAppController } from "@/shell/chat-app-controller";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import { installMessageToolbarLifecycleSuppression } from "@/stores/message-toolbar";

const props = defineProps<{
  controller: ChatAppController;
}>();

const controller = props.controller;

const {
  connectionStore,
  giphyApiKey,
  ui,
  waddles,
  messaging,
  dmConversations,
  channelUnread,
  rosterContacts,
  socialFeed,
  stories,
  communityEvents,
  xmppClient,
  activeMessages,
  activeFirstUnseenId,
  channelExtensionRoutes,
  activeExtensionRouteKey,
  activeExtensionRoute,
  activeChannelRoomJid,
  activeThreadStack,
  activeThreadTargetMessageId,
  activeRightPanel,
  threads,
  reactionModeTarget,
  reactionModeState,
  activeDraft,
  activeForumTitle,
  activeTypingUsers,
  contentAreaIsLoadingMessages,
  activeIsLoadingOlderMessages,
  activeHasOlderMessages,
  activeIsSending,
  activeSearchResults,
  activeIsSearching,
  selfDomain,
  authorJidByNick,
  mentionCandidates,
  displayedMemberCount,
  displayedMemberState,
  activeDmPeer,
  computedChannelUnreadMap,
  notifications,
  appUpdate,
  version,
  avatarUrlByAuthor,
  authorHatsByNick,
  authorAuthorityByNick,
  activeActionError,
  activeErrorActionLabel,
  activeUploadProgress,
  setContentAreaRef,
  getThreadLabel,
  refreshAppUpdate,
  handleRequestNotifications,
  handleToggleNotifications,
  openUserSettings,
  openHome,
  openDmList,
  openThreads,
  openCommunitySurface,
  closeUserSettings,
  handleLogout,
  selectChannel,
  onSelectThread,
  selectExtensionRoute,
  handleOpenDm,
  selectDm,
  openCreateChannelDialog,
  openChannelEdit,
  sendActiveMessage,
  sendPublicChannelMessage,
  sendThreadMessage,
  sendGif,
  openThread,
  pushThread,
  popThreadTo,
  closeThreadPanel,
  activateRightPanel,
  closeExtensionRoutePanel,
  closePinnedPanel,
  notifyActiveComposing,
  editActiveMessage,
  retractActiveMessage,
  reactActiveMessage,
  markActiveDisplayed,
  invokeActiveExtensionAction,
  invokeExtensionRouteAction,
  searchActiveMessages,
  clearActiveSearch,
  loadOlderActiveMessages,
  retryActiveLoad,
  ensureActiveMessageLoaded,
  loadOlderThreadMessages,
  pinActiveMessage,
  unpinActiveMessage,
  jumpToPinnedMessage,
} = props.controller;

const homeDashboardProps = computed(() => buildHomeDashboardProps({
  spaces: waddles.sortedSpaces.value,
  channels: waddles.sortedChannels.value,
  contacts: rosterContacts.contacts.value,
  isLoading: waddles.isLoadingStructure.value || rosterContacts.isLoadingContacts.value,
  channelUnreadMap: computedChannelUnreadMap.value,
  mentionedRoomJids: messaging.mentionedChannelCounts.value,
  activeChannelJids: messaging.activeChannels.value,
  dmConversations: dmConversations.conversations.value,
}));

/**
 * XEP-0272 Muji participant counts keyed by room JID. Derived from
 * the per-room nick list so the sidebar badge updates as occupants
 * join and leave the call without any extra round-trip. Computed
 * once here and threaded through TopicsPanel so per-row lookups
 * stay O(1).
 */
const mucCallParticipantsStore = useStore($mucCallParticipants);
const callParticipantCounts = computed<Record<string, number>>(() => {
  return mucCallParticipantCounts(mucCallParticipantsStore.value);
});

/**
 * Bare room JID of whatever channel the user is currently looking
 * at, or `null` when the active surface isn't a channel chat
 * (dashboard, settings, threads, DMs, community feeds, admin).
 * Used by `CallReturnPill` to decide whether the local user is
 * already inside the room that owns their active call.
 */
const currentlyViewedRoomJid = computed<string | null>(() => {
  if (ui.activePage.value !== "chat") return null;
  if (ui.sidebarMode.value !== "channels") return null;
  if (ui.activeCommunitySurface.value !== null) return null;
  return activeChannelRoomJid.value ?? null;
});

const contentPaneClass = computed(() => {
  if (ui.sidebarMode.value !== "channels") return "";
  if (activeRightPanel.value === "thread") {
    if (activeThreadStack.value.length === 0) return "";
    return activeThreadStack.value.length === 1
      ? "chat-content-pane--desktop-split"
      : "chat-content-pane--hidden";
  }
  return activeRightPanel.value ? "chat-content-pane--desktop-split" : "";
});

function onCommunityRsvp(
  event: { uid: string },
  partstat: "ACCEPTED" | "DECLINED" | "TENTATIVE" | "NEEDS-ACTION",
) {
  const session = connectionStore.session;
  if (!session) return;
  const bareJid = session.jid.split("/")[0] ?? session.jid;
  const localpart = bareJid.split("@")[0];
  if (!localpart) return;
  void communityEvents.rsvp(event.uid, localpart, bareJid, partstat);
}

function selectCurrentChannelExtensionRoute(route: DiscoveredExtensionRoute) {
  const channel = waddles.currentChannel.value;
  if (!channel) return;
  void selectExtensionRoute(channel.id, route);
}

function setPinnedPanelOpen(isOpen: boolean) {
  if (!isOpen) {
    closePinnedPanel();
    return;
  }
  ui.showPinnedPanel.value = true;
  activateRightPanel("pinned");
}

function onSelectCommunitySurface(surface: "feed" | "stories" | "events") {
  openCommunitySurface(surface);
}

function onSelectChannelFromSidebar(id: string) {
  ui.activeCommunitySurface.value = null;
  selectChannel(id);
}

// ── Admin route plumbing ────────────────────────────────────────────
// The admin view is rendered straight out of ChatReadyShell rather
// than from a per-route page Vue island, so the panel slug is
// derived directly from the reactive route match. `navigate()`
// updates the match synchronously, so panel switches re-render this
// tick without any manual pushState / popstate plumbing.
const routeMatch = useRouteMatch();
const adminPanelFromUrl = computed<AdminPanel>(() =>
  routeMatch.value.id === "admin" ? routeMatch.value.params.panel : "users",
);
function onAdminNavigate(match: AdminMatch) {
  navigate(match);
}
function onAdminBack() {
  openHome();
}
let disconnectMessageToolbarLifecycle: (() => void) | null = null;

onMounted(() => {
  disconnectMessageToolbarLifecycle = installMessageToolbarLifecycleSuppression();
});

onUnmounted(() => {
  disconnectMessageToolbarLifecycle?.();
  disconnectMessageToolbarLifecycle = null;
});
</script>

<template>
  <!--
    Cross-surface "you're in a call" return pill. Mounted at the
    template root so it floats above whatever page is active —
    admin, dashboard, threads, settings, DMs, and channels that
    don't own the call all surface it. The pill is `position:
    fixed` so it has no layout impact on the underlying surfaces.
  -->
  <CallReturnPill
    :viewed-room-jid="currentlyViewedRoomJid"
    :channels="waddles.sortedChannels.value"
    :on-navigate="(channelId) => { void selectChannel(channelId); }"
  />
  <AdminView
    v-if="ui.activePage.value === 'admin'"
    :xmpp-client="xmppClient"
    :active-panel="adminPanelFromUrl"
    @navigate="onAdminNavigate"
    @back="onAdminBack"
  />
  <div v-else class="chat-app-shell">
    <ChatMobileDrawers :controller="controller" />

    <!-- Desktop layout -->
    <div class="chat-desktop-shell">
      <!-- Icon rail: waddle switcher -->
      <div class="chat-desktop-rail-slot">
        <WaddlesSidebar
          :waddles="[]"
          :active-space-id="null"
          :active-sidebar-mode="ui.sidebarMode.value"
          :active-page="ui.activePage.value"
          :has-unread-dms="dmConversations.hasUnread.value"
          :session="connectionStore.session"
          :notification-permission="notifications.permissionState.value"
          :notifications-enabled="notifications.notificationsEnabled.value"
          :total-unread-count="channelUnread.totalUnreadCount.value"
          :total-mention-count="channelUnread.totalMentionCount.value"
          :web-commit-sha="version.webCommitSha.value"
          :server-version="version.serverVersion.value"
          @open-home="openHome"
          @open-settings="openUserSettings"
          @toggle-channels="openHome"
          @toggle-dms="openDmList"
          @logout="handleLogout"
          @request-notifications="handleRequestNotifications"
          @toggle-notifications="handleToggleNotifications"
        />
      </div>

      <!-- Channel sidebar -->
      <div class="chat-sidebar-slot">
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
          :call-participant-counts="callParticipantCounts"
          :thread-entries-fn="(roomJid: string) => channelUnread.threadEntries(roomJid)"
          :active-community-surface="ui.activeCommunitySurface.value"
          :stories-active-count="stories.activeStories.value.length"
          :upcoming-event-count="communityEvents.events.value.filter((e: { dtstartMs?: number }) => typeof e.dtstartMs === 'number' && e.dtstartMs > Date.now()).length"
          :is-threads-active="ui.activePage.value === 'threads'"
          @select-channel="onSelectChannelFromSidebar"
          @select-thread="onSelectThread"
          @select-community-surface="onSelectCommunitySurface"
          @select-threads-view="openThreads"
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
          @select-dm="selectDm"
          @new-dm="ui.showNewDm.value = true"
        />
      </div>

      <!-- Main content -->
      <HomeDashboard
        v-if="ui.activePage.value === 'dashboard'"
        v-bind="homeDashboardProps"
        @select-channel="selectChannel"
        @select-contact="handleOpenDm"
        @open-nav="ui.showMobileNav.value = true"
      />
      <FeedPane
        v-else-if="ui.activeCommunitySurface.value === 'feed'"
        :entries="socialFeed.entries.value"
        :is-loading="socialFeed.isLoading.value"
        :is-posting="socialFeed.isPosting.value"
        :error="socialFeed.error.value"
        :can-post="!!connectionStore.session"
        :self-jid="connectionStore.session?.jid ?? null"
        @refresh="socialFeed.refresh()"
        @post="(input) => socialFeed.post(input)"
        @open-nav="ui.showMobileNav.value = true"
      />
      <StoriesPane
        v-else-if="ui.activeCommunitySurface.value === 'stories'"
        :stories="stories.activeStories.value"
        :is-loading="stories.isLoading.value"
        :is-posting="stories.isPosting.value"
        :error="stories.error.value"
        :can-post="!!connectionStore.session"
        :self-jid="connectionStore.session?.jid ?? null"
        :is-story-read="stories.isStoryRead"
        @refresh="stories.refresh()"
        @post="(input) => stories.post(input)"
        @story-selected="(id) => stories.markStoryRead(id)"
        @open-nav="ui.showMobileNav.value = true"
      />
      <EventsPane
        v-else-if="ui.activeCommunitySurface.value === 'events'"
        :events="communityEvents.events.value"
        :is-loading="communityEvents.isLoading.value"
        :is-posting="communityEvents.isPosting.value"
        :error="communityEvents.error.value"
        :can-post="!!connectionStore.session"
        :self-jid="connectionStore.session?.jid ?? null"
        :find-master="communityEvents.findMaster"
        @refresh="communityEvents.refresh()"
        @post="(input) => communityEvents.post(input)"
        @edit="(id, input) => communityEvents.edit(id, input)"
        @cancel-series="(id) => communityEvents.cancel(id)"
        @cancel-instance="(uid, dtstartMs) => communityEvents.cancelInstance(uid, dtstartMs)"
        @rsvp="(event, partstat) => onCommunityRsvp(event, partstat)"
        @open-nav="ui.showMobileNav.value = true"
      />
      <ThreadsView
        v-else-if="ui.activePage.value === 'threads'"
        :channels="waddles.sortedChannels.value"
        :on-select-thread="onSelectThread"
        @open-nav="ui.showMobileNav.value = true"
      />
      <UserSettingsPage
        v-else-if="ui.activePage.value === 'settings' && connectionStore.session"
        :session="connectionStore.session"
        :xmpp-client="xmppClient"
        :web-commit-sha="version.webCommitSha.value"
        :server-version="version.serverVersion.value"
        @close="closeUserSettings"
      />
      <template v-else>
        <!--
          Accordion thread layout
          ---------------------------------------------------------------------
          Stack depth 0  -> ContentArea takes full remaining width
          Stack depth 1  -> ContentArea (desktop left pane) + active ThreadPanel
          Stack depth 2+ -> parent ThreadPanel (desktop, read-only) + active ThreadPanel

          On mobile only the active thread panel is visible when a thread is open.
          Escape key (handled above) pops back one level in the stack.
        -->

        <!-- ContentArea wrapper:
             - depth 0: visible and flex-1 (full remaining width)
             - depth 1: hidden on mobile, flex-1 on desktop (left pane)
             - depth 2+: hidden entirely (parent thread pane takes its place) -->
        <div
          class="chat-workspace"
          :class="activeRightPanel ? 'chat-workspace--right-panel-active' : ''"
        >
          <div
            :class="[
              'chat-content-pane',
              contentPaneClass,
            ]"
          >
            <ContentArea
              :ref="setContentAreaRef"
              v-model:draft="activeDraft"
              v-model:forum-title="activeForumTitle"
              :pinned-panel-open="activeRightPanel === 'pinned' && ui.showPinnedPanel.value"
              :waddle="waddles.currentSpace.value"
              :channel="ui.sidebarMode.value === 'dms' ? null : waddles.currentChannel.value"
              :room-jid="ui.sidebarMode.value === 'dms' ? null : activeChannelRoomJid"
              :dm-peer="activeDmPeer"
              :sidebar-mode="ui.sidebarMode.value"
              :messages="activeMessages"
              :first-unseen-id="activeFirstUnseenId"
              :xmpp-status="messaging.xmppStatus.value"
              :action-error="activeActionError"
              :error-action-label="activeErrorActionLabel"
              :update-available="appUpdate.updateAvailable.value"
              :is-applying-update="appUpdate.isApplyingUpdate.value"
              :is-loading-messages="contentAreaIsLoadingMessages"
              :is-loading-older-messages="activeIsLoadingOlderMessages"
              :has-older-messages="activeHasOlderMessages"
              :is-sending="activeIsSending"
              :can-manage-channels="waddles.canManageChannels.value"
              :member-count="displayedMemberCount"
              :member-state="displayedMemberState"
              :typing-users="activeTypingUsers"
              :current-user="connectionStore.session?.username"
              :current-user-jid="connectionStore.session?.jid"
              :self-domain="selfDomain"
              :avatar-url-by-author="avatarUrlByAuthor"
              :author-jid-by-nick="authorJidByNick"
              :giphy-api-key="giphyApiKey"
              :mention-candidates="mentionCandidates"
              :room-hats="authorHatsByNick"
              :room-authority="authorAuthorityByNick"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :search-results="activeSearchResults"
              :is-searching="activeIsSearching"
              :upload-progress="activeUploadProgress"
              :thread-index="threads.index.value"
              :xmpp-client="xmppClient"
              :reaction-mode="reactionModeTarget === 'main' ? reactionModeState : null"
              :ensure-message-loaded="ensureActiveMessageLoaded"
              :send-public-channel-message="sendPublicChannelMessage"
              @send="sendActiveMessage"
              @typing="notifyActiveComposing"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              @pin-message="pinActiveMessage"
              @unpin-message="unpinActiveMessage"
              @update:pinned-panel-open="setPinnedPanelOpen"
              @search="searchActiveMessages"
              @clear-search="clearActiveSearch"
              @load-older="loadOlderActiveMessages"
              @retry-load="retryActiveLoad"
              @edit-channel="openChannelEdit"
              @open-nav="ui.showMobileNav.value = true"
              @open-details="ui.showMobileDetails.value = true"
              @open-dm="handleOpenDm"
              @open-thread="openThread"
              :invoke-extension-action="invokeActiveExtensionAction"
              @refresh-update="refreshAppUpdate"
            />
          </div>

          <!-- Collapsed accordion bars: one per inactive right-side panel. -->
          <button
            v-if="ui.sidebarMode.value === 'channels'
              && activeRightPanel !== 'thread'
              && activeThreadStack.length >= 1"
            type="button"
            class="chat-accordion-bar bg-muted/20 hover:bg-muted/50"
            :title="getThreadLabel(activeThreadStack[activeThreadStack.length - 1] ?? '')"
            @click="activateRightPanel('thread')"
          >
            <span class="accordion-bar-label text-muted-foreground/70">
              {{ getThreadLabel(activeThreadStack[activeThreadStack.length - 1] ?? '') }}
            </span>
          </button>

          <template v-if="ui.sidebarMode.value === 'channels' && activeRightPanel === 'thread' && activeThreadStack.length >= 2">
            <!-- Channel / main-feed bar -->
            <button
              type="button"
              class="chat-accordion-bar bg-muted/30 hover:bg-muted/60"
              title="Back to channel"
              @click="closeThreadPanel"
            >
              <span class="accordion-bar-label text-muted-foreground/80">
                {{ waddles.currentChannel.value?.name ?? 'Channel' }}
              </span>
            </button>
            <!-- Ancestor thread bars: levels older than the parent (skip last 2 = parent + active) -->
            <button
              v-for="(threadId, i) in activeThreadStack.slice(0, -2)"
              :key="threadId"
              type="button"
              class="chat-accordion-bar bg-muted/20 hover:bg-muted/50"
              :title="getThreadLabel(threadId)"
              @click="popThreadTo(i)"
            >
              <span class="accordion-bar-label text-muted-foreground/60">
                {{ getThreadLabel(threadId) }}
              </span>
            </button>
          </template>

          <button
            v-if="ui.sidebarMode.value === 'channels'
              && activeRightPanel !== 'pinned'
              && ui.showPinnedPanel.value
              && waddles.currentChannel.value
              && activeChannelRoomJid"
            type="button"
            class="chat-accordion-bar bg-muted/20 hover:bg-muted/50"
            title="Pinned messages"
            @click="activateRightPanel('pinned')"
          >
            <span class="accordion-bar-label text-muted-foreground/70">
              Pinned
            </span>
          </button>

          <button
            v-if="ui.sidebarMode.value === 'channels'
              && activeRightPanel !== 'extension'
              && activeExtensionRouteKey"
            type="button"
            class="chat-accordion-bar bg-muted/20 hover:bg-muted/50"
            :title="activeExtensionRoute?.label ?? 'Extensions'"
            @click="activateRightPanel('extension')"
          >
            <span class="accordion-bar-label text-muted-foreground/70">
              {{ activeExtensionRoute?.label ?? 'Extensions' }}
            </span>
          </button>

          <!-- Parent thread pane: desktop-only context when depth >= 2.
               Shows the second-to-last thread in read-only mode (no composer). -->
          <div
            v-if="ui.sidebarMode.value === 'channels' && activeRightPanel === 'thread' && activeThreadStack.length >= 2"
            class="chat-parent-thread-pane"
          >
            <ThreadPanel
              :thread-stack="activeThreadStack.slice(0, -1)"
              :thread-index="threads.index.value"
              :resolve-entry="threads.resolveEntry"
              :current-user="connectionStore.session?.username"
              :current-user-jid="connectionStore.session?.jid"
              :avatar-url-by-author="avatarUrlByAuthor"
              :author-jid-by-nick="authorJidByNick"
              :room-hats="authorHatsByNick"
              :room-authority="authorAuthorityByNick"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :giphy-api-key="giphyApiKey"
              :mention-candidates="mentionCandidates"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :is-sending="false"
              :is-loading-older-replies="messaging.loadingOlderThreadIds.value.has(activeThreadStack[activeThreadStack.length - 2] ?? '')"
              :has-older-replies="messaging.threadHasOlder.value[activeThreadStack[activeThreadStack.length - 2] ?? ''] ?? false"
              :upload-progress="{ uploading: false, progress: 0, filename: '' }"
              :channel-name="waddles.currentChannel.value?.name ?? ''"
              :hide-composer="true"
              :reaction-mode="null"
              @close="closeThreadPanel"
              @pop-to="popThreadTo"
              @push-thread="pushThread"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              :invoke-extension-action="invokeActiveExtensionAction"
              @displayed="markActiveDisplayed"
              @load-older="loadOlderThreadMessages"
            />
          </div>

          <!-- Active thread pane: shown when any thread is open.
               Full-width on mobile; shares space with parent context on desktop. -->
          <div
            v-if="ui.sidebarMode.value === 'channels' && activeRightPanel === 'thread' && activeThreadStack.length >= 1"
            class="chat-active-thread-pane"
          >
            <ThreadPanel
              :thread-stack="activeThreadStack"
              :thread-index="threads.index.value"
              :resolve-entry="threads.resolveEntry"
              :current-user="connectionStore.session?.username"
              :current-user-jid="connectionStore.session?.jid"
              :avatar-url-by-author="avatarUrlByAuthor"
              :author-jid-by-nick="authorJidByNick"
              :room-hats="authorHatsByNick"
              :room-authority="authorAuthorityByNick"
              :room-presence="messaging.roomPresence.value"
              :room-last-seen="messaging.roomLastSeen.value"
              :giphy-api-key="giphyApiKey"
              :mention-candidates="mentionCandidates"
              :slow-mode-cooldown="messaging.slowModeCooldown.value"
              :is-sending="messaging.isSending.value"
              :is-loading-older-replies="messaging.loadingOlderThreadIds.value.has(activeThreadStack[activeThreadStack.length - 1] ?? '')"
              :has-older-replies="messaging.threadHasOlder.value[activeThreadStack[activeThreadStack.length - 1] ?? ''] ?? false"
              :upload-progress="messaging.uploadProgress.value"
              :channel-name="waddles.currentChannel.value?.name ?? ''"
              :reaction-mode="reactionModeTarget === 'thread' ? reactionModeState : null"
              :target-message-id="activeThreadTargetMessageId"
              @close="closeThreadPanel"
              @pop-to="popThreadTo"
              @push-thread="pushThread"
              @send="sendThreadMessage"
              @edit-message="editActiveMessage"
              @retract-message="retractActiveMessage"
              @react-message="reactActiveMessage"
              :invoke-extension-action="invokeActiveExtensionAction"
              @displayed="markActiveDisplayed"
              @select-gif="sendGif"
              @typing="notifyActiveComposing"
              @load-older="loadOlderThreadMessages"
            />
          </div>

          <div
            v-if="ui.sidebarMode.value === 'channels'
              && activeRightPanel === 'pinned'
              && ui.showPinnedPanel.value
              && waddles.currentChannel.value
              && activeChannelRoomJid"
            class="chat-active-thread-pane"
          >
            <PinnedPanel
              :room-jid="activeChannelRoomJid"
              :channel-name="waddles.currentChannel.value?.name ?? ''"
              :timeline-messages="activeMessages"
              @close="closePinnedPanel"
              @jump-to-message="(stanzaId: string) => jumpToPinnedMessage(stanzaId)"
            />
          </div>

          <div
            v-if="ui.sidebarMode.value === 'channels'
              && activeRightPanel === 'extension'
              && activeExtensionRouteKey"
            class="chat-active-thread-pane"
          >
            <ExtensionRouteView
              :waddle="waddles.currentSpace.value"
              :channel="waddles.currentChannel.value"
              :route="activeExtensionRoute"
              :requested-route="activeExtensionRouteKey"
              :room-jid="activeChannelRoomJid"
              :xmpp-client="xmppClient"
              :action-error="activeActionError"
              @open-nav="ui.showMobileNav.value = true"
              @close="closeExtensionRoutePanel"
              @invoke-action="invokeExtensionRouteAction"
            />
          </div>

          <ExtensionRouteRail
            v-if="ui.sidebarMode.value === 'channels' && waddles.currentChannel.value"
            :routes="channelExtensionRoutes"
            :active-route="activeExtensionRouteKey"
            :active="activeRightPanel === 'extension'"
            @select-route="selectCurrentChannelExtensionRoute"
          />
        </div>
      </template>
    </div>

    <ChatAppModals :controller="controller" />
  </div>
</template>
