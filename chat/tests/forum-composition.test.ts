import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import type { ChannelSummary } from "../src/lib/chat-types";
import type { WaddleSession } from "../src/lib/server-auth";
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

function forumChannel(partial: Partial<ChannelSummary> = {}): ChannelSummary {
  return {
    id: "c1",
    name: "roadmap",
    channel_type: "forum",
    ...partial,
  };
}

describe("forum composition", () => {
  test("top-level forum posts require a title and send thread-create metadata", async () => {
    const sendGroupMessage = mock(async () => ({ id: "topic-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref(forumChannel()),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.forumPostTitle.value = "Shipping roadmap";

    await messaging.sendMessage("Let's map the next release.", []);

    expect(sendGroupMessage).toHaveBeenCalledWith(
      "w1",
      "c1",
      "Let's map the next release.",
      expect.objectContaining({
        threadCreate: { title: "Shipping roadmap" },
      }),
    );
    expect(messaging.messages.value.at(-1)).toMatchObject({
      id: "topic-1",
      threadId: "topic-1",
      forumPostKind: "topic",
      forumTitle: "Shipping roadmap",
      forumThreadTitle: "Shipping roadmap",
    });
    expect(messaging.forumPostTitle.value).toBe("");
  });

  test("allows bodyless forum topics with title metadata", async () => {
    const sendGroupMessage = mock(async () => ({ id: "topic-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref(forumChannel()),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.forumPostTitle.value = "Shipping roadmap";

    await messaging.sendMessage("", []);

    expect(sendGroupMessage).toHaveBeenCalledWith(
      "w1",
      "c1",
      "",
      expect.objectContaining({
        threadCreate: { title: "Shipping roadmap" },
      }),
    );
  });

  test("blocks top-level forum posts without a title", async () => {
    const sendGroupMessage = mock(async () => ({ id: "topic-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref(forumChannel()),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.sendMessage("Untitled topic", []);

    expect(sendGroupMessage).not.toHaveBeenCalled();
    expect(actionError.value).toBe("Add a title before posting to this forum.");
  });

  test("forum replies send thread-reply metadata rooted at the topic", async () => {
    const sendGroupMessage = mock(async () => ({ id: "reply-2", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref(forumChannel()),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "topic-1",
        replyableId: "topic-1",
        author: "Bob",
        authorJid: "c1@muc.example.com/Bob",
        body: "Let's map the next release.",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: false,
        threadId: "topic-1",
        forumPostKind: "topic",
        forumTitle: "Shipping roadmap",
        forumThreadTitle: "Shipping roadmap",
      },
      {
        id: "reply-1",
        replyableId: "reply-1",
        author: "Carol",
        authorJid: "c1@muc.example.com/Carol",
        body: "Start with onboarding.",
        createdAt: "2024-01-01T00:01:00Z",
        isSelf: false,
        threadId: "topic-1",
        forumPostKind: "reply",
        forumThreadTitle: "Shipping roadmap",
      },
    ];

    await messaging.sendMessage("Agreed.", [], [], undefined, {
      id: "reply-1",
      author: "Carol",
      body: "Start with onboarding.",
    });

    expect(sendGroupMessage).toHaveBeenCalledWith(
      "w1",
      "c1",
      "Agreed.",
      expect.objectContaining({
        replyTo: {
          id: "reply-1",
          author: "c1@muc.example.com/Carol",
          body: "Start with onboarding.",
        },
        threadId: "topic-1",
        threadReply: { threadId: "topic-1" },
      }),
    );
    expect(messaging.messages.value.at(-1)).toMatchObject({
      id: "reply-2",
      forumPostKind: "reply",
      threadId: "topic-1",
      forumThreadTitle: "Shipping roadmap",
    });
  });

  test("allows bodyless forum replies with thread metadata", async () => {
    const sendGroupMessage = mock(async () => ({ id: "reply-2", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref(forumChannel()),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    messaging.messages.value = [{
      id: "topic-1",
      replyableId: "topic-1",
      author: "Bob",
      authorJid: "c1@muc.example.com/Bob",
      body: "",
      createdAt: "2024-01-01T00:00:00Z",
      isSelf: false,
      threadId: "topic-1",
      forumPostKind: "topic",
      forumTitle: "Shipping roadmap",
      forumThreadTitle: "Shipping roadmap",
    }];

    await messaging.sendMessage("", [], [], undefined, {
      id: "topic-1",
      author: "Bob",
    });

    expect(sendGroupMessage).toHaveBeenCalledWith(
      "w1",
      "c1",
      "",
      expect.objectContaining({
        threadId: "topic-1",
        threadReply: { threadId: "topic-1" },
      }),
    );
  });
});
