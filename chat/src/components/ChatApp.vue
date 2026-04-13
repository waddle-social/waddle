<script setup lang="ts">
import { onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import { useAuth } from "@/composables/useAuth";
import { useWaddles } from "@/composables/useWaddles";
import { useMembers } from "@/composables/useMembers";
import { useMessaging } from "@/composables/useMessaging";
import { useUiState } from "@/composables/useUiState";
import { useNotifications } from "@/composables/useNotifications";
import { parseRoute, pushRoute, resolveWaddle, resolveChannel } from "@/composables/useRouting";
import { BrowserXmppClient } from "@/lib/xmpp-client";
import LandingState from "@/components/chat/LandingState.vue";
import LoginScreen from "@/components/chat/LoginScreen.vue";
import WaddlesSidebar from "@/components/chat/WaddlesSidebar.vue";
import TopicsPanel from "@/components/chat/TopicsPanel.vue";
import ContentArea from "@/components/chat/ContentArea.vue";
import MobileHeader from "@/components/chat/MobileHeader.vue";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import CreateWaddleDialog from "@/components/modals/CreateWaddleDialog.vue";
import BrowsePublicWaddlesDialog from "@/components/modals/BrowsePublicWaddlesDialog.vue";
import CreateChannelDialog from "@/components/modals/CreateChannelDialog.vue";
import WaddleSettingsDialog from "@/components/modals/WaddleSettingsDialog.vue";
import EditChannelDialog from "@/components/modals/EditChannelDialog.vue";
import MemberManagement from "@/components/modals/MemberManagement.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import type { MemberSummary } from "@/lib/waddle-api";

const props = defineProps<{ serverBaseUrl: string; tenorApiKey: string }>();

const ui = useUiState();
const auth = useAuth(props.serverBaseUrl);

const xmppClient = shallowRef<BrowserXmppClient | null>(null);

const waddles = useWaddles(
  auth.api,
  xmppClient,
  auth.session,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
);

const members = useMembers(
  auth.api,
  waddles.activeWaddleId,
  waddles.activeChannelId,
  waddles.members,
  waddles.canManageMembers,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
  waddles.loadStructure,
);

const messaging = useMessaging(
  auth.session,
  auth.api,
  xmppClient,
  waddles.activeWaddleId,
  waddles.activeChannelId,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
);

const notifications = useNotifications();

function resolveChannelNameFromJid(roomJid: string): string | null {
  const localpart = roomJid.split("@")[0] ?? "";
  const waddleId = waddles.activeWaddleId.value;
  const channelId = waddleId && localpart.startsWith(`${waddleId}_`)
    ? localpart.slice(waddleId.length + 1)
    : localpart;
  return waddles.channels.value.find((c) => c.id === channelId)?.name ?? null;
}

watch(() => messaging.lastMentionActivity.value, (event) => {
  if (!event) return;

  const channelName = resolveChannelNameFromJid(event.roomJid) ?? "unknown";
  const isBroadcast = !!event.broadcastMention;
  const isPersonalMention = event.mentions?.some(
    (m) => m === auth.session.value?.username || m.split("@")[0] === auth.session.value?.username,
  );

  if (isBroadcast || isPersonalMention) {
    notifications.showMentionNotification({
      senderNick: event.nick,
      channelName,
      body: event.body,
      roomJid: event.roomJid,
      isBroadcast,
      onNavigate: (roomJid) => {
        const localpart = roomJid.split("@")[0] ?? "";
        const waddleId = waddles.activeWaddleId.value;
        const channelId = waddleId && localpart.startsWith(`${waddleId}_`)
          ? localpart.slice(waddleId.length + 1)
          : localpart;
        void selectChannel(channelId);
      },
    });
  }

  messaging.lastMentionActivity.value = null;
});

const publicBrowseQuery = ref("");
const isApplyingRoute = ref(false);
let routeRequestId = 0;

async function setupPushSubscription() {
  if (!xmppClient.value || !auth.session.value) return;
  await notifications.syncPushSubscription(xmppClient.value, auth.session.value.jid);
}

async function handleRequestNotifications() {
  const state = await notifications.requestPermission();
  if (state === "granted") {
    await setupPushSubscription();
  }
}

function handleToggleNotifications() {
  notifications.notificationsEnabled.value = !notifications.notificationsEnabled.value;
  if (notifications.notificationsEnabled.value) {
    void setupPushSubscription();
  }
}

async function sendGif(url: string) {
  await messaging.sendMessage(url);
}

// --- Deep linking ---

function updateUrl() {
  if (isApplyingRoute.value) return;
  pushRoute(waddles.currentWaddle.value, waddles.currentChannel.value);
}

watch([waddles.activeWaddleId, waddles.activeChannelId], updateUrl);

function onPopState() {
  const route = parseRoute(window.location.pathname);
  if (!route.waddleSlug) return;

  const requestId = ++routeRequestId;
  isApplyingRoute.value = true;
  void applyRouteTarget(route, requestId).finally(() => {
    if (requestId === routeRequestId) {
      isApplyingRoute.value = false;
    }
  });
}

async function applyRouteTarget(route: ReturnType<typeof parseRoute>, requestId: number) {
  let matchedRouteWaddle = false;
  if (route.waddleSlug) {
    const w = resolveWaddle(route.waddleSlug, waddles.waddles.value);
    if (w) {
      matchedRouteWaddle = true;
      if (w.id !== waddles.activeWaddleId.value || waddles.channels.value.length === 0) {
        waddles.activeWaddleId.value = w.id;
        await waddles.loadStructure(w.id);
        if (requestId !== routeRequestId) return;
      }
    }
  }

  if (
    route.waddleSlug &&
    !matchedRouteWaddle &&
    waddles.activeWaddleId.value &&
    waddles.channels.value.length === 0
  ) {
    await waddles.loadStructure(waddles.activeWaddleId.value);
    if (requestId !== routeRequestId) return;
  }

  if (route.channelSlug && waddles.activeWaddleId.value) {
    const ch = resolveChannel(route.channelSlug, waddles.channels.value);
    const channelId = ch?.id ?? waddles.activeChannelId.value;
    if (channelId) {
      waddles.activeChannelId.value = channelId;
      messaging.clearMessages();
      await messaging.loadMessages(waddles.activeWaddleId.value, channelId);
    }
  } else if (waddles.activeWaddleId.value && waddles.activeChannelId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.activeWaddleId.value, waddles.activeChannelId.value);
  }
}

// --- Bootstrap ---

async function bootstrap() {
  await auth.bootstrap();
  if (auth.appState.value === "ready" && auth.session.value) {
    xmppClient.value = new BrowserXmppClient(auth.session.value);

    const route = parseRoute(window.location.pathname);
    const requestId = ++routeRequestId;
    isApplyingRoute.value = true;

    try {
      await waddles.loadWaddles(undefined, { loadStructure: !route.waddleSlug });
      if (requestId === routeRequestId) {
        await applyRouteTarget(route, requestId);
      }
    } finally {
      if (requestId === routeRequestId) {
        isApplyingRoute.value = false;
        updateUrl();
      }
    }

    // Register service worker and sync push subscription (best-effort, non-blocking)
    void (async () => {
      await notifications.registerServiceWorker();
      await setupPushSubscription();
    })();
  }
}

// --- Actions ---

async function handleLogout() {
  messaging.disconnect();
  await xmppClient.value?.disconnect().catch(() => undefined);
  xmppClient.value = null;
  await auth.logout();
  waddles.clearData();
  messaging.clearMessages();
  pushRoute(null, null);
}

async function selectWaddle(waddleId: string, preferredChannelId?: string | null) {
  waddles.activeWaddleId.value = waddleId;
  const channelId = await waddles.loadStructure(waddleId, preferredChannelId);
  if (channelId) {
    messaging.clearMessages();
    await messaging.loadMessages(waddleId, channelId);
  }
  ui.showMobileNav.value = false;
}

async function selectChannel(channelId: string) {
  waddles.activeChannelId.value = channelId;
  messaging.clearMessages();
  // XEP-0502: Clear activity indicator for this channel
  if (waddles.activeWaddleId.value && auth.session.value) {
    const roomJid = `${channelId}@conference.${auth.session.value.xmpp_domain}`;
    messaging.clearChannelActivity(roomJid);
  }
  if (waddles.activeWaddleId.value) {
    await messaging.loadMessages(waddles.activeWaddleId.value, channelId);
  }
  ui.showMobileNav.value = false;
}

async function handleCreateWaddle() {
  const created = await waddles.createWaddle();
  if (created) {
    ui.showCreateWaddle.value = false;
    const channelId = await waddles.loadStructure(created.id);
    if (channelId) {
      messaging.clearMessages();
      await messaging.loadMessages(created.id, channelId);
    }
  }
}

async function openBrowsePublicWaddles() {
  ui.showBrowsePublicWaddles.value = true;
  await waddles.loadPublicWaddles(publicBrowseQuery.value);
}

async function refreshPublicWaddles() {
  await waddles.loadPublicWaddles(publicBrowseQuery.value);
}

async function handleJoinPublicWaddle(waddleId: string) {
  const joined = await waddles.joinPublicWaddle(waddleId);
  if (!joined) return;

  ui.showBrowsePublicWaddles.value = false;
  if (joined.channelId) {
    messaging.clearMessages();
    await messaging.loadMessages(joined.waddleId, joined.channelId);
  }
}

async function handleCreateChannel() {
  const created = await waddles.createChannel();
  if (created) {
    ui.showCreateChannel.value = false;
    if (waddles.activeWaddleId.value) {
      messaging.clearMessages();
      await messaging.loadMessages(waddles.activeWaddleId.value, created.id);
    }
  }
}

async function handleUpdateChannel() {
  await waddles.updateChannel();
  ui.showEditChannel.value = false;
}

async function handleDeleteChannel() {
  ui.confirmDeleteChannel.value = true;
}

async function confirmDeleteChannel() {
  ui.confirmDeleteChannel.value = false;
  await waddles.deleteChannel();
  ui.showEditChannel.value = false;
  if (waddles.activeWaddleId.value && waddles.activeChannelId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.activeWaddleId.value, waddles.activeChannelId.value);
  }
}

async function handleUpdateWaddle() {
  await waddles.updateWaddle();
  ui.showWaddleSettings.value = false;
}

async function handleDeleteWaddle() {
  ui.confirmDeleteWaddle.value = true;
}

async function confirmDeleteWaddle() {
  ui.confirmDeleteWaddle.value = false;
  await waddles.deleteWaddle();
  ui.showWaddleSettings.value = false;
  if (waddles.activeWaddleId.value && waddles.activeChannelId.value) {
    messaging.clearMessages();
    await messaging.loadMessages(waddles.activeWaddleId.value, waddles.activeChannelId.value);
  }
}

let pendingRemoveMember: MemberSummary | null = null;

function handleRemoveMember(member: MemberSummary) {
  pendingRemoveMember = member;
  ui.confirmRemoveMember.value = member.username;
}

async function confirmRemoveMember() {
  if (pendingRemoveMember) {
    const member = pendingRemoveMember;
    pendingRemoveMember = null;
    ui.confirmRemoveMember.value = null;
    await members.removeMember(member);
  }
}

function openChannelEdit() {
  if (waddles.currentChannel.value) {
    ui.showEditChannel.value = true;
  }
}

onMounted(() => {
  window.addEventListener("popstate", onPopState);
  void bootstrap();
});

onUnmounted(() => {
  window.removeEventListener("popstate", onPopState);
  messaging.disconnect();
  void xmppClient.value?.disconnect().catch(() => undefined);
});
</script>

<template>
  <!-- Loading -->
  <LandingState
    v-if="auth.appState.value === 'loading'"
    title="Checking session."
  />

  <!-- Signed out -->
  <LoginScreen
    v-else-if="auth.appState.value === 'signed-out'"
    :default-server-url="props.serverBaseUrl"
    :active-server-url="auth.activeServerUrl.value"
    :providers="auth.providers.value"
    :error-message="auth.appError.value"
    @login="(url, pid) => auth.login(url, pid)"
    @fetch-providers="auth.fetchProviders"
  />

  <!-- Error -->
  <LandingState
    v-else-if="auth.appState.value === 'error'"
    title="Server unavailable."
    :copy="auth.appError.value"
    action-label="Try again"
    @action="bootstrap"
  />

  <!-- Ready -->
  <div v-else class="h-screen flex flex-col bg-background">
    <!-- Mobile header -->
    <MobileHeader
      :waddle="waddles.currentWaddle.value"
      :channel="waddles.currentChannel.value"
      :session="auth.session.value"
      @open-nav="ui.showMobileNav.value = true"
      @open-details="ui.showMobileDetails.value = true"
    />

    <!-- Mobile nav drawer -->
    <AppDrawer v-model:open="ui.showMobileNav.value" side="left">
      <template #title>
        <span class="font-mono font-bold uppercase tracking-wider">Navigation</span>
      </template>
      <div class="flex flex-col h-full">
        <div class="border-b border-foreground">
          <WaddlesSidebar
            :waddles="waddles.sortedWaddles.value"
            :active-waddle-id="waddles.activeWaddleId.value"
            :session="null"
            class="!w-full !border-r-0"
            @select-waddle="selectWaddle($event)"
            @browse-public-waddles="openBrowsePublicWaddles"
            @create-waddle="ui.showCreateWaddle.value = true"
          />
        </div>
        <TopicsPanel
          :waddle="waddles.currentWaddle.value"
          :channels="waddles.sortedChannels.value"
          :active-channel-id="waddles.activeChannelId.value"
          :can-manage-channels="waddles.canManageChannels.value"
          :can-manage-community="waddles.canManageCommunity.value"
          :is-loading="waddles.isLoadingStructure.value"
          :member-count="waddles.members.value.length"
          :active-channel-jids="messaging.activeChannels.value"
          class="!w-full !border-r-0 !flex-1"
          @select-channel="selectChannel"
          @create-channel="ui.showCreateChannel.value = true"
          @open-settings="ui.showWaddleSettings.value = true"
          @open-members="ui.showMembers.value = true"
        />
        <ProfilePanel
          v-if="auth.session.value"
          :session="auth.session.value"
          :notification-permission="notifications.permissionState.value"
          :notifications-enabled="notifications.notificationsEnabled.value"
          @logout="handleLogout"
          @request-notifications="handleRequestNotifications"
          @toggle-notifications="handleToggleNotifications"
        />
      </div>
    </AppDrawer>

    <!-- Mobile details drawer -->
    <AppDrawer v-model:open="ui.showMobileDetails.value" side="right">
      <template #title>
        <span class="font-mono font-bold uppercase tracking-wider">Details</span>
      </template>
      <div class="p-4 space-y-4">
        <div v-if="waddles.currentWaddle.value" class="space-y-2">
          <h3 class="font-mono font-bold uppercase tracking-wider">{{ waddles.currentWaddle.value.name }}</h3>
          <p v-if="waddles.currentWaddle.value.description" class="text-sm font-mono text-muted-foreground">
            {{ waddles.currentWaddle.value.description }}
          </p>
        </div>

        <div class="space-y-2">
          <button
            v-if="waddles.currentChannel.value && waddles.canManageChannels.value"
            class="w-full font-mono uppercase tracking-wider text-sm py-2 px-4 border border-foreground hover:bg-foreground hover:text-background transition-colors"
            @click="openChannelEdit(); ui.showMobileDetails.value = false"
          >
            Edit Channel
          </button>
          <button
            class="w-full font-mono uppercase tracking-wider text-sm py-2 px-4 border border-foreground hover:bg-foreground hover:text-background transition-colors"
            @click="ui.showMobileDetails.value = false; ui.showMembers.value = true"
          >
            Members ({{ waddles.members.value.length }})
          </button>
        </div>
      </div>
    </AppDrawer>

    <!-- Desktop layout -->
    <div class="flex-1 flex overflow-hidden">
      <!-- Left sidebar: waddles + profile -->
      <div class="hidden lg:flex">
          <WaddlesSidebar
            :waddles="waddles.sortedWaddles.value"
            :active-waddle-id="waddles.activeWaddleId.value"
            :session="auth.session.value"
            :notification-permission="notifications.permissionState.value"
            :notifications-enabled="notifications.notificationsEnabled.value"
            @select-waddle="selectWaddle($event)"
            @browse-public-waddles="openBrowsePublicWaddles"
            @create-waddle="ui.showCreateWaddle.value = true"
            @logout="handleLogout"
            @request-notifications="handleRequestNotifications"
            @toggle-notifications="handleToggleNotifications"
          />
        </div>

      <!-- Topics panel -->
      <div class="hidden lg:flex">
        <TopicsPanel
          :waddle="waddles.currentWaddle.value"
          :channels="waddles.sortedChannels.value"
          :active-channel-id="waddles.activeChannelId.value"
          :can-manage-channels="waddles.canManageChannels.value"
          :can-manage-community="waddles.canManageCommunity.value"
          :is-loading="waddles.isLoadingStructure.value"
          :member-count="waddles.members.value.length"
          :active-channel-jids="messaging.activeChannels.value"
          @select-channel="selectChannel"
          @create-channel="ui.showCreateChannel.value = true"
          @open-settings="ui.showWaddleSettings.value = true"
          @open-members="ui.showMembers.value = true"
        />
      </div>

      <!-- Main content -->
      <ContentArea
        v-model:draft="messaging.draft.value"
        :waddle="waddles.currentWaddle.value"
        :channel="waddles.currentChannel.value"
        :messages="messaging.messages.value"
        :xmpp-status="messaging.xmppStatus.value"
        :action-error="ui.actionError.value"
        :is-loading-messages="messaging.isLoadingMessages.value"
        :is-sending="messaging.isSending.value"
        :can-manage-channels="waddles.canManageChannels.value"
        :typing-users="messaging.typingUsers.value"
        :current-user="auth.session.value?.username"
        :tenor-api-key="props.tenorApiKey"
        :member-names="waddles.members.value.map((m) => m.username)"
        :room-hats="messaging.roomHats.value"
        :slow-mode-cooldown="messaging.slowModeCooldown.value"
        :search-results="messaging.searchResults.value"
        :is-searching="messaging.isSearching.value"
        @send="messaging.sendMessage"
        @typing="messaging.notifyComposing"
        @select-gif="sendGif"
        @edit-message="messaging.editMessage"
        @retract-message="messaging.retractMessage"
        @react-message="messaging.toggleReaction"
        @displayed="messaging.markDisplayed"
        @search="messaging.searchMessages"
        @clear-search="messaging.clearSearch"
        @edit-channel="openChannelEdit"
      />
    </div>

    <!-- Dialogs -->
    <CreateWaddleDialog
      v-model:open="ui.showCreateWaddle.value"
      :form="waddles.createWaddleForm.value"
      :is-submitting="waddles.isSubmitting.value"
      @update:form="waddles.createWaddleForm.value = $event"
      @submit="handleCreateWaddle"
    />

    <BrowsePublicWaddlesDialog
      v-model:open="ui.showBrowsePublicWaddles.value"
      :spaces="waddles.publicWaddles.value"
      :joined-waddle-ids="waddles.waddles.value.map((w) => w.id)"
      :is-loading="waddles.isLoadingPublicWaddles.value"
      :joining-waddle-id="waddles.joiningPublicWaddleId.value"
      :query="publicBrowseQuery.value"
      @update:query="publicBrowseQuery.value = $event"
      @refresh="refreshPublicWaddles"
      @join="handleJoinPublicWaddle"
    />

    <CreateChannelDialog
      v-model:open="ui.showCreateChannel.value"
      :form="waddles.createChannelForm.value"
      :is-submitting="waddles.isSubmitting.value"
      @update:form="waddles.createChannelForm.value = $event"
      @submit="handleCreateChannel"
    />

    <WaddleSettingsDialog
      v-model:open="ui.showWaddleSettings.value"
      :waddle="waddles.currentWaddle.value"
      :form="waddles.editWaddleForm.value"
      :is-submitting="waddles.isSubmitting.value"
      @update:form="waddles.editWaddleForm.value = $event"
      @save="handleUpdateWaddle"
      @delete="handleDeleteWaddle"
    />

    <EditChannelDialog
      v-model:open="ui.showEditChannel.value"
      :channel="waddles.currentChannel.value"
      :form="waddles.editChannelForm.value"
      :is-submitting="waddles.isSubmitting.value"
      @update:form="waddles.editChannelForm.value = $event"
      @save="handleUpdateChannel"
      @delete="handleDeleteChannel"
    />

    <MemberManagement
      v-model:open="ui.showMembers.value"
      :members="waddles.sortedMembers.value"
      :member-query="members.memberQuery.value"
      :new-member-role="members.newMemberRole.value"
      :search-results="members.memberSearchResults.value"
      :is-searching="members.isSearchingUsers.value"
      :can-manage-members="waddles.canManageMembers.value"
      @update:member-query="members.memberQuery.value = $event"
      @update:new-member-role="members.newMemberRole.value = $event"
      @add-member="members.addMember"
      @update-role="members.updateMemberRole"
      @remove-member="handleRemoveMember"
    />

    <!-- Confirmation dialogs -->
    <ConfirmDialog
      v-model:open="ui.confirmDeleteWaddle.value"
      title="Delete Waddle?"
      :message="`Are you sure you want to delete ${waddles.currentWaddle.value?.name ?? 'this waddle'}? All channels and messages will be permanently removed.`"
      confirm-label="Delete Waddle"
      destructive
      :loading="waddles.isSubmitting.value"
      @confirm="confirmDeleteWaddle"
    />

    <ConfirmDialog
      v-model:open="ui.confirmDeleteChannel.value"
      title="Delete Channel?"
      :message="`Are you sure you want to delete #${waddles.currentChannel.value?.name ?? 'this channel'}? All messages in this channel will be permanently removed.`"
      confirm-label="Delete Channel"
      destructive
      :loading="waddles.isSubmitting.value"
      @confirm="confirmDeleteChannel"
    />

    <ConfirmDialog
      :open="ui.confirmRemoveMember.value !== null"
      title="Remove Member?"
      :message="`Are you sure you want to remove ${ui.confirmRemoveMember.value ?? 'this member'} from the waddle?`"
      confirm-label="Remove Member"
      destructive
      @update:open="(v: boolean) => { if (!v) ui.confirmRemoveMember.value = null }"
      @confirm="confirmRemoveMember"
    />
  </div>
</template>
