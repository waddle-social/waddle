import { type ComputedRef, nextTick, type Ref, watch } from "vue";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useDirectMessages } from "@/dms/messages";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { useXmppRosterContacts } from "@/contacts/roster";
import type { ChatShellState } from "@/shell/state";
import type { DmConversationScope, BrowserXmppClient } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp-client";
import { conversationPeerAvatarJid, isBotJid } from "@/lib/avatars/author-jid";
import { groupDmSpawnPayloadFromDm } from "@/dms/group-dm-spawn";
import type { UserSearchResult } from "@/lib/chat-types";
import type { ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";
import type { ChannelLoadIntent } from "@/channels/room-access";

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
  cancelPendingRoute: () => void;
  updateUrl: () => void;
  selectGroupDm: (roomJid: string, options?: { updateUrl?: boolean }) => Promise<boolean>;
}

/** Server-hosted bots bounce 1:1 messages and MUC PMs with service-unavailable. */
const BOT_DM_ERROR = "Bots can't receive direct messages.";

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
    cancelPendingRoute,
    updateUrl,
    selectGroupDm,
  } = deps;

  // Closing the compose dialog forgets its seed and every recipient it vetted.
  watch(() => ui.showNewDm.value, (open) => {
    if (open) return;
    ui.groupDmSeedPeerJid.value = null;
    recipientSearch = null;
  });

  async function handleOpenDm(peerJid: string, options: { intent?: ChannelLoadIntent; scope?: DmConversationScope } = {}) {
    // Refuse before touching navigation state or subscribing to the bot's
    // presence; a MUC private-message occupant JID resolves to its real JID.
    if (isBotJid(peerJid) || isBotJid(conversationPeerAvatarJid(peerJid))) {
      ui.actionError.value = BOT_DM_ERROR;
      return;
    }
    if (options.intent !== "automatic") cancelPendingRoute();
    clearPendingChannelRoomJidSelection();
    ui.activePage.value = "chat";
    ui.activeCommunitySurface.value = null;
    ui.sidebarMode.value = "dms";
    activeExtensionRouteKey.value = null;
    // A rejected target must not retain a previous account or group as send target.
    dmConversations.closeDm();
    waddles.activeChannelId.value = null;
    dmMessaging.clearMessages();
    const opening = dmConversations.openDm(peerJid, options.scope);
    const selectedPeer = dmConversations.activePeerJid.value;
    // Let the panel watcher clear the old thread; reselecting a restored peer
    // still needs a URL update even when no watched ref changes.
    if (options.intent !== "automatic") void nextTick(updateUrl);
    await opening;
    if (!selectedPeer || selectedPeer !== dmConversations.activePeerJid.value) return;
    const unreadAtLoad = dmConversations.conversations.value.find((c) => c.peerJid === selectedPeer)?.unreadCount ?? 0;
    await dmMessaging.loadMessages(selectedPeer, unreadAtLoad);
    ui.showMobileNav.value = false;
  }

  async function selectDm(peerJid: string) {
    await handleOpenDm(peerJid);
  }

  let searchRequestId = 0;
  // Every account a successful, current search returned while the compose
  // dialog is open, so people picked from an earlier query stay pickable.
  let recipientSearch: {
    client: BrowserXmppClient;
    ownerJid: string;
    results: UserSearchResult[];
  } | null = null;

  async function searchDmRecipients(input: string): Promise<UserSearchResult[]> {
    const requestId = ++searchRequestId;
    const query = input.trim().replace(/^@/, "");
    if (!query) return [];
    const client = xmppClient.value;
    const ownerJid = session.value?.jid;
    const domain = selfDomain.value.toLowerCase();
    if (!client || !ownerJid || !domain) throw new Error("Connect to search for accounts.");
    const address = query.includes("@") ? query.toLowerCase() : null;
    if (query.includes("/") || (address && !/^[^@\s]+@[^@\s]+$/.test(address))) {
      throw new Error("Enter a username or a local account address.");
    }
    if (address && address.split("@")[1] !== domain) {
      throw new Error(`Search for an account on ${domain}.`);
    }
    // XEP-0055 returns the account JID. Never reconstruct it from a display name.
    const users = await client.searchUsers(address ? jidLocalpart(address) : query);
    if (requestId !== searchRequestId || client !== xmppClient.value || ownerJid !== session.value?.jid) return [];
    const results = users.filter((user) => {
      const jid = user.jid.toLowerCase();
      return /^[^@/\s]+@[^@/\s]+$/.test(jid)
        && jid.split("@")[1] === domain
        && (!address || jid === address)
        && !client.isKnownMucRoom?.(user.jid);
    });
    const kept = recipientSearch?.client === client && recipientSearch.ownerJid === ownerJid
      ? recipientSearch.results
      : [];
    recipientSearch = { client, ownerJid, results: [...kept, ...results] };
    return results;
  }

  /** A directory result from this session or a contact; never typed text. */
  function vettedRecipientJid(peerJid: string): string | null {
    const current = recipientSearch?.client === xmppClient.value && recipientSearch?.ownerJid === session.value?.jid;
    const searched = current ? recipientSearch?.results.find((user) => user.jid === peerJid) : undefined;
    return searched?.jid ?? rosterContacts.contacts.value.find((contact) => contact.jid === peerJid)?.jid ?? null;
  }

  async function handleNewDm(peerJid: string) {
    const jid = vettedRecipientJid(peerJid);
    if (jid) await handleOpenDm(jid);
  }

  function handleAddPeopleToDm(peerJid: string) {
    if (isBotJid(peerJid)) {
      ui.actionError.value = BOT_DM_ERROR;
      return;
    }
    ui.groupDmSeedPeerJid.value = barePeerJid(peerJid);
    ui.showNewDm.value = true;
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
    if (createPayload.memberJids.some(isBotJid)) {
      ui.actionError.value = BOT_DM_ERROR;
      return;
    }
    if (payload.memberJids.some((jid) => !vettedRecipientJid(jid))) {
      ui.actionError.value = "Pick people from the directory or your contacts.";
      return;
    }
    if (createPayload.memberJids.length < 2) {
      ui.actionError.value = seedPeerJid ? "Choose at least one more contact." : "Choose at least two contacts.";
      return;
    }
    waddles.isSubmitting.value = true;
    ui.clearActionError();
    try {
      const created = await client.createGroupDm(createPayload.name, createPayload.memberJids);
      await waddles.loadStructure(null, { noChannelSelect: true });
      ui.showNewDm.value = false;
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
    searchDmRecipients,
    handleNewDm,
    handleAddPeopleToDm,
    handleCreateGroupDm,
  };
}
