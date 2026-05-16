<script setup lang="ts">
import CreateChannelDialog from "@/components/modals/CreateChannelDialog.vue";
import WaddleSettingsDialog from "@/components/modals/WaddleSettingsDialog.vue";
import EditChannelDialog from "@/components/modals/EditChannelDialog.vue";
import NewDmDialog from "@/components/modals/NewDmDialog.vue";
import MemberManagement from "@/components/modals/MemberManagement.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import type { ChatAppController } from "@/shell/chat-app-controller";

const props = defineProps<{
  controller: ChatAppController;
}>();

const {
  ui,
  waddles,
  messaging,
  members,
  membersWithAvatars,
  inferredMemberJids,
  displayedMemberState,
  handleNewDm,
  handleCreateChannel,
  handleUpdateChannel,
  handleDeleteChannel,
  confirmDeleteChannel,
  handleUpdateWaddle,
  handleDeleteWaddle,
  confirmDeleteWaddle,
  handleRemoveMember,
  confirmRemoveMember,
} = props.controller;
</script>

<template>
    <!-- Dialogs -->
    <NewDmDialog
      v-model:open="ui.showNewDm.value"
      @submit="handleNewDm"
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
      @update:form="waddles.editChannelForm.value = $event"
      @save="handleUpdateChannel"
      @delete="handleDeleteChannel"
    />

    <MemberManagement
      v-model:open="ui.showMembers.value"
      :members="membersWithAvatars"
      :inferred-member-jids="inferredMemberJids"
      :member-state="displayedMemberState"
      :member-query="members.memberQuery.value"
      :new-member-affiliation="members.newMemberAffiliation.value"
      :search-results="members.memberSearchResults.value"
      :is-searching="members.isSearchingUsers.value"
      :can-manage-members="waddles.canManageMembers.value"
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
      title="Remove member?"
      :message="`Are you sure you want to remove ${ui.confirmRemoveMember.value ?? 'this member'} from the waddle?`"
      confirm-label="Remove member"
      destructive
      @update:open="(v: boolean) => { if (!v) ui.confirmRemoveMember.value = null }"
      @confirm="confirmRemoveMember"
    />
</template>
