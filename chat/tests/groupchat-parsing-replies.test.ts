import { describe, test, expect } from "bun:test";
import { dispatchGroupchat, type GroupchatHandlers } from "../src/lib/xmpp/message-parsing";
import type { ReceivedMessage } from "stanza/protocol";
import type { LiveRoomMessage } from "../src/lib/xmpp/types";
import { codePointLength } from "../src/lib/text-offsets";

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

  test("keeps body-less file-sharing messages in the timeline", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "msg-file-only",
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

  test("extracts forum topic metadata from thread-create", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "topic-1",
        body: "Welcome aboard",
        threadCreate: { title: "Getting started" },
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].forumPostKind).toBe("topic");
    expect(h.messages[0].forumTitle).toBe("Getting started");
    expect(h.messages[0].forumThreadTitle).toBe("Getting started");
    expect(h.messages[0].threadId).toBe("topic-1");
  });

  test("extracts forum reply metadata from thread-reply", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "reply-1",
        body: "Sounds good",
        threadReply: { threadId: "topic-1" },
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].forumPostKind).toBe("reply");
    expect(h.messages[0].threadId).toBe("topic-1");
  });

  test("attaches encrypted file metadata to parsed shared files", () => {
    const h = makeHandlers();
    const encryptedUrl = "https://files.example.com/blob.enc";
    const encrypted = {
      cipher: "urn:xmpp:ciphers:aes-256-gcm-nopadding:0",
      keyB64: "a2V5",
      ivB64: "aXY=",
      hashes: [{ algo: "sha-256", valueB64: "aGFzaA==" }],
      sources: [encryptedUrl],
    };

    dispatchGroupchat(
      makeMsg({
        id: "msg-file",
        body: encryptedUrl,
        fileSharing: {
          name: "photo.jpg",
          mediaType: "image/jpeg",
          size: "42",
          url: encryptedUrl,
        },
        encryptedFiles: [encrypted],
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].sharedFiles).toEqual([
      {
        url: encryptedUrl,
        name: "photo.jpg",
        mediaType: "image/jpeg",
        size: 42,
        disposition: "inline",
        encrypted,
      },
    ]);
  });

  test("prefers stable stanza ids for rendered messages and keeps wire aliases", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "echo-1",
        body: "stable target",
        originId: { id: "client-1" },
        stanzaIds: [
          { id: "stable-1", by: "general@muc.waddle.social" },
          { id: "server-1", by: "waddle.social" },
        ],
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].id).toBe("stable-1");
    expect(h.messages[0].wireIds).toEqual(["echo-1", "client-1", "server-1"]);
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
    const prefixLength = codePointLength(prefix);

    dispatchGroupchat(
      makeMsg({
        id: "msg-5",
        body: `${prefix}${body}`,
        reply: { to: "general@muc.waddle.social/bob", id: "msg-1" },
        fallbacks: [{ for: "urn:xmpp:reply:0", body: { start: 0, end: prefixLength } }],
        markup: {
          spans: [
            { type: "span", start: prefixLength, end: prefixLength + codePointLength("bold"), styles: ["strong"] },
            { type: "span", start: prefixLength + codePointLength("bold "), end: prefixLength + codePointLength("bold code"), styles: ["code"] },
            { type: "span", start: prefixLength - codePointLength("hi\n\n"), end: prefixLength + codePointLength("bo"), styles: ["emphasis"] },
            { type: "span", start: codePointLength("> "), end: codePointLength("> 👋"), styles: ["deleted"] },
            { type: "span", start: prefixLength + codePointLength("bold"), end: prefixLength + codePointLength("bold"), styles: ["code"] },
          ],
        },
        references: [
          {
            type: "data",
            begin: String(prefixLength + codePointLength("bold code ")),
            end: String(prefixLength + codePointLength(body)),
            uri: "https://example.com/docs",
          },
        ],
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe(body);
    expect(h.messages[0].markup).toEqual([
      { type: "span", start: 0, end: codePointLength("bold"), styles: ["strong"] },
      { type: "span", start: codePointLength("bold "), end: codePointLength("bold code"), styles: ["code"] },
      { type: "span", start: 0, end: codePointLength("bo"), styles: ["emphasis"] },
    ]);
    expect(h.messages[0].references).toEqual([
      {
        type: "data",
        begin: codePointLength("bold code "),
        end: codePointLength(body),
        uri: "https://example.com/docs",
      },
    ]);
  });
});
