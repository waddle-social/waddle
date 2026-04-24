import { describe, expect, mock, test } from "bun:test";
import { nextTick, ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
import { roomBareJidFor, type LiveDmMessage, type LiveRoomMessage } from "../src/lib/xmpp-client";
import { useDmMessaging } from "../src/composables/useDmMessaging";
import { useMessaging } from "../src/composables/useMessaging";
import { handlerStubs } from "./helpers/xmpp-client-mock";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/xmpp",
    ...partial,
  } as WaddleSession;
}

function makeRoomMessaging(xmppClient: ReturnType<typeof makeRoomClient>) {
  const actionError = ref("");
  const messaging = useMessaging(
    ref(session()),
    ref(null),
    xmppClient,
    ref("w1"),
    ref("c1"),
    ref({ id: "c1", name: "general", channel_type: "text" }),
    String,
    actionError,
    () => {
      actionError.value = "";
    },
  );
  return { messaging, actionError };
}

function makeRoomClient(queryMamResults: LiveRoomMessage[] = []) {
  const queryMam = mock(async () => queryMamResults);
  return ref({ ...handlerStubs(), queryMam } as never) as never;
}

function makeDmMessaging(xmppClient: ReturnType<typeof makeDmClient>) {
  const actionError = ref("");
  const dm = useDmMessaging(
    ref(session()),
    xmppClient,
    ref("bob@example.com"),
    String,
    actionError,
    () => {
      actionError.value = "";
    },
  );
  return { dm, actionError };
}

function makeDmClient(queryPersonalMamResults: LiveDmMessage[] = []) {
  const queryPersonalMam = mock(async () => queryPersonalMamResults);
  return ref({ queryPersonalMam } as never) as never;
}

describe("XEP-0198 delivery status (group chat)", () => {
  test("onMessageAck promotes sending -> delivered", () => {
    const client = makeRoomClient();
    const { messaging } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "client-1",
        author: "alice",
        body: "hi",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
        deliveryStatus: "sending",
      },
    ];

    messaging.onMessageAck("client-1");

    expect(messaging.messages.value[0].deliveryStatus).toBe("delivered");
  });

  test("onMessageAck is a no-op for unknown ids", () => {
    const client = makeRoomClient();
    const { messaging } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "client-1",
        author: "alice",
        body: "hi",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
        deliveryStatus: "sending",
      },
    ];

    messaging.onMessageAck("other-id");

    expect(messaging.messages.value[0].deliveryStatus).toBe("sending");
  });

  test("onMessageDeliveryFailure marks pending messages failed", () => {
    const client = makeRoomClient();
    const { messaging } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "client-1",
        author: "alice",
        body: "hi",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
        deliveryStatus: "sending",
      },
    ];

    messaging.onMessageDeliveryFailure("client-1");

    expect(messaging.messages.value[0].deliveryStatus).toBe("failed");
  });

  test("onMessageDeliveryFailure does not regress a delivered message", () => {
    const client = makeRoomClient();
    const { messaging } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "client-1",
        author: "alice",
        body: "hi",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
        deliveryStatus: "delivered",
      },
    ];

    messaging.onMessageDeliveryFailure("client-1");

    expect(messaging.messages.value[0].deliveryStatus).toBe("delivered");
  });
});

describe("XEP-0198 session lifecycle catch-up (group chat)", () => {
  test("fresh session re-fetches MAM when messages were already loaded", async () => {
    const client = makeRoomClient();
    const { messaging } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "seen-1",
        author: "bob",
        body: "hi",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    messaging.onSessionLifecycle({ type: "fresh" });

    // Wait for microtasks so loadMessages can fire.
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as { value: { queryMam: ReturnType<typeof mock> } };
    expect(clientAny.value.queryMam).toHaveBeenCalledWith("w1", "c1", 100);
  });

  test("fresh session is a no-op when no messages are loaded yet", async () => {
    const client = makeRoomClient();
    const { messaging } = makeRoomMessaging(client);

    messaging.onSessionLifecycle({ type: "fresh" });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as { value: { queryMam: ReturnType<typeof mock> } };
    expect(clientAny.value.queryMam).not.toHaveBeenCalled();
  });

  test("resumed session never triggers a MAM refetch", async () => {
    const client = makeRoomClient();
    const { messaging } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "seen-1",
        author: "bob",
        body: "hi",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    messaging.onSessionLifecycle({ type: "resumed" });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as { value: { queryMam: ReturnType<typeof mock> } };
    expect(clientAny.value.queryMam).not.toHaveBeenCalled();
  });
});

describe("XEP-0198 delivery status (DM)", () => {
  test("onMessageAck promotes sending -> delivered in DM timeline", () => {
    const client = makeDmClient();
    const { dm } = makeDmMessaging(client);

    dm.messages.value = [
      {
        id: "dm-1",
        author: "alice",
        body: "hey",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
        deliveryStatus: "sending",
      },
    ];

    dm.onMessageAck("dm-1");

    expect(dm.messages.value[0].deliveryStatus).toBe("delivered");
  });

  test("fresh DM session re-fetches personal MAM when messages were loaded", async () => {
    const client = makeDmClient();
    const { dm } = makeDmMessaging(client);

    dm.messages.value = [
      {
        id: "dm-old",
        author: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    dm.onSessionLifecycle({ type: "fresh" });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as {
      value: { queryPersonalMam: ReturnType<typeof mock> };
    };
    expect(clientAny.value.queryPersonalMam).toHaveBeenCalledWith("bob@example.com", 100);
  });

  test("DM self-echo reconciles first send only when duplicate text is queued", async () => {
    const client = makeDmClient();
    const { dm } = makeDmMessaging(client);

    // Seed two optimistic sends with identical body via the public sendMessage
    // path so the internal pendingEchoClientIds set is populated.
    const sendDirectMessage = mock(async (_peer: string, _body: string) => ({
      id: "client-a",
      state: "sending" as const,
    }));
    const sendDmChatState = mock(async () => undefined);
    (client as unknown as { value: Record<string, unknown> }).value = {
      ...(client as unknown as { value: Record<string, unknown> }).value,
      sendDirectMessage,
      sendDmChatState,
    };
    await dm.sendMessage("ok");
    sendDirectMessage.mockImplementationOnce(async () => ({
      id: "client-b",
      state: "sending" as const,
    }));
    await dm.sendMessage("ok");

    expect(dm.messages.value.map((m) => m.id)).toEqual(["client-a", "client-b"]);

    // First echo with server id — should reconcile the first optimistic entry.
    dm.onIncomingMessage({
      id: "server-a",
      peerJid: "bob@example.com",
      fromJid: "alice@example.com/desktop",
      nick: "alice",
      body: "ok",
      createdAt: "2024-01-01T00:00:01Z",
      type: "message",
    });
    expect(dm.messages.value[0].id).toBe("server-a");
    expect(dm.messages.value[0].deliveryStatus).toBe("delivered");
    expect(dm.messages.value[1].id).toBe("client-b");
    expect(dm.messages.value[1].deliveryStatus).toBe("sending");

    // Second echo with a different server id — must NOT retarget the first
    // (already reconciled) message.
    dm.onIncomingMessage({
      id: "server-b",
      peerJid: "bob@example.com",
      fromJid: "alice@example.com/desktop",
      nick: "alice",
      body: "ok",
      createdAt: "2024-01-01T00:00:02Z",
      type: "message",
    });
    expect(dm.messages.value[0].id).toBe("server-a");
    expect(dm.messages.value[1].id).toBe("server-b");
    expect(dm.messages.value[1].deliveryStatus).toBe("delivered");
  });

  test("DM self-echo promotes a previously-failed message to delivered", async () => {
    const client = makeDmClient();
    const { dm } = makeDmMessaging(client);

    const sendDirectMessage = mock(async (_peer: string, _body: string) => ({
      id: "client-x",
      state: "sending" as const,
    }));
    const sendDmChatState = mock(async () => undefined);
    (client as unknown as { value: Record<string, unknown> }).value = {
      ...(client as unknown as { value: Record<string, unknown> }).value,
      sendDirectMessage,
      sendDmChatState,
    };
    await dm.sendMessage("ping");
    dm.onMessageDeliveryFailure("client-x");
    expect(dm.messages.value[0].deliveryStatus).toBe("failed");

    dm.onIncomingMessage({
      id: "server-x",
      peerJid: "bob@example.com",
      fromJid: "alice@example.com/desktop",
      nick: "alice",
      body: "ping",
      createdAt: "2024-01-01T00:00:03Z",
      type: "message",
    });

    expect(dm.messages.value).toHaveLength(1);
    expect(dm.messages.value[0].id).toBe("server-x");
    expect(dm.messages.value[0].deliveryStatus).toBe("delivered");
  });

  test("fresh DM session preserves local-only sending/failed entries", async () => {
    const mamFresh: LiveDmMessage[] = [
      {
        id: "server-old",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/desktop",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message",
      },
    ];
    const client = makeDmClient(mamFresh);
    const { dm } = makeDmMessaging(client);

    dm.messages.value = [
      {
        id: "server-old",
        author: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
      {
        id: "client-unsent",
        author: "alice",
        body: "didn't go through",
        createdAt: "2024-01-01T00:00:02Z",
        isSelf: true,
        deliveryStatus: "failed",
      },
    ];

    dm.onSessionLifecycle({ type: "fresh" });
    // Wait two ticks: one for microtask, one for loadMessages async path.
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();

    const ids = dm.messages.value.map((m) => m.id);
    expect(ids).toContain("server-old");
    expect(ids).toContain("client-unsent");
  });
});

describe("XEP-0198 self-echo reconciliation (group chat)", () => {
  test("fresh session preserves local-only sending/failed entries", async () => {
    const mamFresh: LiveRoomMessage[] = [
      {
        id: "server-old",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message",
      },
    ];
    const client = makeRoomClient(mamFresh);
    const { messaging } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "server-old",
        author: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
      {
        id: "client-unsent",
        author: "alice",
        body: "not acked yet",
        createdAt: "2024-01-01T00:00:02Z",
        isSelf: true,
        deliveryStatus: "sending",
      },
    ];

    messaging.onSessionLifecycle({ type: "fresh" });
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();

    const ids = messaging.messages.value.map((m) => m.id);
    expect(ids).toContain("server-old");
    expect(ids).toContain("client-unsent");
  });

  test("fresh room session self-echo reconciles preserved sending entries", async () => {
    const currentSession = session();
    const roomJid = roomBareJidFor(currentSession, "c1");
    const queryMam = mock(async () => []);
    const sendGroupMessage = mock(async () => ({ id: "client-room", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    let onMessage: ((msg: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const xmppClient = ref(null as never);
    const messaging = useMessaging(
      ref(currentSession),
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    xmppClient.value = {
      queryMam,
      sendGroupMessage,
      sendChatState,
      setMessageHandler(handler: (msg: LiveRoomMessage) => void) {
        onMessage = handler;
      },
      setStatusHandler() {},
      setChatStateHandler() {},
      setReactionHandler() {},
      setDisplayedHandler() {},
      setHatsHandler() {},
      setPresenceHandler() {},
      setLastSeenHandler() {},
      setActivityHandler() {},
      setRoomAvatarHandler() {},
      setSlowModeHandler() {},
    } as never;
    await nextTick();

    await messaging.sendMessage("hello room");
    expect(messaging.messages.value[0]?.deliveryStatus).toBe("sending");

    messaging.onSessionLifecycle({ type: "fresh" });
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();

    onMessage?.({
      id: "server-room",
      roomJid,
      nick: "alice",
      body: "hello room",
      createdAt: "2024-01-01T00:00:03Z",
      type: "message",
    });

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0].id).toBe("server-room");
    expect(messaging.messages.value[0].deliveryStatus).toBe("delivered");
  });
});

test("fresh DM session self-echo reconciles preserved sending entries", async () => {
  const client = makeDmClient([]);
  const { dm } = makeDmMessaging(client);

  const sendDirectMessage = mock(async (_peer: string, _body: string) => ({
    id: "client-dm",
    state: "sending" as const,
  }));
  const sendDmChatState = mock(async () => undefined);
  (client as unknown as { value: Record<string, unknown> }).value = {
    ...(client as unknown as { value: Record<string, unknown> }).value,
    sendDirectMessage,
    sendDmChatState,
  };

  await dm.sendMessage("hello dm");
  expect(dm.messages.value[0]?.deliveryStatus).toBe("sending");

  dm.onSessionLifecycle({ type: "fresh" });
  await new Promise((r) => setTimeout(r, 0));
  await new Promise((r) => setTimeout(r, 0));
  await nextTick();

  dm.onIncomingMessage({
    id: "server-dm",
    peerJid: "bob@example.com",
    fromJid: "alice@example.com/desktop",
    nick: "alice",
    body: "hello dm",
    createdAt: "2024-01-01T00:00:04Z",
    type: "message",
  });

  expect(dm.messages.value).toHaveLength(1);
  expect(dm.messages.value[0].id).toBe("server-dm");
  expect(dm.messages.value[0].deliveryStatus).toBe("delivered");
});
