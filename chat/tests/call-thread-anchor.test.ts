import { describe, expect, test } from "bun:test";
import { isFeedTimelineMessage, mapLiveRoomMessageToTimeline } from "../src/channels/timeline";
import { callThreadAnchorLabel, callThreadAnchorThreadId } from "../src/lib/call-thread-anchor";
import type { WaddleSession } from "../src/lib/server-auth";
import { roomMessageFromArchived } from "../src/lib/xmpp/wasm-message-codecs";
import type { LiveRoomMessage } from "../src/lib/xmpp-client";

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

function callAnchor(overrides: Partial<LiveRoomMessage> = {}): LiveRoomMessage {
  return {
    id: "anchor-1",
    roomJid: "general@muc.example.com",
    nick: "Alice",
    body: "Alice started a call",
    createdAt: "2026-06-07T14:30:00Z",
    createdAtSource: "archive",
    type: "message",
    threadId: "call-thread-uuid",
    callThread: {
      kind: "muc",
      sid: "session-uuid",
      media: ["audio", "video"],
      initiator: "alice@example",
      started: "2026-06-07T14:30:00Z",
    },
    ...overrides,
  };
}

describe("call-thread anchor timeline mapping", () => {
  test("maps inbound channel call-thread marker onto the unified timeline message", () => {
    const timelineMessage = mapLiveRoomMessageToTimeline(session, callAnchor());

    expect(timelineMessage.body).toBe("Alice started a call");
    expect(timelineMessage.threadId).toBe("call-thread-uuid");
    expect(timelineMessage.callThread).toEqual({
      kind: "muc",
      sid: "session-uuid",
      media: ["audio", "video"],
      initiator: "alice@example",
      started: "2026-06-07T14:30:00Z",
    });
  });

  test("keeps the call-thread anchor visible in the channel feed", () => {
    const timelineMessage = mapLiveRoomMessageToTimeline(session, callAnchor());

    expect(timelineMessage.id).toBe("anchor-1");
    expect(timelineMessage.threadId).toBe("call-thread-uuid");
    expect(isFeedTimelineMessage(timelineMessage)).toBe(true);
  });

  test("uses the inbound anchor body as the inline label and opens the call thread", () => {
    const timelineMessage = mapLiveRoomMessageToTimeline(session, callAnchor());

    expect(callThreadAnchorLabel(timelineMessage)).toBe("Alice started a call");
    expect(callThreadAnchorThreadId(timelineMessage)).toBe("call-thread-uuid");
  });

  test("room archive codec maps the WASM call-thread marker onto LiveRoomMessage", () => {
    const live = roomMessageFromArchived({
      mam_id: "mam-anchor-1",
      id: "anchor-1",
      from: "general@muc.example.com",
      to: "alice@example.com/web",
      message_type: "groupchat",
      body: "Alice started a call",
      timestamp: "2026-06-07T14:30:00Z",
      reaction_emojis: [],
      is_muc: true,
      thread: "call-thread-uuid",
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      link_previews: [],
      call_thread: {
        kind: "muc",
        sid: "session-uuid",
        media: ["audio", "video"],
        initiator: "alice@example",
        started: "2026-06-07T14:30:00Z",
      },
    });

    expect(live?.threadId).toBe("call-thread-uuid");
    expect(live?.callThread).toEqual({
      kind: "muc",
      sid: "session-uuid",
      media: ["audio", "video"],
      initiator: "alice@example",
      started: "2026-06-07T14:30:00Z",
    });
  });

  test("room archive codec still drops bodyless unsupported call-thread markers", () => {
    const live = roomMessageFromArchived({
      mam_id: "mam-anchor-2",
      id: "anchor-2",
      from: "general@muc.example.com",
      to: "alice@example.com/web",
      message_type: "groupchat",
      reaction_emojis: [],
      is_muc: true,
      thread: "call-thread-uuid",
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      link_previews: [],
      call_thread: {
        kind: "dm",
        sid: "session-uuid",
        media: ["audio"],
        initiator: "alice@example",
        started: "2026-06-07T14:30:00Z",
      },
    });

    expect(live).toBeNull();
  });

  test("room archive codec drops bodyless call-thread anchors", () => {
    const live = roomMessageFromArchived({
      mam_id: "mam-anchor-3",
      id: "anchor-3",
      from: "general@muc.example.com",
      to: "alice@example.com/web",
      message_type: "groupchat",
      reaction_emojis: [],
      is_muc: true,
      thread: "call-thread-uuid",
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      link_previews: [],
      call_thread: {
        kind: "muc",
        sid: "session-uuid",
        media: ["audio"],
        initiator: "alice@example",
        started: "2026-06-07T14:30:00Z",
      },
    });

    expect(live).toBeNull();
  });
});
