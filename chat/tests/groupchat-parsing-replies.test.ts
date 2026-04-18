import { describe, test, expect } from "bun:test";
import { dispatchGroupchat, type GroupchatHandlers } from "../src/lib/xmpp/message-parsing";
import type { ReceivedMessage } from "stanza/protocol";
import type { LiveRoomMessage } from "../src/lib/xmpp/types";

const encoder = new TextEncoder();

function byteLen(input: string): number {
  return encoder.encode(input).byteLength;
}

function makeHandlers(overrides?: Partial<GroupchatHandlers>): GroupchatHandlers & {
  messages: LiveRoomMessage[];
} {
  const messages: LiveRoomMessage[] = [];
  return {
    currentRoom: "general@muc.waddle.social",
    selfNick: "me",
    onMessage: (msg) => messages.push(msg),
    onChatState: null,
    onDisplayed: null,
    onReaction: null,
    onActivity: null,
    messages,
    ...overrides,
  };
}

function makeMsg(overrides: Record<string, unknown>): ReceivedMessage {
  return {
    from: "general@muc.waddle.social/alice",
    type: "groupchat",
    ...overrides,
  } as ReceivedMessage;
}

describe("groupchat reply + thread parsing", () => {
  test("extracts reply pointer into replyTo", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "msg-2",
        body: "yes",
        reply: { to: "general@muc.waddle.social/bob", id: "msg-1" },
      }),
      h,
    );
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].replyTo).toEqual({
      id: "msg-1",
      author: "general@muc.waddle.social/bob",
    });
  });

  test("strips the XEP-0428 reply fallback range from the displayed body", () => {
    const h = makeHandlers();
    const prefix = "> hi\n\n";
    dispatchGroupchat(
      makeMsg({
        id: "msg-2",
        body: `${prefix}actual reply`,
        reply: { to: "general@muc.waddle.social/bob", id: "msg-1" },
        fallbacks: [{ for: "urn:xmpp:reply:0", body: { start: 0, end: prefix.length } }],
      }),
      h,
    );
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe("actual reply");
    expect(h.messages[0].replyTo?.id).toBe("msg-1");
  });

  test("ignores fallbacks for other namespaces", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "msg-2",
        body: "https://example.com/file.jpg",
        fallbacks: [{ for: "urn:xmpp:sfs:0" }],
      }),
      h,
    );
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe("https://example.com/file.jpg");
  });

  test("extracts thread id and parent thread id", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "msg-3",
        body: "branch",
        thread: "thread-xyz",
        parentThread: "thread-root",
      }),
      h,
    );
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].threadId).toBe("thread-xyz");
    expect(h.messages[0].parentThreadId).toBe("thread-root");
  });

  test("leaves replyTo undefined when no reply element is present", () => {
    const h = makeHandlers();
    dispatchGroupchat(makeMsg({ id: "msg-4", body: "plain" }), h);
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].replyTo).toBeUndefined();
    expect(h.messages[0].threadId).toBeUndefined();
  });

  test("rebases rich markup after stripping the reply fallback prefix", () => {
    const h = makeHandlers();
    const body = "bold code link";
    const prefix = "> 👋 hi\n\n";
    const prefixBytes = byteLen(prefix);

    dispatchGroupchat(
      makeMsg({
        id: "msg-5",
        body: `${prefix}${body}`,
        reply: { to: "general@muc.waddle.social/bob", id: "msg-1" },
        fallbacks: [{ for: "urn:xmpp:reply:0", body: { start: 0, end: prefix.length } }],
        markup: {
          spans: [
            { type: "b", start: prefixBytes, end: prefixBytes + byteLen("bold") },
            { type: "code", start: prefixBytes + byteLen("bold "), end: prefixBytes + byteLen("bold code") },
            { type: "link", start: prefixBytes + byteLen("bold code "), end: prefixBytes + byteLen(body), uri: "https://example.com/docs" },
            { type: "i", start: prefixBytes - byteLen("hi\n\n"), end: prefixBytes + byteLen("bo") },
            { type: "s", start: byteLen("> "), end: byteLen("> 👋") },
            { type: "code", start: prefixBytes + byteLen("bold"), end: prefixBytes + byteLen("bold") },
          ],
        },
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe(body);
    expect(h.messages[0].markup).toEqual([
      { type: "b", start: 0, end: byteLen("bold") },
      { type: "code", start: byteLen("bold "), end: byteLen("bold code") },
      { type: "link", start: byteLen("bold code "), end: byteLen(body), uri: "https://example.com/docs" },
      { type: "i", start: 0, end: byteLen("bo") },
    ]);
  });
});
