<script setup lang="ts">
import { computed } from "vue";
import CreateChannelDialog from "@/components/modals/CreateChannelDialog.vue";
import WaddleSettingsDialog from "@/components/modals/WaddleSettingsDialog.vue";
import EditChannelDialog from "@/components/modals/EditChannelDialog.vue";
import NewDmDialog from "@/components/modals/NewDmDialog.vue";
import MemberManagement from "@/components/modals/MemberManagement.vue";
import RemoteCommunitiesDialog from "@/components/modals/RemoteCommunitiesDialog.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import type { ChatAppController } from "@/shell/chat-app-controller";
import { jidLocalpart } from "@/lib/xmpp/jid";

const props = defineProps<{
  controller: ChatAppController;
}>();

const {
  ui,
  waddles,
  remoteCommunities,
  messaging,
  members,
  displayedMembers,
  inferredMemberJids,
  displayedMemberState,
  handleNewDm,
  searchDmRecipients,
  handleCreateGroupDm,
  handleCreateChannel,
  handleUpdateChannel,
  handleDeleteChannel,
  handleMoveChannelToSpace,
  confirmDeleteChannel,
  handleUpdateWaddle,
  handleDeleteWaddle,
  confirmDeleteWaddle,
  handleRemoveMember,
  confirmRemoveMember,
} = props.controller;

/** "Add people" from a 1:1 DM: that person is already in the new group. */
const groupDmSeed = computed(() => {
  const jid = ui.groupDmSeedPeerJid.value;
  if (!jid) return null;
  const contact = props.controller.rosterContacts.contacts.value.find((c) => c.jid === jid);
  const conversation = props.controller.dmConversations.conversations.value.find((c) => c.peerJid === jid);
  return { jid, label: contact?.name?.trim() || conversation?.peerUsername || jidLocalpart(jid) || jid };
});
</script>

<template>
    <!-- Dialogs -->
    <RemoteCommunitiesDialog
      v-if="waddles.federationAvailable.value"
      v-model:open="ui.showRemoteCommunities.value"
      :remote="remoteCommunities"
      :suggestions="waddles.remoteRoomSuggestions.value"
      :home-spaces="waddles.sortedSpaces.value"
    />
    <NewDmDialog
      v-model:open="ui.showNewDm.value"
      :search-recipients="searchDmRecipients"
      :contacts="controller.rosterContacts.contacts.value"
      :self-jid="controller.connectionStore.session?.jid ?? null"
      :seed="groupDmSeed"
      :is-submitting="waddles.isSubmitting.value"
      @submit="handleNewDm"
      @create-group="handleCreateGroupDm"
    />
    <CreateChannelDialog
      v-model:open="ui.showCreateChannel.value"
      :form="waddles.createChannelForm.value"
      :is-submitting="waddles.isSubmitting.value"
      :spaces="waddles.sortedSpaces.value.map((space) => ({ node: space.id, name: space.name }))"
      :default-space-node="ui.createChannelContextSpaceId.value"
      @update:form="waddles.createChannelForm.value = $event"
      @submit="handleCreateChannel"
    />

    <WaddleSettingsDialog
      v-model:open="ui.showWaddleSettings.value"
      :waddle="waddles.currentSpace.value"
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
      :spaces="waddles.sortedSpaces.value"
      :can-move-channel="waddles.canManageCurrentChannel.value"
      :federation-available="waddles.federationAvailable.value"
      @update:form="waddles.editChannelForm.value = $event"
      @save="handleUpdateChannel"
      @delete="handleDeleteChannel"
      @move-to-space="handleMoveChannelToSpace"
    />

    <MemberManagement
      v-model:open="ui.showMembers.value"
      :members="displayedMembers"
      :inferred-member-jids="inferredMemberJids"
      :member-state="displayedMemberState"
      :member-query="members.memberQuery.value"
      :new-member-affiliation="members.newMemberAffiliation.value"
      :search-results="members.memberSearchResults.value"
      :is-searching="members.isSearchingUsers.value"
      :can-manage-members="waddles.canManageCurrentMembers.value"
      :home-domain="controller.selfDomain.value"
      :room-presence="messaging.roomPresence.value"
      :room-last-seen="messaging.roomLastSeen.value"
      @update:member-query="members.memberQuery.value = $event"
      @update:new-member-affiliation="members.newMemberAffiliation.value = $event"
      @add-member="members.addMember"
      @update-affiliation="members.updateMemberAffiliation"
      @remove-member="handleRemoveMember"
    />

    <!-- Confirmation dialogs -->
    <ConfirmDialog
      v-model:open="ui.confirmDeleteWaddle.value"
      title="Delete Waddle?"
      :message="`Are you sure you want to delete ${waddles.currentSpace.value?.name ?? 'this waddle'}? All channels and messages will be permanently removed.`"
      confirm-label="Delete Waddle"
      destructive
      :loading="waddles.isSubmitting.value"
      @confirm="confirmDeleteWaddle"
    />

    <ConfirmDialog
      v-model:open="ui.confirmDeleteChannel.value"
      title="Delete channel?"
      :message="`Are you sure you want to delete #${waddles.currentChannel.value?.name ?? 'this channel'}? All messages in this channel will be permanently removed.`"
      confirm-label="Delete channel"
      destructive
      :loading="waddles.isSubmitting.value"
      @confirm="confirmDeleteChannel"
    />

    <ConfirmDialog
      :open="ui.confirmRemoveMember.value !== null"
      title="Revoke access?"
      :message="`${ui.confirmRemoveMember.value ?? 'This member'} will no longer be a member of this room. If it is members-only, they can't join again until someone adds them. This does not change their own bookmarks or the room's history.`"
      confirm-label="Revoke access"
      destructive
      @update:open="(v: boolean) => { if (!v) ui.confirmRemoveMember.value = null }"
      @confirm="confirmRemoveMember"
    />
</template>
