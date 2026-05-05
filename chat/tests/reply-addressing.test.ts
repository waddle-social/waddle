import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
import { useDirectMessages } from "../src/dms/messages";
import { useChannelMessages } from "../src/channels/messages";
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

describe("reply addressing", () => {
  test("room replies serialize the parent occupant JID while keeping the reply UI label", async () => {
    const sendGroupMessage = mock(async () => ({ id: "reply-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "parent-1",
        // XEP-0461 §3.2: groupchat replies require the room-assigned
        // stanza-id; a typical room-stamped message carries replyableId.
        replyableId: "parent-1",
        author: "Friendly Bob",
        authorJid: "c1@muc.example.com/Friendly Bob",
        body: "hello there",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.sendMessage("sounds good", [], [], undefined, {
      id: "parent-1",
      author: "Friendly Bob",
      body: "hello there",
    });

    expect(sendGroupMessage).toHaveBeenCalledWith(
      "w1",
      "c1",
      "sounds good",
      expect.objectContaining({
        replyTo: {
          id: "parent-1",
          author: "c1@muc.example.com/Friendly Bob",
          body: "hello there",
        },
      }),
    );
    expect(messaging.messages.value.at(-1)?.replyTo).toEqual({
      id: "parent-1",
      author: "Friendly Bob",
      preview: "hello there",
    });
  });

  test("room replies prefer the parent occupant JID when a real bare JID is known", async () => {
    const sendGroupMessage = mock(async () => ({ id: "reply-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "client-id-1",
        replyableId: "room-stanza-1",
        author: "Friendly Bob",
        authorJid: "bob@example.com",
        authorOccupantJid: "c1@muc.example.com/Friendly Bob",
        body: "hello there",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.sendMessage("sounds good", [], [], undefined, {
      id: "client-id-1",
      author: "Friendly Bob",
      body: "hello there",
    });

    expect(sendGroupMessage).toHaveBeenCalledWith(
      "w1",
      "c1",
      "sounds good",
      expect.objectContaining({
        replyTo: {
          id: "room-stanza-1",
          author: "c1@muc.example.com/Friendly Bob",
          body: "hello there",
        },
      }),
    );
  });

  test("DM replies serialize the parent author JID while keeping the reply UI label", async () => {
    const sendDirectMessage = mock(async () => ({ id: "reply-1", state: "sending" as const }));
    const sendDmChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useDirectMessages(
      ref(session()),
      ref({ sendDirectMessage, sendDmChatState } as never),
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "parent-1",
        author: "Bob",
        authorJid: "bob@example.com/mobile",
        body: "want to grab lunch?",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.sendMessage("sure!", undefined, undefined, undefined, {
      id: "parent-1",
      author: "Bob",
      body: "want to grab lunch?",
    });

    expect(sendDirectMessage).toHaveBeenCalledWith(
      "bob@example.com",
      "sure!",
      expect.objectContaining({
        replyTo: {
          id: "parent-1",
          author: "bob@example.com/mobile",
          body: "want to grab lunch?",
        },
      }),
    );
    expect(messaging.messages.value.at(-1)?.replyTo).toEqual({
      id: "parent-1",
      author: "Bob",
      preview: "want to grab lunch?",
    });
  });

  test("XEP-0461: refuses to send a groupchat reply when the parent has no room-assigned stanza-id", async () => {
    // XEP-0461 §3.2 closing line: "messages without one cannot be replied
    // to". Refuse the send and surface a non-blocking error rather than
    // leak a non-conformant id (origin-id or @id).
    const sendGroupMessage = mock(async () => ({ id: "reply-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        // No replyableId: this parent came from a non-conformant room or a
        // pre-stanza-id history slice.
        id: "parent-no-stanza-id",
        author: "Friendly Bob",
        authorJid: "c1@muc.example.com/Friendly Bob",
        body: "hello there",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.sendMessage("sounds good", [], [], undefined, {
      id: "parent-no-stanza-id",
      author: "Friendly Bob",
      body: "hello there",
    });

    expect(sendGroupMessage).not.toHaveBeenCalled();
    expect(actionError.value).toContain("can't be replied to");
  });

  test("room sends drop stale reply context when the parent is not in the active timeline", async () => {
    const sendGroupMessage = mock(async () => ({ id: "reply-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c2"),
      ref({ id: "c2", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "current-1",
        author: "Carol",
        authorJid: "c2@muc.example.com/Carol",
        body: "different room",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.sendMessage("fresh message", [], [], undefined, {
      id: "stale-parent",
      author: "Friendly Bob",
      body: "hello there",
    });

    expect(sendGroupMessage).toHaveBeenCalledWith("w1", "c2", "fresh message", expect.any(Object));
    const options = sendGroupMessage.mock.calls[0]?.[3] as Record<string, unknown>;
    expect(options.replyTo).toBeUndefined();
    expect(options.threadId).toBeUndefined();
    expect(messaging.messages.value.at(-1)?.replyTo).toBeUndefined();
    expect(messaging.messages.value.at(-1)?.threadId).toBeUndefined();
  });

  test("DM sends drop stale reply context when the parent is not in the active timeline", async () => {
    const sendDirectMessage = mock(async () => ({ id: "reply-1", state: "sending" as const }));
    const sendDmChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useDirectMessages(
      ref(session()),
      ref({ sendDirectMessage, sendDmChatState } as never),
      ref("carol@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "current-1",
        author: "Carol",
        authorJid: "carol@example.com/phone",
        body: "different DM",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.sendMessage("fresh dm", undefined, undefined, undefined, {
      id: "stale-parent",
      author: "Bob",
      body: "want to grab lunch?",
    });

    expect(sendDirectMessage).toHaveBeenCalledWith("carol@example.com", "fresh dm", expect.any(Object));
    const options = sendDirectMessage.mock.calls[0]?.[2] as Record<string, unknown>;
    expect(options.replyTo).toBeUndefined();
    expect(options.threadId).toBeUndefined();
    expect(messaging.messages.value.at(-1)?.replyTo).toBeUndefined();
    expect(messaging.messages.value.at(-1)?.threadId).toBeUndefined();
  });
});

describe("room stanza-id targeting", () => {
  test("room retraction and moderation target the room-assigned stanza-id", async () => {
    const sendRetraction = mock(async () => undefined);
    const sendModeration = mock(async () => undefined);
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendRetraction, sendModeration, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "client-id-1",
        replyableId: "room-stanza-1",
        author: "Friendly Bob",
        authorJid: "c1@muc.example.com/Friendly Bob",
        body: "hello there",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.retractMessage("client-id-1");
    await messaging.moderateMessage("client-id-1", "spam");

    expect(sendRetraction).toHaveBeenCalledWith("w1", "c1", "room-stanza-1");
    expect(sendModeration).toHaveBeenCalledWith("w1", "c1", "room-stanza-1", "spam");
  });

  test("room retraction and moderation refuse messages without a room stanza-id", async () => {
    const sendRetraction = mock(async () => undefined);
    const sendModeration = mock(async () => undefined);
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendRetraction, sendModeration, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "client-id-1",
        author: "Friendly Bob",
        authorJid: "c1@muc.example.com/Friendly Bob",
        body: "hello there",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.retractMessage("client-id-1");
    expect(sendRetraction).not.toHaveBeenCalled();
    expect(actionError.value).toContain("can't be retracted");

    await messaging.moderateMessage("client-id-1", "spam");
    expect(sendModeration).not.toHaveBeenCalled();
    expect(actionError.value).toContain("can't be moderated");
  });
});
