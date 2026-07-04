import { type ComputedRef, type Ref, watch } from "vue";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useDirectMessages } from "@/dms/messages";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { useXmppRosterContacts } from "@/contacts/roster";
import type { ChatShellState } from "@/shell/state";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp-client";
import { groupDmSpawnPayloadFromDm } from "@/dms/group-dm-spawn";
import type { ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";

interface DmSyncDeps {
  ui: ChatShellState;
  xmppClient: ComputedRef<BrowserXmppClient | null>;
  session: ComputedRef<WaddleSession | null>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  dmMessaging: ReturnType<typeof useDirectMessages>;
  rosterContacts: ReturnType<typeof useXmppRosterContacts>;
  selfDomain: ComputedRef<string>;
  activeExtensionRouteKey: Ref<ExtensionRouteKey | null>;
  clearPendingChannelRoomJidSelection: () => void;
  selectGroupDm: (roomJid: string, options?: { updateUrl?: boolean }) => Promise<boolean>;
}

/**
 * Direct-message surface orchestration: opening 1:1 conversations (with
 * unread-aware history load) and spawning group DMs, including the
 * seeded add-people-to-DM flow.
 */
export function useDmSync(deps: DmSyncDeps) {
  const {
    ui,
    xmppClient,
    session,
    waddles,
    dmConversations,
    dmMessaging,
    rosterContacts,
    selfDomain,
    activeExtensionRouteKey,
    clearPendingChannelRoomJidSelection,
    selectGroupDm,
  } = deps;

  watch(() => ui.showNewGroupDm.value, (open) => {
    if (!open) ui.groupDmSeedPeerJid.value = null;
  });

  async function handleOpenDm(peerJid: string) {
    clearPendingChannelRoomJidSelection();
    ui.activePage.value = "chat";
    ui.sidebarMode.value = "dms";
    activeExtensionRouteKey.value = null;
    await dmConversations.openDm(peerJid);
    dmMessaging.clearMessages();
    const activePeer = dmConversations.activePeerJid.value;
    if (activePeer) {
      const unreadAtLoad = dmConversations.conversations.value.find((c) => c.peerJid === activePeer)?.unreadCount ?? 0;
      await dmMessaging.loadMessages(activePeer, unreadAtLoad);
    }
    ui.showMobileNav.value = false;
  }

  async function selectDm(peerJid: string) {
    await handleOpenDm(peerJid);
  }

  async function handleNewDm(username: string) {
    if (!selfDomain.value) return;
    await handleOpenDm(`${username}@${selfDomain.value}`);
  }

  function handleAddPeopleToDm(peerJid: string) {
    ui.groupDmSeedPeerJid.value = barePeerJid(peerJid);
    ui.showNewGroupDm.value = true;
  }

  function handleNewGroupDm() {
    ui.groupDmSeedPeerJid.value = null;
    ui.showNewGroupDm.value = true;
  }

  async function handleCreateGroupDm(payload: { name: string; memberJids: string[] }) {
    const client = xmppClient.value;
    if (!client) {
      ui.actionError.value = "XMPP session is not ready.";
      return;
    }
    const seedPeerJid = ui.groupDmSeedPeerJid.value;
    const createPayload = seedPeerJid
      ? groupDmSpawnPayloadFromDm({
          peerJid: seedPeerJid,
          selfJid: session.value?.jid ?? null,
          name: payload.name,
          selectedMemberJids: payload.memberJids,
          selectedMemberLabels: payload.memberJids.map(groupDmMemberLabel),
        })
      : {
          name: payload.name.trim() || payload.memberJids.map(groupDmMemberLabel).join(", "),
          memberJids: payload.memberJids,
        };
    if (createPayload.memberJids.length < 2) {
      ui.actionError.value = seedPeerJid ? "Choose at least one more contact." : "Choose at least two contacts.";
      return;
    }
    waddles.isSubmitting.value = true;
    ui.clearActionError();
    try {
      const created = await client.createGroupDm(createPayload.name, createPayload.memberJids);
      await waddles.loadStructure(null, { noChannelSelect: true });
      ui.showNewGroupDm.value = false;
      ui.groupDmSeedPeerJid.value = null;
      await selectGroupDm(created.roomJid);
    } catch (error) {
      ui.actionError.value = ui.normalizeError(error);
    } finally {
      waddles.isSubmitting.value = false;
    }
  }

  function groupDmMemberLabel(jid: string): string {
    const normalized = barePeerJid(jid);
    const contact = rosterContacts.contacts.value.find((candidate) => barePeerJid(candidate.jid) === normalized);
    return contact?.name?.trim() || contact?.username?.trim() || jidLocalpart(normalized) || normalized;
  }

  return {
    handleOpenDm,
    selectDm,
    handleNewDm,
    handleAddPeopleToDm,
    handleNewGroupDm,
    handleCreateGroupDm,
  };
}
