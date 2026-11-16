import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
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
import { occupantJidDirectory } from "../src/lib/avatars/author-jid";

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
  const createGroupDm = mock(async (_name: string, _memberJids: string[]) => ({ roomJid: "group@muc.example.com" }));
  const client = { searchUsers, createGroupDm } as unknown as BrowserXmppClient;
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
  const contacts = ref<{ jid: string; name?: string }[]>([]);
  const rosterContacts = {
    contacts,
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
    ui,
    searchUsers,
    createGroupDm,
    currentClient,
    contacts,
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

describe("useDmSync refuses server-hosted bots", () => {
  const BOT = "helper@extensions.example.com";
  beforeEach(() => occupantJidDirectory.record("general@muc.example.com", "helper", `${BOT}/bot`, true));
  afterEach(() => occupantJidDirectory.clear());

  test.each([BOT, `${BOT}/bot`, "Helper@Extensions.example.com"])("opening %s navigates nowhere and never opens a conversation", async (jid) => {
    const h = makeHarness();
    await h.dmSync.handleOpenDm(jid);
    await h.dmSync.handleOpenDm(jid, { intent: "automatic" });
    expect(h.openDm).not.toHaveBeenCalled();
    expect(h.cancelPendingRoute).not.toHaveBeenCalled();
    expect(h.loadMessages).not.toHaveBeenCalled();
    expect(h.ui.actionError.value).toBe("Bots can't receive direct messages.");
    h.scope.stop();
  });

  test("a MUC private message to a bot's occupant is refused through its disclosed real JID", async () => {
    const h = makeHarness();
    await h.dmSync.handleOpenDm("general@muc.example.com/helper", { scope: "muc-occupant" });
    expect(h.openDm).not.toHaveBeenCalled();
    await h.dmSync.handleOpenDm("general@muc.example.com/sam", { scope: "muc-occupant" });
    expect(h.openDm).toHaveBeenCalledTimes(1);
    h.scope.stop();
  });

  test("a bot is not a recipient, a group-DM seed, or a group-DM member", async () => {
    const h = makeHarness();
    h.dmSync.handleAddPeopleToDm(BOT);
    expect(h.ui.showNewDm.value).toBe(false);
    expect(h.ui.groupDmSeedPeerJid.value).toBeNull();

    await h.dmSync.handleCreateGroupDm({ name: "", memberJids: ["bob@example.com", BOT] });
    expect(h.createGroupDm).not.toHaveBeenCalled();
    expect(h.ui.actionError.value).toBe("Bots can't receive direct messages.");

    h.ui.groupDmSeedPeerJid.value = BOT;
    await h.dmSync.handleCreateGroupDm({ name: "", memberJids: ["bob@example.com"] });
    expect(h.createGroupDm).not.toHaveBeenCalled();

    h.ui.groupDmSeedPeerJid.value = null;
    h.contacts.value = [{ jid: "bob@example.com" }, { jid: "carol@example.com" }];
    await h.dmSync.handleCreateGroupDm({ name: "", memberJids: ["bob@example.com", "carol@example.com"] });
    expect(h.createGroupDm).toHaveBeenCalledTimes(1);
    h.scope.stop();
  });
});

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
      { id: "chat@example.com", jid: "chat@example.com", username: "display-chat", display_name: null },
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
      { id: "chatty@example.com", jid: "chatty@example.com", username: "chatty", display_name: null },
      { id: "chat@muc.example.com", jid: "chat@muc.example.com", username: "chat", display_name: null },
      { id: "chat@example.com/resource", jid: "chat@example.com/resource", username: "chat", display_name: null },
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

  test("Add people never seeds a group from a room occupant's private messages", () => {
    const h = makeHarness();
    h.conversations.value = [{ peerJid: "general@muc.example.com/sam", peerUsername: "sam", unreadCount: 0, mucPm: true }];
    h.dmSync.handleAddPeopleToDm("general@muc.example.com/sam");
    expect(h.ui.showNewDm.value).toBe(false);
    expect(h.ui.groupDmSeedPeerJid.value).toBeNull();
    h.dmSync.handleAddPeopleToDm("bob@example.com");
    expect(h.ui.groupDmSeedPeerJid.value).toBe("bob@example.com");
    expect(h.ui.showNewDm.value).toBe(true);
    h.scope.stop();
  });

  test("people picked from earlier searches stay valid until the compose dialog closes", async () => {
    const h = makeHarness();
    const chat = { id: "chat@example.com", jid: "chat@example.com", username: "chat", display_name: null };
    const bob = { id: "bob@example.com", jid: "bob@example.com", username: "bob", display_name: null };
    h.ui.showNewDm.value = true;
    await nextTick();
    h.searchUsers.mockResolvedValueOnce([chat]).mockResolvedValueOnce([bob]);
    await h.dmSync.searchDmRecipients("chat");
    await h.dmSync.searchDmRecipients("bob");
    h.searchUsers.mockRejectedValue(new Error("Directory unavailable"));
    await expect(h.dmSync.searchDmRecipients("carol")).rejects.toThrow("Directory unavailable");
    await h.dmSync.handleCreateGroupDm({ name: "", memberJids: ["chat@example.com", "bob@example.com", "carol@example.com"] });
    expect(h.createGroupDm).not.toHaveBeenCalled();
    expect(h.ui.actionError.value).toBe("Pick people from the directory or your contacts.");
    await h.dmSync.handleNewDm(chat.jid);
    expect(h.openDm).toHaveBeenCalledTimes(1);

    h.ui.showNewDm.value = false;
    await nextTick();
    await h.dmSync.handleNewDm(bob.jid);
    expect(h.openDm).toHaveBeenCalledTimes(1);
    h.contacts.value = [{ jid: "bob@example.com" }];
    await h.dmSync.handleNewDm(bob.jid);
    expect(h.openDm).toHaveBeenLastCalledWith("bob@example.com", undefined);
    h.scope.stop();
  });

  test("a stale search and a replaced connection cannot supply selectable recipients", async () => {
    const h = makeHarness();
    const user = { id: "chat@example.com", jid: "chat@example.com", username: "chat", display_name: null };
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
