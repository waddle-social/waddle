import { describe, expect, test } from "bun:test";
import type { JSONContent } from "@tiptap/core";
import { mapLiveRoomMessageToTimeline } from "@/channels/timeline";
import { buildChannelTimelineFromMamResults } from "@/channels/message-timeline-state";
import { buildDmTimelineFromMamResults, fromLiveDmMessage } from "@/dms/message-timeline-state";
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

  test("maps archived LinkPreview payloads into room timeline messages", () => {
    const result = roomMessageFromArchived({
      ...baseArchivedRoom,
      link_previews: [{
        original_url: "https://example.com/article",
        normalized_url: "https://example.com/article",
        title: "Example Article",
        description: "Plain text summary",
        image: {
          url: "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
          media_type: "image/png",
          width: 640,
          height: 360,
          alt: "Article screenshot",
        },
      }],
    }, { trustedMediaOrigin: "https://waddle.example" });

    expect(result).not.toBeNull();
    const timeline = mapLiveRoomMessageToTimeline(session, result!);
    expect(timeline.linkPreviews).toEqual([{
      originalUrl: "https://example.com/article",
      normalizedUrl: "https://example.com/article",
      title: "Example Article",
      description: "Plain text summary",
      image: {
        url: "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
        mediaType: "image/png",
        width: 640,
        height: 360,
        alt: "Article screenshot",
      },
    }]);
  });

  test("maps bodyless archived LinkPreview payloads into room timeline messages", () => {
    const result = roomMessageFromArchived({
      ...baseArchivedRoom,
      body: undefined,
      link_previews: [{
        original_url: "https://example.com/article",
        normalized_url: "https://example.com/article",
        title: "Example Article",
        description: "Plain text summary",
      }],
    });

    expect(result).not.toBeNull();
    const timeline = mapLiveRoomMessageToTimeline(session, result!);
    expect(timeline.body).toBe("");
    expect(timeline.linkPreviews).toEqual([{
      originalUrl: "https://example.com/article",
      normalizedUrl: "https://example.com/article",
      title: "Example Article",
      description: "Plain text summary",
    }]);
  });

  test("maps remote-media-unavailable LinkPreview payloads into room timeline messages", () => {
    const result = roomMessageFromArchived({
      ...baseArchivedRoom,
      link_previews: [{
        original_url: "https://example.com/article",
        normalized_url: "https://example.com/article",
        title: "Example Article",
        description: "Plain text summary",
        remote_media_unavailable: true,
      }],
    });

    expect(result).not.toBeNull();
    const timeline = mapLiveRoomMessageToTimeline(session, result!);
    expect(timeline.linkPreviews).toEqual([{
      originalUrl: "https://example.com/article",
      normalizedUrl: "https://example.com/article",
      title: "Example Article",
      description: "Plain text summary",
      remoteMediaUnavailable: true,
    }]);
  });

  test("marks attacker-host cached-path LinkPreview images unavailable after trusted-origin stripping", () => {
    const result = roomMessageFromArchived({
      ...baseArchivedRoom,
      link_previews: [{
        original_url: "https://example.com/article",
        normalized_url: "https://example.com/article",
        title: "Example Article",
        image: {
          url: "https://attacker.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
          media_type: "image/png",
        },
      }],
    }, { trustedMediaOrigin: "https://waddle.example" });

    expect(result).not.toBeNull();
    const timeline = mapLiveRoomMessageToTimeline(session, result!);
    expect(timeline.linkPreviews).toEqual([{
      originalUrl: "https://example.com/article",
      normalizedUrl: "https://example.com/article",
      title: "Example Article",
      remoteMediaUnavailable: true,
    }]);
  });

  test("archive replay clears an optimistic room LinkPreview when payload has none", () => {
    const archived = roomMessageFromArchived({
      ...baseArchivedRoom,
      mam_id: "mam-reload-1",
      id: "server-1",
      stanza_id: "room-stanza-1",
      stanza_id_by: "room@conf.example.com",
      from: "room@conf.example.com/alice",
      body: "read https://example.com",
      link_previews: [],
    });

    expect(archived).not.toBeNull();
    const timeline = buildChannelTimelineFromMamResults({
      session,
      mamResults: [archived!],
      existing: [{
        id: "client-1",
        wireIds: ["room-stanza-1"],
        body: "read https://example.com",
        nick: "alice",
        isSelf: true,
        createdAt: "2026-06-01T12:00:00.000Z",
        linkPreviews: [{ originalUrl: "https://example.com", title: "Example" }],
      }],
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]?.linkPreviews).toBeUndefined();
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
  test("uses the account server stanza-id for DM MDS state", () => {
    const result = dmMessageFromArchived({
      ...baseArchivedDm,
      stanza_id: "foreign-single-id",
      stanza_id_by: "foreign.example.com",
      stanza_ids: [
        { id: "foreign-list-id", by: "foreign.example.com" },
        { id: "own-server-id", by: "example.com" },
      ],
    }, "bob@example.com");

    expect(result).not.toBeNull();
    expect(result?.stanzaId).toBe("own-server-id");
    expect(result?.stanzaIdBy).toBe("example.com");
  });

  test("stanza-id authority matching is case-folded like the seen-id path", () => {
    // Greptile P1: `rawMessageSeenIds` folds the trusted `by` via
    // bareJidKey; the decoded-row identity must fold identically or a
    // mixed-case authority (by="Example.COM") is recorded as seen while
    // the row's identity omits it → duplicate on reconnect.
    const result = dmMessageFromArchived({
      mam_id: "mam-case",
      id: "wire-case",
      from: "bob@example.com/desk",
      to: "alice@example.com",
      body: "mixed-case authority",
      message_type: "chat",
      timestamp: "2026-07-01T10:00:00Z",
      reaction_emojis: [],
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_muc: false,
      is_sticker: false,
      shared_files: [],
      link_previews: [],
      stanza_ids: [
        { id: "case-folded-id", by: "Example.COM" },
      ],
    } as unknown as WasmArchivedMessage, "alice@example.com");

    expect(result).not.toBeNull();
    expect(result?.stanzaId).toBe("case-folded-id");
    expect(result?.wireIds).toContain("case-folded-id");
  });

  test("does not expose a DM MDS stanza-id assigned only by a foreign domain", () => {
    const result = dmMessageFromArchived({
      ...baseArchivedDm,
      stanza_id: "foreign-single-id",
      stanza_id_by: "foreign.example.com",
    }, "bob@example.com");

    expect(result).not.toBeNull();
    expect(result?.stanzaId).toBeUndefined();
    expect(result?.stanzaIdBy).toBeUndefined();
  });

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

  test("maps archived LinkPreview payloads into DM timeline messages", () => {
    const result = dmMessageFromArchived({
      ...baseArchivedDm,
      link_previews: [{
        original_url: "https://example.com/article",
        normalized_url: "https://example.com/article",
        title: "Example Article",
        description: "Plain text summary",
        image: {
          url: "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
          media_type: "image/png",
          width: 640,
          height: 360,
          alt: "Article screenshot",
        },
      }],
    }, "bob@example.com", { trustedMediaOrigin: "https://waddle.example" });

    expect(result).not.toBeNull();
    const timeline = fromLiveDmMessage(session, result!);
    expect(timeline.linkPreviews).toEqual([{
      originalUrl: "https://example.com/article",
      normalizedUrl: "https://example.com/article",
      title: "Example Article",
      description: "Plain text summary",
      image: {
        url: "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
        mediaType: "image/png",
        width: 640,
        height: 360,
        alt: "Article screenshot",
      },
    }]);
  });

  test("maps bodyless archived LinkPreview payloads into DM timeline messages", () => {
    const result = dmMessageFromArchived({
      ...baseArchivedDm,
      body: undefined,
      link_previews: [{
        original_url: "https://example.com/article",
        normalized_url: "https://example.com/article",
        title: "Example Article",
        description: "Plain text summary",
      }],
    }, "bob@example.com");

    expect(result).not.toBeNull();
    const timeline = fromLiveDmMessage(session, result!);
    expect(timeline.body).toBe("");
    expect(timeline.linkPreviews).toEqual([{
      originalUrl: "https://example.com/article",
      normalizedUrl: "https://example.com/article",
      title: "Example Article",
      description: "Plain text summary",
    }]);
  });

  test("archive replay clears an optimistic DM LinkPreview when payload has none", () => {
    const archived = dmMessageFromArchived({
      ...baseArchivedDm,
      mam_id: "mam-dm-reload-1",
      id: "server-1",
      stanza_id: "dm-stanza-1",
      from: "alice@example.com/web",
      to: "bob@example.com",
      body: "read https://example.com",
      link_previews: [],
    }, "bob@example.com");

    expect(archived).not.toBeNull();
    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [archived!],
      existing: [{
        id: "client-1",
        wireIds: ["server-1", "dm-stanza-1", "mam-dm-reload-1"],
        body: "read https://example.com",
        nick: "alice",
        isSelf: true,
        createdAt: "2026-06-01T12:00:00.000Z",
        linkPreviews: [{ originalUrl: "https://example.com", title: "Example" }],
      }],
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]?.linkPreviews).toBeUndefined();
  });
});
