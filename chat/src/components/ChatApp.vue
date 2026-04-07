<script setup lang="ts">
import { onMounted, onUnmounted, watch } from "vue";
import { useAuth } from "@/composables/useAuth";
import { useWaddles } from "@/composables/useWaddles";
import { useMembers } from "@/composables/useMembers";
import { useMessaging } from "@/composables/useMessaging";
import { useUiState } from "@/composables/useUiState";
import LandingState from "@/components/chat/LandingState.vue";
import WaddlesSidebar from "@/components/chat/WaddlesSidebar.vue";
import TopicsPanel from "@/components/chat/TopicsPanel.vue";
import ContentArea from "@/components/chat/ContentArea.vue";
import MobileHeader from "@/components/chat/MobileHeader.vue";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import CreateWaddleDialog from "@/components/modals/CreateWaddleDialog.vue";
import CreateChannelDialog from "@/components/modals/CreateChannelDialog.vue";
import WaddleSettingsDialog from "@/components/modals/WaddleSettingsDialog.vue";
import EditChannelDialog from "@/components/modals/EditChannelDialog.vue";
import MemberManagement from "@/components/modals/MemberManagement.vue";

const props = defineProps<{ serverBaseUrl: string }>();

const ui = useUiState();
const auth = useAuth(props.serverBaseUrl);

const waddles = useWaddles(
  auth.api,
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
  waddles.activeWaddleId,
  waddles.activeChannelId,
  ui.normalizeError,
  ui.actionError,
  ui.clearActionError,
);

async function bootstrap() {
  await auth.bootstrap();
  if (auth.appState.value === "ready") {
    await waddles.loadWaddles();
  }
}

async function handleLogout() {
  await messaging.disconnect();
  await auth.logout();
  waddles.clearData();
  messaging.clearMessages();
}

async function selectWaddle(waddleId: string) {
  waddles.activeWaddleId.value = waddleId;
  const channelId = await waddles.loadStructure(waddleId);
  if (channelId) {
    await messaging.loadMessages(waddleId, channelId);
  }
  ui.showMobileNav.value = false;
}

async function selectChannel(channelId: string) {
  waddles.activeChannelId.value = channelId;
  messaging.clearMessages();
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
      await messaging.loadMessages(created.id, channelId);
    }
  }
}

async function handleCreateChannel() {
  const created = await waddles.createChannel();
  if (created) {
    ui.showCreateChannel.value = false;
    if (waddles.activeWaddleId.value) {
      await messaging.loadMessages(waddles.activeWaddleId.value, created.id);
    }
  }
}

async function handleUpdateChannel() {
  await waddles.updateChannel();
  ui.showEditChannel.value = false;
}

async function handleDeleteChannel() {
  if (!confirm(`Delete #${waddles.currentChannel.value?.name}?`)) return;
  await waddles.deleteChannel();
  ui.showEditChannel.value = false;
  if (waddles.activeWaddleId.value && waddles.activeChannelId.value) {
    await messaging.loadMessages(waddles.activeWaddleId.value, waddles.activeChannelId.value);
  }
}

async function handleUpdateWaddle() {
  await waddles.updateWaddle();
  ui.showWaddleSettings.value = false;
}

async function handleDeleteWaddle() {
  if (!confirm(`Delete ${waddles.currentWaddle.value?.name}?`)) return;
  await waddles.deleteWaddle();
  ui.showWaddleSettings.value = false;
  if (waddles.activeWaddleId.value && waddles.activeChannelId.value) {
    await messaging.loadMessages(waddles.activeWaddleId.value, waddles.activeChannelId.value);
  }
}

async function handleRemoveMember(member: Parameters<typeof members.removeMember>[0]) {
  if (!confirm(`Remove ${member.username}?`)) return;
  await members.removeMember(member);
}

function openChannelEdit() {
  if (waddles.currentChannel.value) {
    ui.showEditChannel.value = true;
  }
}

// Track ref for scrolling
watch(messaging.timelineEl, () => {}, { immediate: true });

onMounted(() => {
  void bootstrap();
});

onUnmounted(() => {
  void messaging.disconnect();
});
</script>

<template>
  <!-- Loading -->
  <LandingState
    v-if="auth.appState.value === 'loading'"
    title="Checking session."
  />

  <!-- Signed out -->
  <LandingState
    v-else-if="auth.appState.value === 'signed-out'"
    title="Sign in with Colony."
    copy="Rooms, membership, and live chat in one compact client."
    action-label="Log in"
    @action="auth.login"
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
            class="!w-full !border-r-0"
            @select-waddle="selectWaddle"
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
          class="!w-full !border-r-0"
          @select-channel="selectChannel"
          @create-channel="ui.showCreateChannel.value = true"
          @open-settings="ui.showWaddleSettings.value = true"
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
            @click="ui.showMobileDetails.value = false; ui.adminTab.value = 'people'"
          >
            Members ({{ waddles.members.value.length }})
          </button>
          <button
            class="w-full font-mono uppercase tracking-wider text-sm py-2 px-4 border border-foreground text-destructive hover:bg-destructive hover:text-destructive-foreground transition-colors"
            @click="handleLogout"
          >
            Log out
          </button>
        </div>
      </div>
    </AppDrawer>

    <!-- Desktop layout -->
    <div class="flex-1 flex overflow-hidden">
      <!-- Left sidebar: waddles -->
      <div class="hidden lg:flex">
        <WaddlesSidebar
          :waddles="waddles.sortedWaddles.value"
          :active-waddle-id="waddles.activeWaddleId.value"
          @select-waddle="selectWaddle"
          @create-waddle="ui.showCreateWaddle.value = true"
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
          @select-channel="selectChannel"
          @create-channel="ui.showCreateChannel.value = true"
          @open-settings="ui.showWaddleSettings.value = true"
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
        :can-manage-community="waddles.canManageCommunity.value"
        @send="messaging.sendMessage"
        @open-settings="ui.showWaddleSettings.value = true"
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
      v-model:open="ui.showMobileDetails.value"
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
  </div>
</template>
