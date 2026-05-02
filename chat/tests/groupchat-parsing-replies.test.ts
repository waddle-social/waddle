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

describe("XEP-0461 replyableId", () => {
  test("groupchat message with a room-assigned stanza-id is replyable using that id", () => {
    // XEP-0461 §3.2: in groupchat, the id used in <reply id=...> MUST be the
    // id assigned by the room (XEP-0359 stanza-id whose `by` matches the
    // room JID).
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "wire-id",
        body: "hi",
        stanzaIds: [
          { id: "room-stanza-id", by: "general@muc.waddle.social" },
        ],
      }),
      h,
    );
    expect(h.messages[0].replyableId).toBe("room-stanza-id");
  });

  test("groupchat message without a room-assigned stanza-id is not replyable", () => {
    // XEP-0461 §3.2 closing line: "messages without one cannot be replied to".
    // Falling back to origin-id or the @id attribute would violate the spec.
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "wire-id",
        body: "hi",
        originId: { id: "origin-only" },
      }),
      h,
    );
    expect(h.messages[0].replyableId).toBeUndefined();
  });

  test("groupchat message with a non-room stanza-id is not replyable", () => {
    // A stanza-id stamped by some other entity (e.g., an upstream archive)
    // is not the one XEP-0461 wants — only the room's own stamp counts.
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "wire-id",
        body: "hi",
        stanzaIds: [{ id: "elsewhere-id", by: "archive@example.com" }],
      }),
      h,
    );
    expect(h.messages[0].replyableId).toBeUndefined();
  });
});

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

  test("XEP-0428: <fallback> with no children strips the entire body", () => {
    // Per XEP-0428 §3 the fallback applies to every <body/> when no children
    // are present. We treat the merged body+subject text as the displayable
    // string, so stripping the whole body produces an empty string.
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "msg-2",
        body: "this whole text is fallback",
        reply: { to: "general@muc.waddle.social/bob", id: "msg-1" },
        fallbacks: [{ for: "urn:xmpp:reply:0" }],
      }),
      h,
    );
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe("");
  });

  test("XEP-0428: <fallback><body/></fallback> with no start/end strips the entire body", () => {
    // "If start and end attribute are not supplied, the whole respective
    // message element should be assumed to be there for fallback purposes."
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "msg-2",
        body: "treat me as fallback",
        reply: { to: "general@muc.waddle.social/bob", id: "msg-1" },
        fallbacks: [{ for: "urn:xmpp:reply:0", body: {} }],
      }),
      h,
    );
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe("");
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

  test("keeps body-less extension enrichments with typed launch metadata in the timeline", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "msg-extension-only",
        waddleExtensions: {
          enrichments: [{
            id: "enrich-quiz",
            plugin: "pub-quiz",
            capability: "launch",
            payloadNamespace: "urn:waddle:pub-quiz:1",
            surface: "game",
            source: {
              stanzaId: "archive-id-quiz-1",
              by: "general@muc.waddle.social",
              bodyStart: "0",
              bodyEnd: "0",
            },
            payload: {
              elements: [{
                name: "quiz-question",
                attributes: {
                  xmlns: "urn:waddle:pub-quiz:1",
                  "game-id": "game-1",
                  "question-id": "q1",
                },
                children: [
                  { name: "prompt", attributes: {}, children: ["Which XEP defines Ad-Hoc Commands?"] },
                  { name: "choice", attributes: { id: "b" }, children: ["XEP-0050"] },
                ],
              }],
            },
            launches: [{
              id: "answer-b",
              plugin: "pub-quiz",
              action: "answer",
              commandNode: "urn:waddle:extension:1:invoke",
              token: "launch-token-answer-b",
              label: "Answer B",
              expiresAt: "2026-04-27T10:05:00Z",
              context: {
                waddleId: "waddle-123",
                room: "general@muc.waddle.social",
                stanzaId: "archive-id-quiz-1",
              },
              payload: {
                elements: [{
                  name: "answer-request",
                  attributes: {
                    xmlns: "urn:waddle:pub-quiz:1",
                    "game-id": "game-1",
                    "question-id": "q1",
                    "choice-id": "b",
                  },
                  children: [],
                }],
              },
            }],
          }],
        },
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe("");
    expect(h.messages[0].extensionAnnotations?.[0]).toMatchObject({
      extensionId: "pub-quiz",
      annotationId: "enrich-quiz",
      surfaceKind: "game",
      title: "Which XEP defines Ad-Hoc Commands?",
      payloadNamespace: "urn:waddle:pub-quiz:1",
      source: {
        stanzaId: "archive-id-quiz-1",
        by: "general@muc.waddle.social",
        bodyStart: 0,
        bodyEnd: 0,
      },
    });
    const action = h.messages[0].extensionAnnotations?.[0]?.actions[0];
    expect(action).toMatchObject({
      label: "Answer B",
      route: "answer-b",
      launch: {
        id: "answer-b",
        pluginId: "pub-quiz",
        actionId: "answer",
        commandNode: "urn:waddle:extension:1:invoke",
        launchToken: "launch-token-answer-b",
        expiresAt: "2026-04-27T10:05:00Z",
        context: {
          waddleId: "waddle-123",
          roomJid: "general@muc.waddle.social",
          stanzaId: "archive-id-quiz-1",
        },
      },
    });
    expect(action?.launch?.payloads[0]).toMatchObject({
      namespace: "urn:waddle:pub-quiz:1",
      name: "answer-request",
      attributes: {
        "choice-id": "b",
      },
    });
  });

  test("defaults sample payload namespaces to the generic message-card surface", () => {
    const samples = [
      ["urn:waddle:links-task-board:1", "link"],
      ["urn:waddle:pub-quiz:1", "quiz-question"],
      ["urn:waddle:ai-chatbot:1", "assistant-answer"],
      ["urn:waddle:ai-assistant-canvas:1", "canvas"],
      ["urn:waddle:decision-polls:1", "poll"],
    ] as const;

    for (const [namespace, element] of samples) {
      const h = makeHandlers();
      dispatchGroupchat(
        makeMsg({
          id: `msg-${element}`,
          waddleExtensions: {
            enrichments: [{
              id: `enrich-${element}`,
              plugin: element,
              capability: "message.enrich",
              payloadNamespace: namespace,
              payload: {
                elements: [{
                  name: element,
                  attributes: { xmlns: namespace, title: `${element} title` },
                  children: [],
                }],
              },
            }],
          },
        }),
        h,
      );

      expect(h.messages).toHaveLength(1);
      expect(h.messages[0].extensionAnnotations?.[0]?.surfaceKind).toBe("message-card");
      expect(h.messages[0].extensionAnnotations?.[0]?.payloads?.[0]?.namespace).toBe(namespace);
    }
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

  test("keeps bodyless XEP-0201 thread metadata in standard MUC timelines", () => {
    const h = makeHandlers();

    dispatchGroupchat(
      makeMsg({
        id: "thread-marker-1",
        body: "",
        thread: "thread-root",
        parentThread: "parent-root",
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0]).toMatchObject({
      id: "thread-marker-1",
      body: "",
      threadId: "thread-root",
      parentThreadId: "parent-root",
    });
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

  test("forum reply metadata replaces conflicting XEP thread", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "reply-1",
        body: "",
        thread: "conflicting-thread",
        threadReply: { threadId: "topic-1" },
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].forumPostKind).toBe("reply");
    expect(h.messages[0].threadId).toBe("topic-1");
  });

  test("ignores malformed bodyless forum metadata", () => {
    const h = makeHandlers();

    dispatchGroupchat(makeMsg({ id: "topic-1", body: "", threadCreate: {} }), h);
    dispatchGroupchat(makeMsg({ id: "topic-2", body: "", threadCreate: { title: 123 } }), h);
    dispatchGroupchat(makeMsg({ id: "reply-1", body: "", threadReply: {} }), h);
    dispatchGroupchat(makeMsg({ id: "reply-2", body: "", threadReply: { threadId: " " } }), h);
    dispatchGroupchat(makeMsg({ id: "reply-3", body: "", threadReply: { threadId: 123 } }), h);

    expect(h.messages).toHaveLength(0);
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
    expect(h.messages[0].reactionTargetId).toBe("stable-1");
    expect(h.messages[0].wireIds).toEqual(["echo-1", "client-1", "server-1"]);
    expect(h.messages[0].correctionTargetId).toBe("client-1");
  });

  test("uses the sender message id as the correction target when no origin id exists", () => {
    const h = makeHandlers();
    dispatchGroupchat(
      makeMsg({
        id: "echo-1",
        body: "editable",
        stanzaIds: [{ id: "stable-1", by: "general@muc.waddle.social" }],
      }),
      h,
    );

    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].id).toBe("stable-1");
    expect(h.messages[0].wireIds).toEqual(["echo-1"]);
    expect(h.messages[0].correctionTargetId).toBe("echo-1");
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

  test("strips fallback when fallbacks is a singleton object instead of array", () => {
    const h = makeHandlers();
    const prefix = "> quoted\n\n";
    dispatchGroupchat(
      makeMsg({
        id: "msg-singleton-fb",
        body: `${prefix}reply text`,
        reply: { to: "general@muc.waddle.social/bob", id: "msg-1" },
        fallbacks: { for: "urn:xmpp:reply:0", body: { start: 0, end: prefix.length } },
      }),
      h,
    );
    expect(h.messages).toHaveLength(1);
    expect(h.messages[0].body).toBe("reply text");
    expect(h.messages[0].replyTo?.id).toBe("msg-1");
  });
});
