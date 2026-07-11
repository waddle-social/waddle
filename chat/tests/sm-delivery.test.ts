import { describe, expect, mock, test } from "bun:test";
import { nextTick, ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
import { roomBareJidFor, type LiveDmMessage, type LiveRoomMessage } from "../src/lib/xmpp-client";
import { useDirectMessages } from "../src/dms/messages";
import { useChannelMessages } from "../src/channels/messages";
import { dmMessageFromArchived, roomMessageFromArchived } from "../src/lib/xmpp/client";
import type { WasmArchivedMessage } from "../src/lib/xmpp/wasm-types";
import { handlerStubs } from "./helpers/xmpp-client-mock";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
    ...partial,
  } as WaddleSession;
}

function makeRoomMessaging(xmppClient: ReturnType<typeof makeRoomClient>) {
  const actionError = ref("");
  const messaging = useChannelMessages(
    ref(session()),
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

function makeDmMessaging(
  xmppClient: ReturnType<typeof makeDmClient>,
  activePeerJid = "bob@example.com",
) {
  const actionError = ref("");
  const dm = useDirectMessages(
    ref(session()),
    xmppClient,
    ref(activePeerJid),
    String,
    actionError,
    () => {
      actionError.value = "";
    },
  );
  return { dm, actionError };
}

function makeDmClient(
  queryPersonalMamResults: LiveDmMessage[] = [],
  isMucPmPeer = (peerJid: string) => peerJid.includes("@muc.example.com/"),
) {
  const queryPersonalMam = mock(async () => queryPersonalMamResults);
  return ref({ queryPersonalMam, isMucPmPeer } as never) as never;
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
  test("fresh session skips the MAM reload when reconnect catch-up covers the room", async () => {
    const client = makeRoomClient();
    const { messaging } = makeRoomMessaging(client);

    const existing = [
      {
        id: "seen-1",
        author: "bob",
        body: "hi",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];
    messaging.messages.value = existing;

    messaging.onSessionLifecycle({
      type: "fresh",
      catchup: { dmJids: [], roomJids: ["c1@muc.example.com"] },
    });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as { value: { queryMam: ReturnType<typeof mock> } };
    expect(clientAny.value.queryMam).not.toHaveBeenCalled();
    expect(messaging.messages.value).toEqual(existing);
  });

  test("catch-up coverage matches room JIDs case-insensitively", async () => {
    // Cursor keys are server-emitted JIDs (RFC 7622 lowercased); the
    // directory-derived room JID can differ in case. The skip must
    // still fire or the double-MAM race silently returns.
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

    messaging.onSessionLifecycle({
      type: "fresh",
      catchup: { dmJids: [], roomJids: ["C1@MUC.Example.com"] },
    });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as { value: { queryMam: ReturnType<typeof mock> } };
    expect(clientAny.value.queryMam).not.toHaveBeenCalled();
  });

  test("catch-up failure for the active room falls back to the wholesale reload", async () => {
    // The coverage-based skip trusts the catch-up to close the gap; when
    // that catch-up fails for this room the reload is the safety net —
    // serialized after the failed attempt, so the #1180 race can't recur.
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

    messaging.onCatchupFailed({ kind: "room", key: "C1@muc.example.com" });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as { value: { queryMam: ReturnType<typeof mock> } };
    expect(clientAny.value.queryMam).toHaveBeenCalledWith("w1", "c1", 100);
  });

  test("a failed fallback reload restores the pre-reload timeline instead of wiping it", async () => {
    // If the reload the fallback fires ALSO fails, loadMessages' catch
    // wipes the timeline to queued-only — and the covered-skip would
    // then block the self-heal on the next reconnect. The fallback must
    // put the pre-reload timeline back.
    const queryMam = mock(async () => {
      throw new Error("mam down");
    });
    const client = ref({ ...handlerStubs(), queryMam } as never) as never;
    const { messaging } = makeRoomMessaging(client);

    const existing = [
      {
        id: "seen-1",
        author: "bob",
        body: "hi",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];
    messaging.messages.value = existing;

    messaging.onCatchupFailed({ kind: "room", key: "c1@muc.example.com" });
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));

    expect(queryMam).toHaveBeenCalled();
    expect(messaging.messages.value).toEqual(existing);
  });

  test("catch-up failure for a different room does not reload", async () => {
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

    messaging.onCatchupFailed({ kind: "room", key: "other@muc.example.com" });
    messaging.onCatchupFailed({ kind: "dm", key: "c1@muc.example.com" });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as { value: { queryMam: ReturnType<typeof mock> } };
    expect(clientAny.value.queryMam).not.toHaveBeenCalled();
  });

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

    messaging.onSessionLifecycle({ type: "fresh", catchup: { dmJids: [], roomJids: [] } });

    // Wait for microtasks so loadMessages can fire.
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as { value: { queryMam: ReturnType<typeof mock> } };
    expect(clientAny.value.queryMam).toHaveBeenCalledWith("w1", "c1", 100);
  });

  test("fresh session preserves learned thread metadata for matching MAM messages", async () => {
    const client = makeRoomClient([
      {
        id: "thread-42",
        roomJid: "w1-c1@rooms.example.com",
        nick: "alice",
        body: "topic root",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message",
      },
      {
        id: "reply-archive",
        wireIds: ["reply-live"],
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "reply from fresh MAM",
        createdAt: "2024-01-01T00:01:00Z",
        type: "message",
        replyTo: { id: "thread-42" },
      },
      {
        id: "reaction-archive",
        roomJid: "w1-c1@rooms.example.com",
        nick: "carol",
        body: "",
        createdAt: "2024-01-01T00:02:00Z",
        type: "subject",
        _reactionTarget: "reply-live",
        _reactionEmojis: ["🔥"],
        _reactionSenderId: "carol@example.com",
      },
    ]);
    const { messaging } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "thread-42",
        author: "alice",
        authorJid: "w1-c1@rooms.example.com/alice",
        authorOccupantJid: "w1-c1@rooms.example.com/alice",
        body: "topic root",
        createdAt: "2024-01-01T00:00:00Z",
        createdAtSource: "delay",
        isSelf: true,
      },
      {
        id: "reply-live",
        wireIds: ["reply-archive"],
        reactionTargetId: "reply-live",
        author: "bob",
        authorJid: "w1-c1@rooms.example.com/bob",
        authorOccupantJid: "w1-c1@rooms.example.com/bob",
        body: "live reply",
        createdAt: "2024-01-01T00:01:00Z",
        createdAtSource: "delay",
        isSelf: false,
        replyTo: {
          id: "thread-42",
          author: "alice",
          preview: "topic root",
        },
        threadId: "thread-42",
        parentThreadId: "parent-thread",
      },
    ];

    messaging.onSessionLifecycle({ type: "fresh", catchup: { dmJids: [], roomJids: [] } });
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();

    expect(messaging.messages.value).toHaveLength(2);
    const reply = messaging.messages.value.find((message) => message.id === "reply-archive");
    expect(reply?.wireIds).toContain("reply-live");
    expect(reply?.body).toBe("reply from fresh MAM");
    expect(reply?.threadId).toBe("thread-42");
    expect(reply?.parentThreadId).toBe("parent-thread");
    expect(reply?.reactionTargetId).toBe("reply-live");
    expect(reply?.replyTo).toEqual({
      id: "thread-42",
      author: "alice",
      preview: "topic root",
    });
    expect(reply?.reactions).toEqual({ "🔥": ["carol"] });
  });

  test("fresh session is a no-op when no messages are loaded yet", async () => {
    const client = makeRoomClient();
    const { messaging } = makeRoomMessaging(client);

    messaging.onSessionLifecycle({ type: "fresh", catchup: { dmJids: [], roomJids: [] } });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as { value: { queryMam: ReturnType<typeof mock> } };
    expect(clientAny.value.queryMam).not.toHaveBeenCalled();
  });

  test("MAM-replayed reactions attach to the original message after a fresh load", async () => {
    // Mirrors what `roomMessageFromArchived` produces for a MAM page that
    // contains the original message followed by a reaction stanza pointing
    // back at that message via XEP-0359 stanza-id.
    const client = makeRoomClient([
      {
        id: "msg-original",
        wireIds: ["msg-original", "room-stanza-1"],
        roomJid: "c1@muc.example.com",
        nick: "alice",
        body: "react to me",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message",
        reactionTargetId: "room-stanza-1",
        replyableId: "room-stanza-1",
      },
      {
        id: "reaction-1",
        roomJid: "c1@muc.example.com",
        nick: "bob",
        body: "",
        createdAt: "2024-01-01T00:01:00Z",
        type: "subject",
        _reactionTarget: "room-stanza-1",
        _reactionEmojis: ["👍"],
        _reactionSenderId: "bob@example.com",
      },
    ]);
    const { messaging } = makeRoomMessaging(client);

    await messaging.loadMessages("w1", "c1");
    await nextTick();

    const original = messaging.messages.value.find((m) => m.id === "msg-original");
    expect(original?.reactions).toEqual({ "👍": ["bob"] });
  });

  test("MAM-replayed reactions persist when LiveRoomMessages come from roomMessageFromArchived (full path)", async () => {
    // Mirrors what production sees: `queryMamPage` returns a list of WASM
    // archived messages that we run through `roomMessageFromArchived`. The
    // resulting LiveRoomMessage[] must be enough for the channel composable
    // to attach the reaction back onto its target on a fresh load.
    const archivedOriginal: WasmArchivedMessage = {
      mam_id: "mam-original",
      message_type: "groupchat",
      from: "c1@muc.example.com/alice",
      to: "alice@example.com",
      id: "client-id-original",
      stanza_id: "room-stanza-original",
      stanza_id_by: "c1@muc.example.com",
      body: "react to me",
      reaction_emojis: [],
      is_muc: true,
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      timestamp: "2024-01-01T00:00:00Z",
    };
    const archivedReaction: WasmArchivedMessage = {
      mam_id: "mam-reaction",
      message_type: "groupchat",
      from: "c1@muc.example.com/bob",
      to: "alice@example.com",
      id: "client-id-reaction",
      stanza_id: "room-stanza-reaction",
      stanza_id_by: "c1@muc.example.com",
      reaction_target_id: "room-stanza-original",
      reaction_emojis: ["👍"],
      author_real_jid: "bob@example.com",
      is_muc: true,
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      timestamp: "2024-01-01T00:01:00Z",
    };
    const liveMessages = [archivedOriginal, archivedReaction]
      .map(roomMessageFromArchived)
      .filter((m): m is LiveRoomMessage => !!m);

    const client = makeRoomClient(liveMessages);
    const { messaging } = makeRoomMessaging(client);

    await messaging.loadMessages("w1", "c1");
    await nextTick();

    const original = messaging.messages.value.find((m) => m.body === "react to me");
    expect(original).toBeDefined();
    expect(original?.reactionTargetId).toBe("room-stanza-original");
    expect(original?.reactions).toEqual({ "👍": ["bob"] });
  });

  test("toggleReaction optimistically updates the local timeline before the wire echo", async () => {
    const sendReaction = mock(async () => undefined);
    const client = makeRoomClient();
    (client as unknown as { value: Record<string, unknown> }).value = {
      ...(client as unknown as { value: Record<string, unknown> }).value,
      sendReaction,
    };
    const { messaging } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "msg-1",
        wireIds: ["msg-1", "room-stanza-1"],
        reactionTargetId: "room-stanza-1",
        author: "bob",
        body: "react to me",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.toggleReaction("msg-1", "👍");

    const target = messaging.messages.value.find((m) => m.id === "msg-1");
    expect(target?.reactions).toEqual({ "👍": ["alice"] });
    expect(sendReaction).toHaveBeenCalled();
  });

  test("toggleReaction rolls back the optimistic update when sendReaction rejects", async () => {
    const sendReaction = mock(async () => {
      throw new Error("session not ready");
    });
    const client = makeRoomClient();
    (client as unknown as { value: Record<string, unknown> }).value = {
      ...(client as unknown as { value: Record<string, unknown> }).value,
      sendReaction,
    };
    const { messaging, actionError } = makeRoomMessaging(client);

    messaging.messages.value = [
      {
        id: "msg-rollback",
        wireIds: ["msg-rollback", "room-stanza-rollback"],
        reactionTargetId: "room-stanza-rollback",
        author: "bob",
        body: "react to me",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.toggleReaction("msg-rollback", "👍");

    const target = messaging.messages.value.find((m) => m.id === "msg-rollback");
    expect(target?.reactions).toBeUndefined();
    expect(actionError.value).not.toBe("");
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

  test("fresh DM session skips the MAM reload when reconnect catch-up covers the peer", async () => {
    const client = makeDmClient();
    const { dm } = makeDmMessaging(client);

    const existing = [
      {
        id: "dm-old",
        author: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];
    dm.messages.value = existing;

    dm.onSessionLifecycle({
      type: "fresh",
      // Case differs from the active peer JID on purpose: coverage
      // matching must be case-insensitive (RFC 7622-lowercased keys).
      catchup: { dmJids: ["Bob@Example.com"], roomJids: [] },
    });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as {
      value: { queryPersonalMam: ReturnType<typeof mock> };
    };
    expect(clientAny.value.queryPersonalMam).not.toHaveBeenCalled();
    expect(dm.messages.value).toEqual(existing);
  });

  test("fresh MUC-PM session reloads when catch-up covers only a sibling occupant", async () => {
    const client = makeDmClient();
    const { dm } = makeDmMessaging(client, "room@muc.example.com/bob");
    dm.messages.value = [{
      id: "pm-old",
      author: "bob",
      body: "earlier",
      createdAt: "2024-01-01T00:00:00Z",
      isSelf: false,
    }];

    dm.onSessionLifecycle({
      type: "fresh",
      catchup: { dmJids: ["room@muc.example.com/alice"], roomJids: [] },
    });
    await new Promise((resolve) => setTimeout(resolve, 0));

    const clientAny = client as unknown as {
      value: { queryPersonalMam: ReturnType<typeof mock> };
    };
    expect(clientAny.value.queryPersonalMam).toHaveBeenCalledWith(
      "room@muc.example.com/bob",
      100,
    );
  });

  test("fresh custom-service MUC-PM session reloads when catch-up covers only a sibling occupant", async () => {
    // Cold reload: topology discovery has not yet updated the client's MUC
    // service, so persisted cursor scope is the authoritative signal.
    const client = makeDmClient([], () => false);
    const { dm } = makeDmMessaging(client, "room@rooms.waddle.example/bob");
    dm.messages.value = [{
      id: "pm-old",
      author: "bob",
      body: "earlier",
      createdAt: "2024-01-01T00:00:00Z",
      isSelf: false,
    }];

    dm.onSessionLifecycle({
      type: "fresh",
      catchup: {
        dmJids: ["room@rooms.waddle.example/alice"],
        dmOccupantJids: ["room@rooms.waddle.example/alice"],
        roomJids: [],
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 0));

    const clientAny = client as unknown as {
      value: { queryPersonalMam: ReturnType<typeof mock> };
    };
    expect(clientAny.value.queryPersonalMam).toHaveBeenCalledWith(
      "room@rooms.waddle.example/bob",
      100,
    );
  });

  test("MUC-PM catch-up failure reloads only the same occupant", async () => {
    const client = makeDmClient();
    const { dm } = makeDmMessaging(client, "room@muc.example.com/bob");
    dm.messages.value = [{
      id: "pm-old",
      author: "bob",
      body: "earlier",
      createdAt: "2024-01-01T00:00:00Z",
      isSelf: false,
    }];

    dm.onCatchupFailed({ kind: "dm", key: "room@muc.example.com/alice" });
    await new Promise((resolve) => setTimeout(resolve, 0));

    const clientAny = client as unknown as {
      value: { queryPersonalMam: ReturnType<typeof mock> };
    };
    expect(clientAny.value.queryPersonalMam).not.toHaveBeenCalled();
  });

  test("catch-up failure for the active DM peer falls back to the wholesale reload", async () => {
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

    dm.onCatchupFailed({ kind: "dm", key: "Bob@example.com" });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as {
      value: { queryPersonalMam: ReturnType<typeof mock> };
    };
    expect(clientAny.value.queryPersonalMam).toHaveBeenCalledWith("bob@example.com", 100);
  });

  test("a failed DM fallback reload restores the pre-reload timeline instead of wiping it", async () => {
    const queryPersonalMam = mock(async () => {
      throw new Error("mam down");
    });
    const client = ref({ queryPersonalMam } as never) as never;
    const { dm } = makeDmMessaging(client);

    const existing = [
      {
        id: "dm-old",
        author: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];
    dm.messages.value = existing;

    dm.onCatchupFailed({ kind: "dm", key: "bob@example.com" });
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));

    expect(queryPersonalMam).toHaveBeenCalled();
    expect(dm.messages.value).toEqual(existing);
  });

  test("a superseded fallback reload does not append stale rows onto the newer load", async () => {
    // The fallback reload can be superseded by a user-triggered reload
    // of the SAME conversation. The superseded call reports "aborted"
    // and the fallback must not react — the newer request owns the
    // timeline now.
    const resolvers: Array<(value: LiveDmMessage[]) => void> = [];
    const queryPersonalMam = mock(
      () => new Promise<LiveDmMessage[]>((resolve) => resolvers.push(resolve)),
    );
    const client = ref({ queryPersonalMam } as never) as never;
    const { dm } = makeDmMessaging(client);

    dm.messages.value = [
      {
        id: "dm-old",
        author: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
      {
        id: "failed-1",
        author: "alice",
        body: "unsent",
        createdAt: "2024-01-01T00:01:00Z",
        isSelf: true,
        deliveryStatus: "failed",
      },
    ];

    dm.onCatchupFailed({ kind: "dm", key: "bob@example.com" });
    await new Promise((r) => setTimeout(r, 0));
    void dm.loadMessages("bob@example.com");
    await new Promise((r) => setTimeout(r, 0));
    expect(resolvers).toHaveLength(2);

    // Newer request completes first and owns the timeline.
    resolvers[1]([]);
    await new Promise((r) => setTimeout(r, 0));
    const ownedByNewer = [...dm.messages.value];

    // The superseded fallback reload now resolves → "aborted".
    resolvers[0]([]);
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));

    expect(dm.messages.value).toEqual(ownedByNewer);
  });

  test("catch-up failure for a different DM peer does not reload", async () => {
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

    dm.onCatchupFailed({ kind: "dm", key: "carol@example.com" });
    dm.onCatchupFailed({ kind: "room", key: "bob@example.com" });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as {
      value: { queryPersonalMam: ReturnType<typeof mock> };
    };
    expect(clientAny.value.queryPersonalMam).not.toHaveBeenCalled();
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

    dm.onSessionLifecycle({ type: "fresh", catchup: { dmJids: [], roomJids: [] } });
    await new Promise((r) => setTimeout(r, 0));

    const clientAny = client as unknown as {
      value: { queryPersonalMam: ReturnType<typeof mock> };
    };
    expect(clientAny.value.queryPersonalMam).toHaveBeenCalledWith("bob@example.com", 100);
  });

  test("MAM-replayed DM reactions attach to the original message after a fresh load", async () => {
    const client = makeDmClient([
      {
        id: "dm-original",
        peerJid: "bob@example.com",
        fromJid: "alice@example.com/desktop",
        nick: "alice",
        body: "react to me in dm",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message",
      },
      {
        id: "dm-reaction",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/desktop",
        nick: "bob",
        body: "",
        createdAt: "2024-01-01T00:01:00Z",
        type: "message",
        _reactionTarget: "dm-original",
        _reactionEmojis: ["🔥"],
      },
    ]);
    const { dm } = makeDmMessaging(client);

    await dm.loadMessages("bob@example.com");
    await nextTick();

    const original = dm.messages.value.find((m) => m.id === "dm-original");
    expect(original?.reactions).toEqual({ "🔥": ["bob"] });
  });

  test("MAM-replayed DM reactions survive when archived DTOs expose the conformant direct ID", async () => {
    const originalWireId = "dm-original-wire-id";
    const archivedOriginal: WasmArchivedMessage = {
      mam_id: "mam-dm-original",
      message_type: "chat",
      from: "alice@example.com/desktop",
      to: "bob@example.com",
      id: originalWireId,
      body: "react to me in dm after refresh",
      reaction_emojis: [],
      is_muc: false,
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      timestamp: "2024-01-01T00:00:00Z",
    };
    const archivedReaction: WasmArchivedMessage = {
      mam_id: "mam-dm-reaction",
      message_type: "chat",
      from: "bob@example.com/desktop",
      to: "alice@example.com",
      reaction_target_id: originalWireId,
      reaction_emojis: ["🔥"],
      is_muc: false,
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      timestamp: "2024-01-01T00:01:00Z",
    };
    const liveMessages = [archivedOriginal, archivedReaction]
      .map((message) => dmMessageFromArchived(message, "alice@example.com"))
      .filter((message): message is LiveDmMessage => !!message);
    const client = makeDmClient(liveMessages);
    const { dm } = makeDmMessaging(client);

    await dm.loadMessages("bob@example.com");
    await nextTick();

    const original = dm.messages.value.find((m) => m.body === "react to me in dm after refresh");
    expect(original).toBeDefined();
    expect(original?.reactions).toEqual({ "🔥": ["bob"] });
  });

  test("DM reactions sent after MAM refresh target the archived origin-id before message id", async () => {
    const messageId = "dm-message-id";
    const originId = "dm-origin-id";
    const archivedOriginal: WasmArchivedMessage = {
      mam_id: "mam-dm-origin-original",
      message_type: "chat",
      from: "bob@example.com/desktop",
      to: "alice@example.com",
      id: messageId,
      origin_id: originId,
      body: "react to origin-id",
      reaction_emojis: [],
      is_muc: false,
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      timestamp: "2024-01-01T00:00:00Z",
    };
    const liveMessages = [archivedOriginal]
      .map((message) => dmMessageFromArchived(message, "alice@example.com"))
      .filter((message): message is LiveDmMessage => !!message);
    const sendDmReaction = mock(async () => undefined);
    const client = makeDmClient(liveMessages);
    (client as unknown as { value: Record<string, unknown> }).value = {
      ...(client as unknown as { value: Record<string, unknown> }).value,
      sendDmReaction,
    };
    const { dm } = makeDmMessaging(client);

    await dm.loadMessages("bob@example.com");
    await nextTick();
    await dm.toggleReaction(messageId, "👍");

    expect(sendDmReaction).toHaveBeenCalledTimes(1);
    expect(sendDmReaction.mock.calls[0]?.[0]).toBe("bob@example.com");
    expect(sendDmReaction.mock.calls[0]?.[1]).toBe(originId);
    expect(sendDmReaction.mock.calls[0]?.[2]).toEqual(["👍"]);
    const original = dm.messages.value.find((m) => m.body === "react to origin-id");
    expect(original?.reactions).toEqual({ "👍": ["alice"] });
  });

  test("DM toggleReaction optimistically updates the local timeline (the sender device gets no carbon)", async () => {
    const sendDmReaction = mock(async () => undefined);
    const client = makeDmClient();
    (client as unknown as { value: Record<string, unknown> }).value = {
      ...(client as unknown as { value: Record<string, unknown> }).value,
      sendDmReaction,
    };
    const { dm } = makeDmMessaging(client);

    dm.messages.value = [
      {
        id: "dm-target",
        author: "bob",
        body: "react to me",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await dm.toggleReaction("dm-target", "👍");

    const target = dm.messages.value.find((m) => m.id === "dm-target");
    expect(target?.reactions).toEqual({ "👍": ["alice"] });
    expect(sendDmReaction).toHaveBeenCalled();
  });

  test("DM onReaction attributes self-sent carbon-forwarded reactions to the sender, not the conversation partner", async () => {
    // Self-sent DM reactions arrive on the sender's other devices via
    // XEP-0280 carbons with `from = self`, `to = peer`. The dispatch
    // normalizes `peerJid` to the conversation key (peer), so without
    // a separate `reactorJid` field the UI would credit the partner
    // for the reaction. Verify `onReaction` uses `reactorJid`.
    const client = makeDmClient();
    const { dm } = makeDmMessaging(client);

    dm.messages.value = [
      {
        id: "dm-target",
        author: "alice",
        body: "react to me",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
      },
    ];

    dm.onReaction({
      peerJid: "bob@example.com",
      reactorJid: "alice@example.com",
      messageId: "dm-target",
      emojis: ["🎉"],
    });

    const target = dm.messages.value.find((m) => m.id === "dm-target");
    expect(target?.reactions).toEqual({ "🎉": ["alice"] });
  });

  test("DM toggleReaction rolls back the optimistic update when sendDmReaction rejects", async () => {
    const sendDmReaction = mock(async () => {
      throw new Error("session not ready");
    });
    const client = makeDmClient();
    (client as unknown as { value: Record<string, unknown> }).value = {
      ...(client as unknown as { value: Record<string, unknown> }).value,
      sendDmReaction,
    };
    const { dm, actionError } = makeDmMessaging(client);

    dm.messages.value = [
      {
        id: "dm-rollback",
        author: "bob",
        body: "react to me",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await dm.toggleReaction("dm-rollback", "👍");

    const target = dm.messages.value.find((m) => m.id === "dm-rollback");
    expect(target?.reactions).toBeUndefined();
    expect(actionError.value).not.toBe("");
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

    dm.onSessionLifecycle({ type: "fresh", catchup: { dmJids: [], roomJids: [] } });
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

    messaging.onSessionLifecycle({ type: "fresh", catchup: { dmJids: [], roomJids: [] } });
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
    const messaging = useChannelMessages(
      ref(currentSession),
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
      setAuthorityHandler() {},
      setPresenceHandler() {},
      setLastSeenHandler() {},
      setActivityHandler() {},
      setRoomAvatarHandler() {},
    } as never;
    await nextTick();

    await messaging.sendMessage("hello room");
    expect(messaging.messages.value[0]?.deliveryStatus).toBe("sending");

    messaging.onSessionLifecycle({ type: "fresh", catchup: { dmJids: [], roomJids: [] } });
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

  dm.onSessionLifecycle({ type: "fresh", catchup: { dmJids: [], roomJids: [] } });
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
