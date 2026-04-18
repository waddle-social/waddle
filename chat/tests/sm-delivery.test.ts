import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
import type { LiveDmMessage, LiveRoomMessage } from "../src/lib/xmpp-client";
import { useDmMessaging } from "../src/composables/useDmMessaging";
import { useMessaging } from "../src/composables/useMessaging";

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
  return ref({ queryMam } as never) as never;
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
});
