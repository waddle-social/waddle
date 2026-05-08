import { describe, expect, test } from "bun:test";
import { buildChannelTimelineFromMamResults } from "../src/channels/message-timeline-state";
import { buildDmTimelineFromMamResults } from "../src/dms/message-timeline-state";
import type { TimelineMessage } from "../src/lib/chat-ui";
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

describe("XEP-0424 tombstone replay", () => {
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
      extensionAnnotations: [],
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
      extensionAnnotations: [],
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

  test("updates an already-loaded direct message from a MAM tombstone", () => {
    const existing: TimelineMessage = {
      id: "dm-origin",
      author: "bob",
      authorJid: "bob@example.com/mobile",
      body: "remove this direct payload",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      sharedFiles: [{ url: "https://example.com/file.png", disposition: "inline" }],
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
    expect(timeline[0]?.mentions).toBeUndefined();
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
      extensionAnnotations: [],
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
      extensionAnnotations: [],
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
    expect(timeline[0]?.mentions).toBeUndefined();
  });
});
