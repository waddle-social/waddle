import { describe, expect, mock, test } from "bun:test";
import { computed, effectScope, nextTick, ref } from "vue";
import { useChatShellState } from "../src/shell/state";
import { useDmSync } from "../src/shell/controllers/use-dm-sync";
import type { WaddleSession } from "../src/lib/server-auth";
import type { useWaddleDirectory } from "../src/waddles/directory";
import type { useDirectMessageConversations } from "../src/dms/conversations";
import type { useDirectMessages } from "../src/dms/messages";
import type { useXmppRosterContacts } from "../src/contacts/roster";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { ChannelSummary, UserSearchResult } from "../src/lib/chat-types";
import type { DmConversation } from "../src/lib/xmpp-client";
import type { ExtensionRouteKey } from "../src/shell/controllers/use-extension-routes";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com",
    session_id: "s1",
    user_id: "u1",
    avatar_url: null,
    xmpp_localpart: "alice",
    xmpp_websocket_url: "wss://example.com/xmpp",
    is_expired: false,
    expires_at: null,
  } as WaddleSession;
}

function makeHarness() {
  const ui = useChatShellState();
  const searchUsers = mock(async (_query: string): Promise<UserSearchResult[]> => []);
  const client = { searchUsers } as unknown as BrowserXmppClient;
  const currentClient = ref<BrowserXmppClient | null>(client);

  const channels = ref<ChannelSummary[]>([
    { id: "general", name: "General", spaceId: "space-1" },
    { id: "chat", name: "Chat", spaceId: "space-1", jid: "chat@muc.example.com" },
  ]);
  const conversations = ref<DmConversation[]>([]);
  const activePeerJid = ref<string | null>(null);
  const openDm = mock(async (peerJid: string) => {
    conversations.value = [{
      peerJid,
      peerUsername: peerJid.split("@")[0] ?? peerJid,
      unreadCount: 0,
    }];
    activePeerJid.value = peerJid;
  });
  const forgetPeer = mock((peerJid: string) => {
    conversations.value = conversations.value.filter((conversation) => conversation.peerJid !== peerJid);
    if (activePeerJid.value === peerJid) activePeerJid.value = null;
  });
  const loadMessages = mock(async () => {});
  const clearMessages = mock(() => {});
  const selectChannel = mock(async () => {});
  const selectGroupDm = mock(async () => true);
  const clearPendingChannelRoomJidSelection = mock(() => {});
  const cancelPendingRoute = mock(() => {});

  const waddles = {
    channels,
    activeChannelId: ref(null),
    isSubmitting: ref(false),
    loadStructure: mock(async () => null),
  } as unknown as ReturnType<typeof useWaddleDirectory>;
  const dmConversations = {
    conversations,
    activePeerJid,
    closeDm: () => { activePeerJid.value = null; },
    openDm,
    forgetPeer,
  } as unknown as ReturnType<typeof useDirectMessageConversations>;
  const dmMessaging = {
    clearMessages,
    loadMessages,
  } as unknown as ReturnType<typeof useDirectMessages>;
  const rosterContacts = {
    contacts: ref([]),
  } as unknown as ReturnType<typeof useXmppRosterContacts>;

  const scope = effectScope();
  const dmSync = scope.run(() =>
    useDmSync({
      ui,
      xmppClient: computed(() => currentClient.value),
      session: computed(() => session()),
      waddles,
      dmConversations,
      dmMessaging,
      rosterContacts,
      selfDomain: computed(() => "example.com"),
      activeExtensionRouteKey: ref<ExtensionRouteKey | null>(null),
      clearPendingChannelRoomJidSelection,
      cancelPendingRoute,
      updateUrl: () => {},
      selectGroupDm,
    }),
  )!;

  return {
    scope,
    searchUsers,
    currentClient,
    dmSync,
    conversations,
    channels,
    openDm,
    forgetPeer,
    selectChannel,
    selectGroupDm,
    loadMessages,
    cancelPendingRoute,
  };
}

describe("useDmSync handleOpenDm / handleNewDm", () => {
  test("opening a full user-domain JID stays a 1:1 even when the node matches a channel id", async () => {
    const h = makeHarness();
    await h.dmSync.handleOpenDm("general@example.com");
    expect(h.selectChannel).not.toHaveBeenCalled();
    expect(h.selectGroupDm).not.toHaveBeenCalled();
    expect(h.openDm).toHaveBeenCalledWith("general@example.com", undefined);
    expect(h.loadMessages).toHaveBeenCalledWith("general@example.com", 0);
    expect(h.cancelPendingRoute).toHaveBeenCalledTimes(1);
    h.scope.stop();
  });

  test.each(["chat", "@chat", "chat@example.com", "Chat@EXAMPLE.COM"])("New DM resolves %s to a selected directory JID", async (query) => {
    const h = makeHarness();
    h.searchUsers.mockResolvedValue([
      { id: "chat@example.com", jid: "chat@example.com", username: "display-chat", display_name: null, avatar_url: null },
    ]);
    const results = await h.dmSync.searchDmRecipients(query);
    await h.dmSync.handleNewDm(results[0]!.jid);
    expect(h.searchUsers).toHaveBeenCalledWith("chat");
    expect(h.openDm).toHaveBeenCalledWith("chat@example.com", undefined);
    expect(h.selectChannel).not.toHaveBeenCalled();
    expect(h.selectGroupDm).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("unchecked text and empty directory results never create a conversation", async () => {
    const h = makeHarness();
    await h.dmSync.handleNewDm("chat");
    await h.dmSync.handleNewDm("chat@example.com");
    expect(await h.dmSync.searchDmRecipients("missing")).toEqual([]);
    await h.dmSync.handleNewDm("missing@example.com");
    expect(h.openDm).not.toHaveBeenCalled();
    expect(h.conversations.value).toEqual([]);
    h.scope.stop();
  });

  test("full addresses require an exact local account result", async () => {
    const h = makeHarness();
    h.searchUsers.mockResolvedValue([
      { id: "chatty@example.com", jid: "chatty@example.com", username: "chatty", display_name: null, avatar_url: null },
      { id: "chat@muc.example.com", jid: "chat@muc.example.com", username: "chat", display_name: null, avatar_url: null },
      { id: "chat@example.com/resource", jid: "chat@example.com/resource", username: "chat", display_name: null, avatar_url: null },
    ]);
    expect(await h.dmSync.searchDmRecipients("chat@example.com")).toEqual([]);
    await h.dmSync.handleNewDm("chat@example.com");
    expect(h.openDm).not.toHaveBeenCalled();
    for (const query of ["chat@elsewhere.example", "chat@example.com@example.com", "chat@example.com/device"]) {
      await expect(h.dmSync.searchDmRecipients(query)).rejects.toThrow();
    }
    expect(h.searchUsers).toHaveBeenCalledTimes(1);
    h.scope.stop();
  });

  test("failed search clears previous recipients and retains the error", async () => {
    const h = makeHarness();
    const user = { id: "chat@example.com", jid: "chat@example.com", username: "chat", display_name: null, avatar_url: null };
    h.searchUsers.mockResolvedValue([user]);
    await h.dmSync.searchDmRecipients("chat");
    h.searchUsers.mockRejectedValue(new Error("Directory unavailable"));
    await expect(h.dmSync.searchDmRecipients("chat")).rejects.toThrow("Directory unavailable");
    await h.dmSync.handleNewDm(user.jid);
    expect(h.openDm).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("a stale search and a replaced connection cannot supply selectable recipients", async () => {
    const h = makeHarness();
    const user = { id: "chat@example.com", jid: "chat@example.com", username: "chat", display_name: null, avatar_url: null };
    let finish!: (users: UserSearchResult[]) => void;
    h.searchUsers.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const stale = h.dmSync.searchDmRecipients("chat");
    await h.dmSync.searchDmRecipients("missing");
    finish([user]);
    expect(await stale).toEqual([]);
    await h.dmSync.handleNewDm(user.jid);
    h.searchUsers.mockResolvedValue([user]);
    await h.dmSync.searchDmRecipients("chat");
    h.currentClient.value = null;
    await h.dmSync.handleNewDm(user.jid);
    expect(h.openDm).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("empty account conversations survive discovery of a room with the same name", async () => {
    const h = makeHarness();
    h.channels.value = [{ id: "general", name: "General", spaceId: "space-1" }];
    h.conversations.value = [{
      peerJid: "chat@example.com",
      peerUsername: "chat",
      unreadCount: 0,
    }];
    await nextTick();
    expect(h.forgetPeer).not.toHaveBeenCalled();

    h.channels.value = [
      { id: "general", name: "General", spaceId: "space-1" },
      { id: "chat", name: "Chat", spaceId: "space-1", jid: "chat@muc.example.com" },
    ];
    await nextTick();
    expect(h.forgetPeer).not.toHaveBeenCalled();
    expect(h.conversations.value[0]?.peerJid).toBe("chat@example.com");
    h.scope.stop();
  });

  test("history-bearing colliding 1:1 rows stay in the DM store", async () => {
    const h = makeHarness();
    h.conversations.value = [{
      peerJid: "chat@example.com",
      peerUsername: "chat",
      unreadCount: 0,
      lastMessageAt: "2026-08-26T12:00:00.000Z",
      lastMessageBody: "hey",
    }];
    await nextTick();
    expect(h.forgetPeer).not.toHaveBeenCalled();
    h.scope.stop();
  });
});
