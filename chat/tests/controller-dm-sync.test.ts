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
import type { ChannelSummary } from "../src/lib/chat-types";
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

  const waddles = {
    channels,
    isSubmitting: ref(false),
    loadStructure: mock(async () => null),
  } as unknown as ReturnType<typeof useWaddleDirectory>;
  const dmConversations = {
    conversations,
    activePeerJid,
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
      xmppClient: computed(() => null as BrowserXmppClient | null),
      session: computed(() => session()),
      waddles,
      dmConversations,
      dmMessaging,
      rosterContacts,
      selfDomain: computed(() => "example.com"),
      activeExtensionRouteKey: ref<ExtensionRouteKey | null>(null),
      clearPendingChannelRoomJidSelection,
      selectGroupDm,
      selectChannel,
    }),
  )!;

  return {
    scope,
    dmSync,
    conversations,
    channels,
    openDm,
    forgetPeer,
    selectChannel,
    selectGroupDm,
    loadMessages,
  };
}

describe("useDmSync handleOpenDm / handleNewDm", () => {
  test("opening a full user-domain JID stays a 1:1 even when the node matches a channel id", async () => {
    const h = makeHarness();
    await h.dmSync.handleOpenDm("general@example.com");
    expect(h.selectChannel).not.toHaveBeenCalled();
    expect(h.selectGroupDm).not.toHaveBeenCalled();
    expect(h.openDm).toHaveBeenCalledWith("general@example.com");
    expect(h.loadMessages).toHaveBeenCalledWith("general@example.com", 0);
    h.scope.stop();
  });

  test("New DM username that matches a community channel opens the channel", async () => {
    const h = makeHarness();
    await h.dmSync.handleNewDm("chat");
    expect(h.openDm).not.toHaveBeenCalled();
    expect(h.selectChannel).toHaveBeenCalledWith("chat", { roomJid: "chat@muc.example.com" });
    h.scope.stop();
  });

  test("empty colliding DM rows are forgotten once rooms are known", async () => {
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
    expect(h.forgetPeer).toHaveBeenCalledWith("chat@example.com");
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
