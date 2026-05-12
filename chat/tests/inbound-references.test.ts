import { describe, expect, test } from "bun:test";
import type { JSONContent } from "@tiptap/core";
import { mapLiveRoomMessageToTimeline } from "@/channels/timeline";
import { renderStyledBody } from "@/lib/chat-ui";
import { tiptapToRichMessage } from "@/lib/rich-message";
import {
  dmMessageFromArchived,
  roomMessageFromArchived,
} from "@/lib/xmpp/client";
import type { WaddleSession } from "@/lib/server-auth";
import type { WasmArchivedMessage } from "@/lib/xmpp/wasm-types";

function paragraph(...content: JSONContent[]): JSONContent {
  return { type: "paragraph", content };
}

function text(text: string): JSONContent {
  return { type: "text", text };
}

function doc(...content: JSONContent[]): JSONContent {
  return { type: "doc", content };
}

const session: WaddleSession = {
  session_id: "session-1",
  user_id: "alice-id",
  username: "alice",
  avatar_url: null,
  xmpp_localpart: "alice",
  jid: "alice@example.com/web",
  xmpp_websocket_url: "wss://example.com/ws",
  is_expired: false,
  expires_at: null,
};

const baseArchivedRoom: WasmArchivedMessage = {
  mam_id: "mam-1",
  message_type: "groupchat",
  from: "room@conf.example.com/alice",
  to: "bob@example.com",
  body: "see https://example.com",
  reaction_emojis: [],
  is_muc: true,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

const baseArchivedDm: WasmArchivedMessage = {
  mam_id: "mam-dm-1",
  message_type: "chat",
  from: "alice@example.com/web",
  to: "bob@example.com",
  body: "see https://example.com",
  reaction_emojis: [],
  is_muc: false,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

describe("roomMessageFromArchived", () => {
  test("keeps editor autolink XEP-0372 references clickable after live echo and MAM reload", () => {
    const outbound = tiptapToRichMessage(doc(paragraph(text("see https://example.com today"))));
    expect(outbound.references).toEqual([
      { type: "data", uri: "https://example.com/", begin: 4, end: 23 },
    ]);

    const wasmReferences = outbound.references.map((reference) => ({
      ref_type: reference.type,
      uri: reference.uri,
      begin: reference.begin ?? 0,
      end: reference.end ?? 0,
      ...(reference.anchor ? { anchor: reference.anchor } : {}),
    }));
    const liveEcho = roomMessageFromArchived({
      ...baseArchivedRoom,
      mam_id: "live-echo",
      id: "client-1",
      stanza_id: "room-stanza-1",
      stanza_id_by: "room@conf.example.com",
      from: "room@conf.example.com/alice",
      body: outbound.body,
      references: wasmReferences,
    });
    const mamReload = roomMessageFromArchived({
      ...baseArchivedRoom,
      mam_id: "mam-reload-1",
      id: "server-1",
      stanza_id: "room-stanza-1",
      stanza_id_by: "room@conf.example.com",
      from: "room@conf.example.com/alice",
      body: outbound.body,
      references: wasmReferences,
    });

    for (const message of [liveEcho, mamReload]) {
      expect(message).not.toBeNull();
      const timeline = mapLiveRoomMessageToTimeline(session, message!, () => undefined);
      expect(timeline.references).toEqual(outbound.references);
      const html = renderStyledBody(timeline.body, timeline.markup, timeline.references);
      expect(html).toContain('<a href="https://example.com/"');
      expect(html).toContain(">https://example.com</a>");
    }
  });

  test("maps XEP-0372 references with anchor onto LiveRoomMessage.references", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      references: [
        {
          ref_type: "data",
          uri: "https://example.com",
          begin: 4,
          end: 23,
          anchor: "https://example.com",
        },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result).not.toBeNull();
    expect(result?.references).toEqual([
      {
        type: "data",
        uri: "https://example.com",
        begin: 4,
        end: 23,
        anchor: "https://example.com",
      },
    ]);
  });

  test("omits anchor when absent", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      references: [
        { ref_type: "mention", uri: "xmpp:bob@example.com", begin: 0, end: 4 },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result?.references).toEqual([
      { type: "mention", uri: "xmpp:bob@example.com", begin: 0, end: 4 },
    ]);
  });

  test("leaves references undefined when WASM payload has none", () => {
    const result = roomMessageFromArchived(baseArchivedRoom);
    expect(result?.references).toBeUndefined();
  });

  test("maps Waddle extension envelopes onto room message annotations", () => {
    const result = roomMessageFromArchived({
      ...baseArchivedRoom,
      body: "GitHub waddle-social/waddle: ci completed with failure",
      extension_envelope: {
        version: 1,
        enrichments: [{
          id: "github-delivery-1",
          plugin: "github",
          capability: "message.enrich",
          payload_namespace: "urn:waddle:web-integration:1",
          created: "2026-05-12T00:00:00Z",
          source: { stanza_id: "room-stanza-1" },
          title: "GitHub",
          summary: "GitHub waddle-social/waddle: ci completed with failure",
          payloads: [{
            namespace: "urn:waddle:web-integration:1",
            name: "github-event",
            attributes: [
              { name: "event-type", value: "workflow_run" },
              { name: "repository", value: "waddle-social/waddle" },
              { name: "conclusion", value: "failure" },
              { name: "name", value: "ci" },
            ],
            children: [],
          }],
          launches: [{
            id: "retry-1",
            plugin: "github",
            action: "retry",
            command_node: "urn:waddle:extension:1:invoke",
            label: "Retry",
            context: {
              waddle_id: "github-delivery-1",
              room: "room@conf.example.com",
              source_stanza_id: "room-stanza-1",
            },
            payloads: [],
            expires_at: "2026-05-12T00:05:00Z",
            token: "signed-token",
          }],
        }],
      },
      extension_body_fallback: true,
    });

    expect(result?.extensionBodyFallback).toBe(true);
    expect(result?.extensionAnnotations).toEqual([
      expect.objectContaining({
        extensionId: "github",
        annotationId: "github-delivery-1",
        surfaceKind: "message-card",
        title: "GitHub",
        payloadNamespace: "urn:waddle:web-integration:1",
        fields: expect.objectContaining({
          repository: "waddle-social/waddle",
          conclusion: "failure",
        }),
        actions: [expect.objectContaining({
          label: "Retry",
          launch: expect.objectContaining({
            id: "retry-1",
            launchToken: "signed-token",
          }),
        })],
      }),
    ]);
  });

  test("keeps extension fallback body when WASM exposes no renderable annotation", () => {
    const result = roomMessageFromArchived({
      ...baseArchivedRoom,
      body: "GitHub waddle-social/waddle: ci completed with failure",
      extension_body_fallback: true,
    });

    expect(result?.extensionAnnotations).toBeUndefined();
    expect(result?.extensionBodyFallback).toBeUndefined();
    expect(result?.body).toBe("GitHub waddle-social/waddle: ci completed with failure");
  });

  test("derives reactionTargetId from the room-assigned XEP-0359 stanza-id", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      id: "client-origin-id",
      stanza_id: "foreign-stanza-xyz",
      stanza_id_by: "archive.example.com",
      stanza_ids: [
        { id: "foreign-stanza-xyz", by: "archive.example.com" },
        { id: "room-stanza-xyz", by: "room@conf.example.com" },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result?.reactionTargetId).toBe("room-stanza-xyz");
    expect(result?.replyableId).toBe("room-stanza-xyz");
    expect(result?.wireIds).toContain("room-stanza-xyz");
    expect(result?.wireIds).not.toContain("foreign-stanza-xyz");
  });

  test("leaves room-targeted ids undefined when the room did not assign the stanza-id", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      id: "client-origin-id",
      stanza_id: "foreign-stanza-xyz",
      stanza_id_by: "example.com",
    };

    const result = roomMessageFromArchived(archived);

    expect(result?.reactionTargetId).toBeUndefined();
    expect(result?.replyableId).toBeUndefined();
    expect(result?.wireIds).toBeUndefined();
  });

  test("requires an exact room JID on XEP-0359 stanza-id by", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      id: "client-origin-id",
      stanza_id: "resource-scoped-stanza",
      stanza_id_by: "room@conf.example.com/alice",
      stanza_ids: [
        { id: "resource-scoped-stanza", by: "room@conf.example.com/alice" },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result?.reactionTargetId).toBeUndefined();
    expect(result?.replyableId).toBeUndefined();
    expect(result?.wireIds).toBeUndefined();
  });

  test("maps XEP-0424 tombstones without projecting a new retraction target", () => {
    const archivedBase = { ...baseArchivedRoom };
    delete archivedBase.body;
    const archived: WasmArchivedMessage = {
      ...archivedBase,
      id: "original-message-id",
      is_retracted: true,
      retraction_id: "retract-message-id",
    };

    const result = roomMessageFromArchived(archived);

    expect(result).toMatchObject({
      id: "original-message-id",
      body: "",
      isRetracted: true,
      retractionId: "retract-message-id",
    });
    expect(result?.retractsId).toBeUndefined();
  });

  test("converts an archived reaction stanza into a marker with _reactionTarget set", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      id: "reaction-msg-id",
      stanza_id: "reaction-stanza-id",
      from: "room@conf.example.com/bob",
      body: "",
      reaction_target_id: "original-room-stanza-id",
      reaction_emojis: ["👍", "🎉"],
      author_real_jid: "bob@example.com",
    };

    const result = roomMessageFromArchived(archived);

    expect(result?._reactionTarget).toBe("original-room-stanza-id");
    expect(result?._reactionEmojis).toEqual(["👍", "🎉"]);
    expect(result?._reactionSenderId).toBe("bob@example.com");
    expect(result?.nick).toBe("bob");
  });
});

describe("roomMessageFromArchived with reply-fallback", () => {
  test("preserves anchor-only (0, 0) reference intact when stripping fallback", () => {
    // Inbound: external client sent an anchor-only reference (no body
    // position, omitted begin/end → parsed as (0, 0)) on a reply that has a
    // fallback prefix. stripReplyFallback must NOT delete the reference.
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      body: "> quoted\n\nactual reply",
      reply_fallback_start: 0,
      reply_fallback_end: 10,
      references: [
        {
          ref_type: "data",
          uri: "xmpp:room@conf.example?message;id=earlier",
          begin: 0,
          end: 0,
          anchor: "xmpp:alice@example.com",
        },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result?.references).toEqual([
      {
        type: "data",
        uri: "xmpp:room@conf.example?message;id=earlier",
        begin: 0,
        end: 0,
        anchor: "xmpp:alice@example.com",
      },
    ]);
  });
});

describe("dmMessageFromArchived", () => {
  test("maps XEP-0372 references onto LiveDmMessage.references", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedDm,
      references: [
        {
          ref_type: "data",
          uri: "https://example.com",
          begin: 4,
          end: 23,
        },
      ],
    };

    const result = dmMessageFromArchived(archived, "bob@example.com");

    expect(result).not.toBeNull();
    expect(result?.references).toEqual([
      {
        type: "data",
        uri: "https://example.com",
        begin: 4,
        end: 23,
      },
    ]);
  });
});
