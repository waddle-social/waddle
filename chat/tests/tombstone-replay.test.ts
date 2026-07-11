import { describe, expect, test } from "bun:test";
import { buildChannelTimelineFromMamResults } from "../src/channels/message-timeline-state";
import { buildDmTimelineFromMamResults } from "../src/dms/message-timeline-state";
import type { ExtensionAnnotation, TimelineMessage } from "../src/lib/chat-ui";
import type { WaddleSession } from "../src/lib/server-auth";
import type { LiveDmMessage, LiveRoomMessage } from "../src/lib/xmpp-client";

const session: WaddleSession = {
  username: "alice",
  jid: "alice@example.com/desktop",
  session_id: "session-1",
  user_id: "user-1",
  avatar_url: null,
  xmpp_localpart: "alice",
  xmpp_websocket_url: "wss://example.com/xmpp",
  is_expired: false,
  expires_at: null,
};

const extensionAnnotation: ExtensionAnnotation = {
  extensionId: "github",
  annotationId: "github-delivery-1",
  surfaceKind: "message-card",
  title: "GitHub",
  fields: { repository: "waddle-social/waddle", conclusion: "failure" },
  actions: [],
};

describe("XEP-0424 tombstone replay", () => {
  test("folds archived call-thread-ended fastenings into their call anchors", () => {
    const anchor: LiveRoomMessage = {
      id: "anchor-message",
      wireIds: ["anchor-origin-id"],
      replyableId: "anchor-origin-id",
      stanzaId: "anchor-room-stanza-id",
      roomJid: "room@muc.example.com",
      nick: "alice",
      body: "Alice started a call",
      createdAt: "2026-05-08T13:00:00Z",
      type: "message",
      callThread: {
        kind: "muc",
        sid: "call-thread-uuid",
        media: ["audio"],
        initiator: "room@muc.example.com/alice",
        started: "2026-05-08T13:00:00Z",
      },
    };
    const ended: LiveRoomMessage = {
      id: "ended-message",
      roomJid: "room@muc.example.com",
      nick: "room",
      body: "",
      createdAt: "2026-05-08T13:05:00Z",
      type: "message",
      callThreadEnded: {
        anchorId: "anchor-origin-id",
        ended: "2026-05-08T13:05:00Z",
        duration: "PT5M",
      },
    };

    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [anchor, ended],
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]?.callThread).toEqual({
      kind: "muc",
      sid: "call-thread-uuid",
      media: ["audio"],
      initiator: "room@muc.example.com/alice",
      started: "2026-05-08T13:00:00Z",
      ended: "2026-05-08T13:05:00Z",
      duration: "PT5M",
    });
  });

  test("updates an already-loaded channel message from a MAM tombstone", () => {
    const existing: TimelineMessage = {
      id: "original-message",
      wireIds: ["room-stanza-1"],
      replyableId: "room-stanza-1",
      author: "bob",
      authorJid: "room@muc.example.com/bob",
      authorOccupantJid: "room@muc.example.com/bob",
      body: "remove this payload",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      markup: [{ type: "span", start: 0, end: 6, styles: ["strong"] }],
      references: [{ type: "data", uri: "https://example.com", begin: 7, end: 26 }],
      sharedFiles: [{ url: "https://example.com/file.png", disposition: "inline" }],
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
      isSticker: true,
      mentions: ["bob@example.com"],
      broadcastMention: "here",
    };
    const tombstone: LiveRoomMessage = {
      id: "original-message",
      wireIds: ["room-stanza-1"],
      roomJid: "room@muc.example.com",
      nick: "bob",
      body: "",
      createdAt: "2026-05-08T13:00:00Z",
      type: "message",
      isRetracted: true,
      retractionId: "retract-message",
    };

    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [tombstone],
      existing: [existing],
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toMatchObject({
      id: "original-message",
      body: "",
      isRetracted: true,
      retractionId: "retract-message",
    });
    expect(timeline[0]?.markup).toBeUndefined();
    expect(timeline[0]?.references).toBeUndefined();
    expect(timeline[0]?.sharedFiles).toBeUndefined();
    expect(timeline[0]?.extensionAnnotations).toBeUndefined();
    expect(timeline[0]?.extensionBodyFallback).toBeUndefined();
    expect(timeline[0]?.isSticker).toBeUndefined();
    expect(timeline[0]?.mentions).toBeUndefined();
    expect(timeline[0]?.broadcastMention).toBeUndefined();
  });

  test("scrubs a fresh channel tombstone before insertion", () => {
    const tombstone: LiveRoomMessage = {
      id: "standalone-tombstone",
      wireIds: ["standalone-room-stanza"],
      roomJid: "room@muc.example.com",
      nick: "bob",
      body: "",
      createdAt: "2026-05-08T13:00:00Z",
      type: "message",
      isRetracted: true,
      retractionId: "retract-message",
      markup: [{ type: "span", start: 0, end: 6, styles: ["strong"] }],
      references: [{ type: "data", uri: "https://example.com", begin: 7, end: 26 }],
      sharedFiles: [{ url: "https://example.com/file.png", disposition: "inline" }],
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
      isSticker: true,
      mentions: ["alice@example.com"],
      broadcastMention: "here",
      forumPostKind: "topic",
      forumTitle: "Private incident title",
      forumThreadTitle: "Private incident title",
    };

    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: true,
      mamResults: [tombstone],
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toMatchObject({
      id: "standalone-tombstone",
      body: "",
      isRetracted: true,
      retractionId: "retract-message",
    });
    expect(timeline[0]?.markup).toBeUndefined();
    expect(timeline[0]?.references).toBeUndefined();
    expect(timeline[0]?.sharedFiles).toBeUndefined();
    expect(timeline[0]?.extensionAnnotations).toBeUndefined();
    expect(timeline[0]?.extensionBodyFallback).toBeUndefined();
    expect(timeline[0]?.isSticker).toBeUndefined();
    expect(timeline[0]?.mentions).toBeUndefined();
    expect(timeline[0]?.broadcastMention).toBeUndefined();
    expect(timeline[0]?.forumPostKind).toBeUndefined();
    expect(timeline[0]?.forumTitle).toBeUndefined();
    expect(timeline[0]?.forumThreadTitle).toBeUndefined();
  });

  test("ignores MAM room retractions that do not target the room-assigned stanza-id", () => {
    const existing: TimelineMessage = {
      id: "client-id",
      wireIds: ["room-stanza-1"],
      replyableId: "room-stanza-1",
      author: "bob",
      authorJid: "room@muc.example.com/bob",
      authorOccupantJid: "room@muc.example.com/bob",
      body: "keep me",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      markup: [{ type: "span", start: 0, end: 4, styles: ["strong"] }],
      references: [{ type: "data", uri: "https://example.com", begin: 0, end: 4 }],
      sharedFiles: [{ url: "https://example.com/file.png", disposition: "inline" }],
      extensionAnnotations: [],
      mentions: ["alice@example.com"],
      broadcastMention: "here",
    };
    const wrongTargetRetraction: LiveRoomMessage = {
      id: "retract-wrong-target",
      roomJid: "room@muc.example.com",
      nick: "bob",
      body: "",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      retractsId: "client-id",
    };
    const validRetraction: LiveRoomMessage = {
      ...wrongTargetRetraction,
      id: "retract-valid-target",
      retractsId: "room-stanza-1",
    };

    const unchanged = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [wrongTargetRetraction],
      existing: [existing],
    });
    expect(unchanged[0]?.isRetracted).toBeUndefined();

    const retracted = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [validRetraction],
      existing: [existing],
    });
    expect(retracted[0]?.isRetracted).toBe(true);
    expect(retracted[0]?.markup).toBeUndefined();
    expect(retracted[0]?.references).toBeUndefined();
    expect(retracted[0]?.sharedFiles).toBeUndefined();
    expect(retracted[0]?.extensionAnnotations).toBeUndefined();
    expect(retracted[0]?.mentions).toBeUndefined();
    expect(retracted[0]?.broadcastMention).toBeUndefined();
  });

  test("does not let retracted forum topics seed thread titles", () => {
    const topic: TimelineMessage = {
      id: "topic-client-id",
      wireIds: ["topic-room-stanza"],
      replyableId: "topic-room-stanza",
      threadId: "topic-client-id",
      forumPostKind: "topic",
      forumTitle: "Private incident title",
      forumThreadTitle: "Private incident title",
      author: "bob",
      authorJid: "room@muc.example.com/bob",
      authorOccupantJid: "room@muc.example.com/bob",
      body: "topic body",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
    };
    const reply: TimelineMessage = {
      id: "reply-1",
      threadId: "topic-client-id",
      author: "carol",
      authorJid: "room@muc.example.com/carol",
      authorOccupantJid: "room@muc.example.com/carol",
      body: "reply body",
      createdAt: "2026-05-08T13:02:00Z",
      isSelf: false,
    };
    const retraction: LiveRoomMessage = {
      id: "retract-topic",
      roomJid: "room@muc.example.com",
      nick: "bob",
      body: "",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      retractsId: "topic-room-stanza",
    };

    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: true,
      mamResults: [retraction],
      existing: [topic, reply],
    });

    expect(timeline[0]).toMatchObject({
      id: "topic-client-id",
      body: "",
      isRetracted: true,
    });
    expect(timeline[0]?.forumPostKind).toBeUndefined();
    expect(timeline[0]?.forumTitle).toBeUndefined();
    expect(timeline[0]?.forumThreadTitle).toBeUndefined();
    expect(timeline[1]?.forumThreadTitle).toBeUndefined();
  });

  test("MAM channel corrections clear extension fallback state when the edit has no rich card", () => {
    const existing: TimelineMessage = {
      id: "original-message",
      wireIds: ["room-stanza-1"],
      replyableId: "room-stanza-1",
      author: "bob",
      authorJid: "room@muc.example.com/bob",
      authorOccupantJid: "room@muc.example.com/bob",
      body: "GitHub waddle-social/waddle: ci completed with failure",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
    };
    const correction: LiveRoomMessage = {
      id: "edit-message",
      roomJid: "room@muc.example.com",
      nick: "bob",
      body: "plain edit",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      replacesId: "room-stanza-1",
    };

    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [correction],
      existing: [existing],
    });

    expect(timeline[0]?.body).toBe("plain edit");
    expect(timeline[0]?.isEdited).toBe(true);
    expect(timeline[0]?.extensionAnnotations).toBeUndefined();
    expect(timeline[0]?.extensionBodyFallback).toBeUndefined();
  });

  test("MAM channel corrections resolve reused sender ids inside the verified occupant scope", () => {
    const existing: TimelineMessage[] = [
      {
        id: "room-stanza-alice",
        wireIds: ["shared-client-id"],
        author: "alice",
        authorJid: "room@muc.example.com/alice",
        authorOccupantJid: "room@muc.example.com/alice",
        body: "alice original",
        createdAt: "2026-05-08T13:00:00Z",
        isSelf: false,
      },
      {
        id: "room-stanza-bob",
        wireIds: ["shared-client-id"],
        author: "bob",
        authorJid: "room@muc.example.com/bob",
        authorOccupantJid: "room@muc.example.com/bob",
        body: "bob original",
        createdAt: "2026-05-08T13:00:01Z",
        isSelf: false,
      },
    ];
    const correction: LiveRoomMessage = {
      id: "edit-message",
      roomJid: "room@muc.example.com",
      nick: "alice",
      body: "alice edited",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      replacesId: "shared-client-id",
    };

    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [correction],
      existing,
    });

    expect(timeline.find((message) => message.id === "room-stanza-alice")).toMatchObject({
      body: "alice edited",
      isEdited: true,
    });
    expect(timeline.find((message) => message.id === "room-stanza-bob")).toMatchObject({
      body: "bob original",
    });
  });

  test("MAM channel corrections replace and clear link preview state", () => {
    const existing: TimelineMessage = {
      id: "original-message",
      wireIds: ["room-stanza-1"],
      author: "bob",
      authorJid: "room@muc.example.com/bob",
      body: "old https://old.example",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      linkPreviews: [{ originalUrl: "https://old.example", title: "Old" }],
    };
    const correctionWithPreview: LiveRoomMessage = {
      id: "edit-message",
      roomJid: "room@muc.example.com",
      nick: "bob",
      body: "new https://new.example",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      replacesId: "room-stanza-1",
      linkPreviews: [{ originalUrl: "https://new.example", title: "New" }],
    };
    const correctionWithoutPreview: LiveRoomMessage = {
      ...correctionWithPreview,
      id: "edit-message-2",
      body: "new without preview",
      linkPreviews: undefined,
    };

    const withPreview = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [correctionWithPreview],
      existing: [existing],
    });
    expect(withPreview[0]?.linkPreviews).toEqual([{ originalUrl: "https://new.example", title: "New" }]);

    const withoutPreview = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [correctionWithoutPreview],
      existing: [withPreview[0]!],
    });
    expect(withoutPreview[0]?.linkPreviews).toBeUndefined();
  });

  test("MAM channel corrections never repopulate an already-retracted message", () => {
    const tombstone: TimelineMessage = {
      id: "room-stanza-1",
      author: "bob",
      authorJid: "room@muc.example.com/bob",
      authorOccupantJid: "room@muc.example.com/bob",
      body: "",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      isRetracted: true,
      retractionId: "room-retract-1",
    };
    const correction: LiveRoomMessage = {
      id: "edit-message",
      roomJid: "room@muc.example.com",
      nick: "bob",
      body: "must stay deleted",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      replacesId: "room-stanza-1",
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
    };

    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [correction],
      existing: [tombstone],
    });

    expect(timeline[0]).toMatchObject({
      body: "",
      isRetracted: true,
      retractionId: "room-retract-1",
    });
    expect(timeline[0]?.isEdited).toBeUndefined();
    expect(timeline[0]?.extensionAnnotations).toBeUndefined();
  });

  test("updates an already-loaded direct message from a MAM tombstone", () => {
    const existing: TimelineMessage = {
      id: "dm-origin",
      author: "bob",
      authorJid: "bob@example.com/mobile",
      body: "remove this direct payload",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      sharedFiles: [{ url: "https://example.com/file.png", disposition: "inline" }],
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
      mentions: ["alice@example.com"],
    };
    const tombstone: LiveDmMessage = {
      id: "dm-origin",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "",
      createdAt: "2026-05-08T13:00:00Z",
      type: "message",
      isRetracted: true,
      retractionId: "dm-retract",
    };

    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [tombstone],
      existing: [existing],
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toMatchObject({
      id: "dm-origin",
      body: "",
      isRetracted: true,
      retractionId: "dm-retract",
    });
    expect(timeline[0]?.sharedFiles).toBeUndefined();
    expect(timeline[0]?.extensionAnnotations).toBeUndefined();
    expect(timeline[0]?.extensionBodyFallback).toBeUndefined();
    expect(timeline[0]?.mentions).toBeUndefined();
  });

  test("MAM direct-message corrections apply extension fallback state when the edit has a rich card", () => {
    const existing: TimelineMessage = {
      id: "dm-origin",
      author: "bob",
      authorJid: "bob@example.com/mobile",
      body: "plain text",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
    };
    const correction: LiveDmMessage = {
      id: "dm-edit",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "GitHub waddle-social/waddle: ci completed with failure",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      replacesId: "dm-origin",
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
    };

    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [correction],
      existing: [existing],
    });

    expect(timeline[0]?.body).toBe("GitHub waddle-social/waddle: ci completed with failure");
    expect(timeline[0]?.isEdited).toBe(true);
    expect(timeline[0]?.extensionAnnotations).toEqual([extensionAnnotation]);
    expect(timeline[0]?.extensionBodyFallback).toBe(true);
  });

  test("MAM direct-message corrections never repopulate an already-retracted message", () => {
    const tombstone: TimelineMessage = {
      id: "dm-origin",
      author: "bob",
      authorJid: "bob@example.com/mobile",
      body: "",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      isRetracted: true,
      retractionId: "dm-retract",
    };
    const correction: LiveDmMessage = {
      id: "dm-edit",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "must stay deleted",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      replacesId: "dm-origin",
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
    };

    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [correction],
      existing: [tombstone],
    });

    expect(timeline[0]).toMatchObject({
      body: "",
      isRetracted: true,
      retractionId: "dm-retract",
    });
    expect(timeline[0]?.isEdited).toBeUndefined();
    expect(timeline[0]?.extensionAnnotations).toBeUndefined();
  });

  test("MAM direct-message corrections and retractions resolve aliases inside sender scope", () => {
    const existing: TimelineMessage[] = [
      {
        id: "alice-canonical",
        wireIds: ["shared-id"],
        author: "alice",
        authorJid: "alice@example.com/phone",
        body: "alice message",
        createdAt: "2026-05-08T13:00:00Z",
        isSelf: true,
      },
      {
        id: "bob-canonical",
        wireIds: ["shared-id"],
        author: "bob",
        authorJid: "bob@example.com/laptop",
        body: "bob message",
        createdAt: "2026-05-08T13:00:01Z",
        isSelf: false,
      },
    ];
    const correction: LiveDmMessage = {
      id: "bob-edit",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "bob edited",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      replacesId: "shared-id",
    };
    const corrected = buildDmTimelineFromMamResults({
      session,
      mamResults: [correction],
      existing,
    });

    expect(corrected.find((message) => message.id === "alice-canonical")).toMatchObject({
      body: "alice message",
    });
    expect(corrected.find((message) => message.id === "bob-canonical")).toMatchObject({
      body: "bob edited",
      isEdited: true,
    });

    const retraction: LiveDmMessage = {
      id: "bob-retraction",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "",
      createdAt: "2026-05-08T13:02:00Z",
      type: "message",
      retractsId: "shared-id",
      retractionId: "bob-retraction",
    };
    const retracted = buildDmTimelineFromMamResults({
      session,
      mamResults: [retraction],
      existing,
    });

    expect(retracted.find((message) => message.id === "alice-canonical")).toMatchObject({
      body: "alice message",
    });
    expect(retracted.find((message) => message.id === "bob-canonical")).toMatchObject({
      body: "",
      isRetracted: true,
    });
  });

  test("MAM direct-message mutations fail closed for duplicate aliases inside one sender scope", () => {
    const bob = {
      wireIds: ["shared-id"],
      author: "bob",
      authorJid: "bob@example.com/laptop",
      isSelf: false,
    };
    const existing: TimelineMessage[] = [
      { ...bob, id: "bob-1", body: "first", createdAt: "2026-05-08T13:00:00Z" },
      { ...bob, id: "bob-2", body: "second", createdAt: "2026-05-08T13:00:01Z" },
    ];
    const correction: LiveDmMessage = {
      id: "bob-edit",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "ambiguous edit",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      replacesId: "shared-id",
    };
    const retraction: LiveDmMessage = {
      id: "bob-retraction",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "",
      createdAt: "2026-05-08T13:02:00Z",
      type: "message",
      retractsId: "shared-id",
    };

    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [correction, retraction],
      existing,
    });

    expect(timeline.map((message) => message.body)).toEqual(["first", "second"]);
    expect(timeline.every((message) => !message.isEdited && !message.isRetracted)).toBe(true);
  });

  test("MAM direct-message corrections replace and clear link preview state", () => {
    const existing: TimelineMessage = {
      id: "dm-origin",
      author: "bob",
      authorJid: "bob@example.com/mobile",
      body: "old https://old.example",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      linkPreviews: [{ originalUrl: "https://old.example", title: "Old" }],
    };
    const correctionWithPreview: LiveDmMessage = {
      id: "dm-edit",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "new https://new.example",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      replacesId: "dm-origin",
      linkPreviews: [{ originalUrl: "https://new.example", title: "New" }],
    };
    const correctionWithoutPreview: LiveDmMessage = {
      ...correctionWithPreview,
      id: "dm-edit-2",
      body: "new without preview",
      linkPreviews: undefined,
    };

    const withPreview = buildDmTimelineFromMamResults({
      session,
      mamResults: [correctionWithPreview],
      existing: [existing],
    });
    expect(withPreview[0]?.linkPreviews).toEqual([{ originalUrl: "https://new.example", title: "New" }]);

    const withoutPreview = buildDmTimelineFromMamResults({
      session,
      mamResults: [correctionWithoutPreview],
      existing: [withPreview[0]!],
    });
    expect(withoutPreview[0]?.linkPreviews).toBeUndefined();
  });

  test("scrubs a fresh direct-message tombstone before insertion", () => {
    const tombstone: LiveDmMessage = {
      id: "standalone-dm-tombstone",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "",
      createdAt: "2026-05-08T13:00:00Z",
      type: "message",
      isRetracted: true,
      retractionId: "dm-retract",
      markup: [{ type: "span", start: 0, end: 6, styles: ["strong"] }],
      references: [{ type: "data", uri: "https://example.com", begin: 7, end: 26 }],
      sharedFiles: [{ url: "https://example.com/file.png", disposition: "inline" }],
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
      mentions: ["alice@example.com"],
    };

    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [tombstone],
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toMatchObject({
      id: "standalone-dm-tombstone",
      body: "",
      isRetracted: true,
      retractionId: "dm-retract",
    });
    expect(timeline[0]?.markup).toBeUndefined();
    expect(timeline[0]?.references).toBeUndefined();
    expect(timeline[0]?.sharedFiles).toBeUndefined();
    expect(timeline[0]?.extensionAnnotations).toBeUndefined();
    expect(timeline[0]?.extensionBodyFallback).toBeUndefined();
    expect(timeline[0]?.mentions).toBeUndefined();
  });

  test("scrubs an already-loaded direct message from a MAM retraction", () => {
    const existing: TimelineMessage = {
      id: "dm-origin",
      author: "bob",
      authorJid: "bob@example.com/mobile",
      body: "remove this direct payload",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      markup: [{ type: "span", start: 0, end: 6, styles: ["strong"] }],
      references: [{ type: "data", uri: "https://example.com", begin: 7, end: 26 }],
      sharedFiles: [{ url: "https://example.com/file.png", disposition: "inline" }],
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
      mentions: ["alice@example.com"],
    };
    const retraction: LiveDmMessage = {
      id: "dm-retract",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "",
      createdAt: "2026-05-08T13:01:00Z",
      type: "message",
      retractsId: "dm-origin",
    };

    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [retraction],
      existing: [existing],
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toMatchObject({
      id: "dm-origin",
      body: "",
      isRetracted: true,
    });
    expect(timeline[0]?.markup).toBeUndefined();
    expect(timeline[0]?.references).toBeUndefined();
    expect(timeline[0]?.sharedFiles).toBeUndefined();
    expect(timeline[0]?.extensionAnnotations).toBeUndefined();
    expect(timeline[0]?.extensionBodyFallback).toBeUndefined();
    expect(timeline[0]?.mentions).toBeUndefined();
  });

  test("sorts channel MAM timeline by parsed timestamp, not lexical timestamp text", () => {
    const first: LiveRoomMessage = {
      id: "m1",
      roomJid: "room@muc.example.com",
      nick: "bob",
      body: "first",
      createdAt: "2026-05-14T10:36:55Z",
      type: "message",
    };
    const second: LiveRoomMessage = {
      id: "m2",
      roomJid: "room@muc.example.com",
      nick: "bob",
      body: "second",
      createdAt: "2026-05-14T10:36:55.100Z",
      type: "message",
    };

    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [second, first],
    });

    expect(timeline.map((message) => message.id)).toEqual(["m1", "m2"]);
  });

  test("sorts DM MAM timeline by parsed timestamp across RFC3339 variants", () => {
    const first: LiveDmMessage = {
      id: "dm-1",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "first",
      createdAt: "2026-05-14T10:36:55+00:00",
      type: "message",
    };
    const second: LiveDmMessage = {
      id: "dm-2",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
      body: "second",
      createdAt: "2026-05-14T10:36:55.100Z",
      type: "message",
    };

    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [second, first],
    });

    expect(timeline.map((message) => message.id)).toEqual(["dm-1", "dm-2"]);
  });
});
