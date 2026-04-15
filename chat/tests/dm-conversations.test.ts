import { describe, test, expect } from "bun:test";
import { ref } from "vue";
import { useDmConversations } from "../src/composables/useDmConversations";
import type { WaddleSession } from "../src/lib/server-auth";
import type { BrowserXmppClient, LiveDmMessage } from "../src/lib/xmpp-client";

function makeSession(jid = "alice@example.com/web"): WaddleSession {
  return { jid, username: "alice", token: "tok" } as WaddleSession;
}

function makeComposable(jid = "alice@example.com/web") {
  const session = ref<WaddleSession | null>(makeSession(jid));
  const client = ref<BrowserXmppClient | null>(null);
  return { composable: useDmConversations(session, client), session, client };
}

function makeDmMessage(overrides: Partial<LiveDmMessage> = {}): LiveDmMessage {
  return {
    id: "msg-1",
    peerJid: "bob@example.com",
    fromJid: "bob@example.com",
    nick: "bob",
    body: "hello",
    createdAt: new Date().toISOString(),
    type: "message",
    ...overrides,
  };
}

describe("useDmConversations", () => {
  test("starts with empty state", () => {
    const { composable } = makeComposable();
    expect(composable.conversations.value).toEqual([]);
    expect(composable.activePeerJid.value).toBeNull();
    expect(composable.hasUnread.value).toBe(false);
  });

  test("openDm creates a conversation and sets activePeerJid", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    expect(composable.activePeerJid.value).toBe("bob@example.com");
    expect(composable.conversations.value).toHaveLength(1);
    expect(composable.conversations.value[0].peerUsername).toBe("bob");
  });

  test("openDm strips resource from JID", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com/mobile");
    expect(composable.activePeerJid.value).toBe("bob@example.com");
    expect(composable.conversations.value[0].peerJid).toBe("bob@example.com");
  });

  test("openDm does not duplicate conversations", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    await composable.openDm("bob@example.com");
    expect(composable.conversations.value).toHaveLength(1);
  });

  test("closeDm clears activePeerJid", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    composable.closeDm();
    expect(composable.activePeerJid.value).toBeNull();
  });

  test("receiveIncomingDm creates conversation and increments unread", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage());
    expect(composable.conversations.value).toHaveLength(1);
    expect(composable.conversations.value[0].unreadCount).toBe(1);
    expect(composable.conversations.value[0].lastMessageBody).toBe("hello");
    expect(composable.hasUnread.value).toBe(true);
  });

  test("receiveIncomingDm does not increment unread for active conversation", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    composable.receiveIncomingDm(makeDmMessage());
    expect(composable.conversations.value[0].unreadCount).toBe(0);
  });

  test("receiveIncomingDm does not increment unread for self-sent messages", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(
      makeDmMessage({ fromJid: "alice@example.com", peerJid: "bob@example.com" }),
    );
    expect(composable.conversations.value[0].unreadCount).toBe(0);
  });

  test("markRead resets unread count", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(makeDmMessage());
    composable.receiveIncomingDm(makeDmMessage({ id: "msg-2" }));
    expect(composable.conversations.value[0].unreadCount).toBe(2);
    composable.markRead("bob@example.com");
    expect(composable.conversations.value[0].unreadCount).toBe(0);
    expect(composable.hasUnread.value).toBe(false);
  });

  test("updatePresence updates conversation presence", async () => {
    const { composable } = makeComposable();
    await composable.openDm("bob@example.com");
    composable.updatePresence({ bareJid: "bob@example.com", show: "away" });
    expect(composable.conversations.value[0].presenceShow).toBe("away");
  });

  test("conversations sort by most recent message", () => {
    const { composable } = makeComposable();
    composable.receiveIncomingDm(
      makeDmMessage({ peerJid: "carol@example.com", fromJid: "carol@example.com", nick: "carol", createdAt: "2026-01-01T00:00:00Z" }),
    );
    composable.receiveIncomingDm(
      makeDmMessage({ peerJid: "bob@example.com", fromJid: "bob@example.com", nick: "bob", createdAt: "2026-01-02T00:00:00Z" }),
    );
    expect(composable.conversations.value[0].peerJid).toBe("bob@example.com");
    expect(composable.conversations.value[1].peerJid).toBe("carol@example.com");
  });
});
