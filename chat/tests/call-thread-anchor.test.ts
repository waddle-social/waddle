import { afterEach, describe, expect, test } from "bun:test";
import { isFeedTimelineMessage, mapLiveRoomMessageToTimeline } from "../src/channels/timeline";
import {
  callThreadAnchorLabel,
  callThreadAnchorThreadId,
  readCallAnchorCardState,
  wasmThreadEntryToAnchorMessage,
} from "../src/lib/call-thread-anchor";
import type { WasmThreadEntry } from "../src/lib/xmpp/wasm-types";
import { $callState } from "../src/lib/calls/call-store";
import { $mucCallMedia, $mucCallParticipants, clearMucCallParticipants } from "../src/lib/calls/muc-call-presence";
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
  afterEach(() => {
    $callState.set({ phase: "idle" });
    clearMucCallParticipants();
    $mucCallMedia.set({});
  });

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

  test("derives rich anchor card state from room live detectors with stale-live override", () => {
    const timelineMessage = mapLiveRoomMessageToTimeline(session, callAnchor({
      roomJid: "general@conference.example.com",
    }));
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob"],
    });
    $mucCallMedia.setKey("general@conference.example.com", { audio: true, video: true });

    expect(readCallAnchorCardState(timelineMessage, "general@conference.example.com")).toEqual({
      status: "live",
      media: { audio: true, video: true },
      participantCount: 2,
      participantLabels: ["alice", "bob"],
      messageCount: 0,
      threadId: "call-thread-uuid",
      title: "Live video call",
      actionLabel: "Join",
      actionDisabled: false,
      ariaLabel: "Join live video call, 2 people: alice, bob",
    });

    clearMucCallParticipants();

    expect(readCallAnchorCardState(timelineMessage, "general@conference.example.com")).toMatchObject({
      status: "ended",
      participantCount: 0,
      participantLabels: [],
      title: "Call ended",
      actionLabel: null,
      actionDisabled: false,
    });
  });

  test("uses the banner's busy semantics for live anchor join actions", () => {
    const timelineMessage = mapLiveRoomMessageToTimeline(session, callAnchor({
      roomJid: "general@conference.example.com",
    }));
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob"],
    });
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: "other@conference.example.com",
      sid: "other-call",
      media: { audio: true, video: false },
      join: {
        url: "wss://livekit.test",
        room: "other",
        identity: "alice@example.com/web",
        token: "tok",
      },
      selfNick: "alice",
    });

    expect(readCallAnchorCardState(timelineMessage, "general@conference.example.com")).toMatchObject({
      status: "live",
      actionLabel: "In another call",
      actionDisabled: true,
      ariaLabel: "Live call, 2 people: alice, bob; already in another call",
    });
  });

  test("renders ended call-thread anchors as muted duration summaries", () => {
    const timelineMessage = mapLiveRoomMessageToTimeline(session, callAnchor({
      callThread: {
        kind: "muc",
        sid: "session-uuid",
        media: ["audio"],
        initiator: "alice@example",
        started: "2026-06-07T14:30:00Z",
        ended: "2026-06-07T14:35:00Z",
        duration: "PT5M",
      },
    }));

    expect(callThreadAnchorLabel(timelineMessage)).toBe("Call ended · 5m");
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

  test("room archive codec maps call-thread-ended fastening onto a control event", () => {
    const live = roomMessageFromArchived({
      mam_id: "mam-ended-1",
      id: "ended-1",
      from: "general@muc.example.com",
      to: "alice@example.com/web",
      message_type: "groupchat",
      timestamp: "2026-06-07T14:35:00Z",
      reaction_emojis: [],
      is_muc: true,
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      link_previews: [],
      call_thread_ended: {
        anchor_id: "anchor-stanza-id",
        ended: "2026-06-07T14:35:00Z",
        duration: "PT5M",
      },
    });

    expect(live?.body).toBe("");
    expect(live?.callThreadEnded).toEqual({
      anchorId: "anchor-stanza-id",
      ended: "2026-06-07T14:35:00Z",
      duration: "PT5M",
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

const baseEntry: WasmThreadEntry = {
  channel: "general@conference.example.com",
  thread_id: "call-thread-uuid",
  last_stanza_id: "s1",
  last_activity: "2026-06-07T14:30:00Z",
  unread: 0,
  reply_count: 7,
  has_unread: false,
};

describe("wasmThreadEntryToAnchorMessage", () => {
  afterEach(() => {
    $callState.set({ phase: "idle" });
    clearMucCallParticipants();
    $mucCallMedia.set({});
  });

  test("adapts a live MUC call-thread entry into an anchor-shaped message", () => {
    const msg = wasmThreadEntryToAnchorMessage({
      ...baseEntry,
      callThread: { kind: "muc", media: ["audio", "video"] },
    });
    expect(msg).not.toBeNull();
    expect(msg?.threadId).toBe("call-thread-uuid");
    expect(msg?.callThread?.media).toEqual(["audio", "video"]);
    expect(msg?.callThread?.ended).toBeUndefined();
  });

  test("adapts an ended MUC call-thread entry with duration", () => {
    const msg = wasmThreadEntryToAnchorMessage({
      ...baseEntry,
      callThread: { kind: "muc", media: ["audio"] },
      callThreadEnded: { ended: "2026-06-07T14:35:00Z", duration: "PT5M" },
    });
    expect(msg?.callThread).toMatchObject({
      kind: "muc",
      media: ["audio"],
      ended: "2026-06-07T14:35:00Z",
      duration: "PT5M",
    });
  });

  test("returns null for non-call and non-muc entries", () => {
    expect(wasmThreadEntryToAnchorMessage(baseEntry)).toBeNull();
    expect(
      wasmThreadEntryToAnchorMessage({ ...baseEntry, callThread: { kind: "dm", media: ["audio"] } }),
    ).toBeNull();
  });

  test("drives the shared composable to a live card when the room call is active", () => {
    const msg = wasmThreadEntryToAnchorMessage({
      ...baseEntry,
      callThread: { kind: "muc", media: ["audio", "video"] },
    });
    expect(msg).not.toBeNull();
    $mucCallParticipants.set({ "general@conference.example.com": ["alice", "bob"] });
    $mucCallMedia.setKey("general@conference.example.com", { audio: true, video: true });

    expect(readCallAnchorCardState(msg!, "general@conference.example.com")).toMatchObject({
      status: "live",
      media: { audio: true, video: true },
      participantCount: 2,
      participantLabels: ["alice", "bob"],
      threadId: "call-thread-uuid",
      title: "Live video call",
      actionLabel: "Join",
    });
  });

  test("drives the shared composable to an ended card for an ended entry", () => {
    const msg = wasmThreadEntryToAnchorMessage({
      ...baseEntry,
      callThread: { kind: "muc", media: ["audio"] },
      callThreadEnded: { ended: "2026-06-07T14:35:00Z", duration: "PT5M" },
    });
    expect(msg).not.toBeNull();

    expect(readCallAnchorCardState(msg!, "general@conference.example.com")).toMatchObject({
      status: "ended",
      title: "Call ended",
      actionLabel: null,
      ariaLabel: "Call ended · 5m",
    });
  });
});
