import type { Ref } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { useWaddleMembers } from "@/waddles/members";
import type { ChatShellState } from "@/shell/state";
import type { connectionStore as ConnectionStore } from "@/lib/connection-store";
import type { MemberSummary } from "@/lib/chat-types";
import type { ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";

interface WorkspaceDialogsDeps {
  ui: ChatShellState;
  connectionStore: typeof ConnectionStore;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  members: ReturnType<typeof useWaddleMembers>;
  activeExtensionRouteKey: Ref<ExtensionRouteKey | null>;
}

/**
 * Workspace management dialogs: first-run setup, create/edit/delete
 * channel and Waddle flows, and member-removal confirmation.
 */
export function useWorkspaceDialogs(deps: WorkspaceDialogsDeps) {
  const { ui, connectionStore, waddles, messaging, members, activeExtensionRouteKey } = deps;

  let setupPromptShown = false;

  function showFirstRunSetupIfNeeded() {
    if (
      setupPromptShown ||
      connectionStore.appState !== "ready" ||
      ui.sidebarMode.value !== "channels" ||
      ui.activePage.value !== "chat" ||
      !waddles.canManageChannels.value ||
      !waddles.isEmptyDeployment.value ||
      waddles.isLoadingStructure.value
    ) {
      return;
    }

    setupPromptShown = true;
    ui.createChannelContextSpaceId.value = null;
    waddles.createChannelForm.value = {
      intent: "space-with-muc",
      space_name: "",
      space_description: "",
      muc_name: "general",
      muc_description: "",
      muc_type: "text",
    };
    ui.showCreateChannel.value = true;
  }

  /** Logout teardown: allow the first-run prompt again for the next account. */
  function resetSetupPrompt() {
    setupPromptShown = false;
  }

  function openCreateChannelDialog(spaceId: string | null = null) {
    ui.createChannelContextSpaceId.value = spaceId;
    waddles.prepareCreateChannelForContext(spaceId);
    ui.showCreateChannel.value = true;
  }

  async function handleCreateChannel() {
    const created = await waddles.createChannel();
    if (!created) return;
    ui.showCreateChannel.value = false;
    ui.createChannelContextSpaceId.value = null;
    if (created.intent === "space") {
      // Space-only: topology reloaded, no room to load - leave channel unselected.
      return;
    }
    // MUC was created (muc / space-muc / space-with-muc): select and load messages.
    ui.activePage.value = "chat";
    activeExtensionRouteKey.value = null;
    messaging.clearMessages();
    await messaging.loadMessages(waddles.currentChannel.value?.spaceId ?? "", created.channelId);
  }

  async function handleUpdateChannel() {
    const ok = await waddles.updateChannel();
    if (ok) ui.showEditChannel.value = false;
  }

  async function handleDeleteChannel() {
    ui.confirmDeleteChannel.value = true;
  }

  async function handleMoveChannelToSpace(targetSpaceId: string) {
    const channel = waddles.currentChannel.value;
    if (!channel) return;
    const ok = await waddles.moveChannelToSpace(channel.id, targetSpaceId);
    if (ok) ui.showEditChannel.value = false;
  }

  async function confirmDeleteChannel() {
    ui.confirmDeleteChannel.value = false;
    await waddles.deleteChannel();
    ui.showEditChannel.value = false;
    if (waddles.activeChannelId.value) {
      messaging.clearMessages();
      await messaging.loadMessages(waddles.currentChannel.value?.spaceId ?? "", waddles.activeChannelId.value);
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
    if (waddles.activeChannelId.value) {
      messaging.clearMessages();
      await messaging.loadMessages(waddles.currentChannel.value?.spaceId ?? "", waddles.activeChannelId.value);
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

  return {
    showFirstRunSetupIfNeeded,
    resetSetupPrompt,
    openCreateChannelDialog,
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
    openChannelEdit,
  };
}
