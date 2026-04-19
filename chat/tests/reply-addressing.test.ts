import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
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

describe("reply addressing", () => {
  test("room replies serialize the parent occupant JID while keeping the reply UI label", async () => {
    const sendGroupMessage = mock(async () => ({ id: "reply-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useMessaging(
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
        author: "Friendly Bob",
        authorJid: "w1_c1@muc.example.com/Friendly Bob",
        body: "hello there",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.sendMessage("sounds good", [], undefined, {
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
          author: "w1_c1@muc.example.com/Friendly Bob",
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

  test("DM replies serialize the parent author JID while keeping the reply UI label", async () => {
    const sendDirectMessage = mock(async () => ({ id: "reply-1", state: "sending" as const }));
    const sendDmChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useDmMessaging(
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

    await messaging.sendMessage("sure!", undefined, undefined, {
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

  test("room sends drop stale reply context when the parent is not in the active timeline", async () => {
    const sendGroupMessage = mock(async () => ({ id: "reply-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useMessaging(
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
        authorJid: "w1_c2@muc.example.com/Carol",
        body: "different room",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
      },
    ];

    await messaging.sendMessage("fresh message", [], undefined, {
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
    const messaging = useDmMessaging(
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

    await messaging.sendMessage("fresh dm", undefined, undefined, {
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
