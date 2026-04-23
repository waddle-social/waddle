import { describe, test, expect, mock } from "bun:test";
import { dispatchChat, type DmHandlers } from "../src/lib/xmpp/dm-parsing";
import type { ReceivedMessage } from "stanza/protocol";
import type { LiveDmMessage, DmChatStateEvent, DmDisplayedEvent, DmReactionEvent } from "../src/lib/xmpp/types";
import { codePointLength } from "../src/lib/text-offsets";

function makeHandlers(overrides?: Partial<DmHandlers>): DmHandlers & {
  messages: LiveDmMessage[];
  chatStates: DmChatStateEvent[];
  displayed: DmDisplayedEvent[];
  reactions: DmReactionEvent[];
} {
  const messages: LiveDmMessage[] = [];
  const chatStates: DmChatStateEvent[] = [];
  const displayed: DmDisplayedEvent[] = [];
  const reactions: DmReactionEvent[] = [];
  return {
    selfBareJid: "alice@example.com",
    onMessage: (msg) => messages.push(msg),
    onChatState: (event) => chatStates.push(event),
    onDisplayed: (event) => displayed.push(event),
    onReaction: (event) => reactions.push(event),
    messages,
    chatStates,
    displayed,
    reactions,
    ...overrides,
  };
}

function makeMsg(overrides: Partial<ReceivedMessage>): ReceivedMessage {
  return {
    from: "bob@example.com/web",
    to: "alice@example.com/web",
    type: "chat",
    ...overrides,
  } as ReceivedMessage;
}

describe("dispatchChat", () => {
  test("dispatches a basic incoming message", () => {
    const h = makeHandlers();
    dispatchChat(makeMsg({ id: "msg-1", body: "hello" }), h);
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].id).toBe("msg-1");
    expect(h.messages[0].body).toBe("hello");
    expect(h.messages[0].peerJid).toBe("bob@example.com");
    expect(h.messages[0].nick).toBe("bob");
  });

  test("identifies self-sent messages correctly", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({
        id: "msg-2",
        from: "alice@example.com/web",
        to: "bob@example.com/web",
        body: "from me",
      }),
      h,
    );
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].peerJid).toBe("bob@example.com");
    expect(h.messages[0].fromJid).toBe("alice@example.com/web");
  });

  test("filters out MUC messages by domain", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({ from: "room@muc.example.com/nick", body: "group msg" }),
      h,
    );
    expect(h.messages).toHaveLength(0);
  });

  test("filters out messages to MUC addresses", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({ to: "room@muc.example.com", body: "group msg" }),
      h,
    );
    expect(h.messages).toHaveLength(0);
  });

  test("does not filter non-MUC domains that happen to contain 'muc'", () => {
    const h = makeHandlers({ selfBareJid: "alice@chat.example.com" });
    dispatchChat(
      makeMsg({
        from: "bob@education.example.com/web",
        to: "alice@chat.example.com/web",
        body: "hi",
      }),
      h,
    );
    expect(h.messages).toHaveLength(1);
  });

  test("ignores groupchat type messages", () => {
    const h = makeHandlers();
    dispatchChat(makeMsg({ type: "groupchat", body: "hello" }), h);
    expect(h.messages).toHaveLength(0);
  });

  test("dispatches chat state events from peers", () => {
    const h = makeHandlers();
    dispatchChat(makeMsg({ chatState: "composing" }), h);
    expect(h.chatStates).toHaveLength(1);
    expect(h.chatStates[0].state).toBe("composing");
    expect(h.chatStates[0].peerJid).toBe("bob@example.com");
  });

  test("does not dispatch chat state events from self", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({
        from: "alice@example.com/web",
        to: "bob@example.com/web",
        chatState: "composing",
      }),
      h,
    );
    expect(h.chatStates).toHaveLength(0);
  });

  test("dispatches displayed markers from peers", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({ marker: { type: "displayed", id: "msg-1" } }),
      h,
    );
    expect(h.displayed).toHaveLength(1);
    expect(h.displayed[0].messageId).toBe("msg-1");
    expect(h.displayed[0].peerJid).toBe("bob@example.com");
  });

  test("does not dispatch displayed markers from self", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({
        from: "alice@example.com/web",
        to: "bob@example.com/web",
        marker: { type: "displayed", id: "msg-1" },
      }),
      h,
    );
    expect(h.displayed).toHaveLength(0);
  });

  test("dispatches retraction messages", () => {
    const h = makeHandlers();
    const msg = makeMsg({ id: "ret-1" });
    (msg as Record<string, unknown>).retract = { id: "original-msg" };
    dispatchChat(msg, h);
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].retractsId).toBe("original-msg");
    expect(h.messages[0].body).toBe("");
  });

  test("dispatches reaction events", () => {
    const h = makeHandlers();
    const msg = makeMsg({ id: "react-1" });
    (msg as Record<string, unknown>).reactions = { id: "target-msg", items: ["👍", "❤️"] };
    dispatchChat(msg, h);
    expect(h.reactions).toHaveLength(1);
    expect(h.reactions[0].messageId).toBe("target-msg");
    expect(h.reactions[0].emojis).toEqual(["👍", "❤️"]);
  });

  test("prefers stable stanza ids for direct messages and keeps wire aliases", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({
        id: "echo-1",
        body: "hey",
        originId: { id: "client-1" },
        stanzaIds: [{ id: "stable-1", by: "example.com" }],
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].id).toBe("stable-1");
    expect(h.messages[0].wireIds).toEqual(["echo-1", "client-1"]);
  });

  test("dispatches message corrections", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({ id: "corr-1", body: "updated text", replace: "original-id" }),
      h,
    );
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].replacesId).toBe("original-id");
    expect(h.messages[0].body).toBe("updated text");
  });

  test("dispatches body-less file-sharing messages", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({
        id: "dm-file-1",
        fileSharing: {
          name: "photo.jpg",
          mediaType: "image/jpeg",
          size: "42",
          url: "https://files.example.com/photo.jpg",
        },
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe("");
    expect(h.messages[0].sharedFiles).toEqual([
      {
        url: "https://files.example.com/photo.jpg",
        name: "photo.jpg",
        mediaType: "image/jpeg",
        size: 42,
        disposition: "inline",
      },
    ]);
  });

  test("ignores messages with no body/subject/replace", () => {
    const h = makeHandlers();
    dispatchChat(makeMsg({}), h);
    expect(h.messages).toHaveLength(0);
  });

  test("ignores messages with missing from/to", () => {
    const h = makeHandlers();
    dispatchChat(makeMsg({ from: undefined, body: "hi" }), h);
    expect(h.messages).toHaveLength(0);
    dispatchChat(makeMsg({ to: undefined, body: "hi" }), h);
    expect(h.messages).toHaveLength(0);
  });

  test("preserves the full author JID while normalizing the conversation peer", () => {
    const h = makeHandlers();
    dispatchChat(
      makeMsg({ id: "r-1", from: "bob@example.com/mobile-xyz", body: "hey" }),
      h,
    );
    expect(h.messages[0].peerJid).toBe("bob@example.com");
    expect(h.messages[0].fromJid).toBe("bob@example.com/mobile-xyz");
  });

  test("strips reply fallbacks and rebases incoming DM markup", () => {
    const h = makeHandlers();
    const body = "reply code";
    const prefix = "> 👋 hi\n\n";
    const prefixLength = codePointLength(prefix);

    dispatchChat(
      makeMsg({
        id: "reply-1",
        body: `${prefix}${body}`,
        reply: { to: "bob@example.com", id: "dm-1" },
        fallbacks: [{ for: "urn:xmpp:reply:0", body: { start: 0, end: prefixLength } }],
        markup: {
          spans: [
            { type: "span", start: prefixLength + codePointLength("reply "), end: prefixLength + codePointLength(body), styles: ["code"] },
          ],
        },
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe(body);
    expect(h.messages[0].replyTo).toEqual({ id: "dm-1", author: "bob@example.com" });
    expect(h.messages[0].markup).toEqual([
      { type: "span", start: codePointLength("reply "), end: codePointLength(body), styles: ["code"] },
    ]);
  });
});
