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
import { $dmCallActivities, clearDmCallActivities } from "../src/lib/calls/dm-call-activity";
import { dmCallAnchorId } from "../src/lib/calls/dm-call-anchor";
import { $mucCallMedia, $mucCallParticipants, clearMucCallParticipants } from "../src/lib/calls/muc-call-presence";
import type { WaddleSession } from "../src/lib/server-auth";
import { dmMessageFromArchived, roomMessageFromArchived } from "../src/lib/xmpp/wasm-message-codecs";
import type { LiveRoomMessage } from "../src/lib/xmpp-client";
import { buildDmTimelineFromMamResults, fromLiveDmMessage } from "../src/dms/message-timeline-state";

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
    clearDmCallActivities();
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

  test("derives ended card media from the anchor, not the cleared live detector", () => {
    const timelineMessage = mapLiveRoomMessageToTimeline(session, callAnchor({
      roomJid: "general@conference.example.com",
      callThread: {
        kind: "muc",
        sid: "session-uuid",
        media: ["audio", "video"],
        initiator: "alice@example",
        started: "2026-06-07T14:30:00Z",
        ended: "2026-06-07T14:35:00Z",
        duration: "PT5M",
      },
    }));

    // No active call seeded: the live detector falls back to audio-only default.
    expect(readCallAnchorCardState(timelineMessage, "general@conference.example.com")).toMatchObject({
      status: "ended",
      media: { audio: true, video: true },
      title: "Call ended · 5m",
    });
  });

  test("shows the formatted duration in the ended anchor card title", () => {
    const timelineMessage = mapLiveRoomMessageToTimeline(session, callAnchor({
      roomJid: "general@conference.example.com",
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

    expect(readCallAnchorCardState(timelineMessage, "general@conference.example.com")).toMatchObject({
      status: "ended",
      title: "Call ended · 5m",
    });
  });

  test("derives live DM anchor card state from peer and sid activity", () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-live",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "outgoing",
        updatedAt: "2026-06-07T14:31:00Z",
      },
    });
    const message = {
      body: "",
      author: "Bob",
      threadId: "dm-call-thread",
      callThread: {
        kind: "dm" as const,
        sid: "dm-live",
        media: ["audio"],
        initiator: "alice@example.com",
        started: "2026-06-07T14:30:00Z",
      },
    };

    expect(readCallAnchorCardState(message, "bob@example.com")).toMatchObject({
      status: "live",
      media: { audio: true, video: true },
      participantCount: 2,
      actionLabel: "Join",
    });
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

  test("DM archive codec maps the WASM call-thread marker onto TimelineMessage", () => {
    const live = dmMessageFromArchived({
      mam_id: "mam-dm-anchor-1",
      id: "dm-anchor-1",
      from: "bob@example.com/phone",
      to: "alice@example.com/web",
      message_type: "chat",
      timestamp: "2026-06-07T14:30:00Z",
      reaction_emojis: [],
      is_muc: false,
      thread: "dm-call-thread-uuid",
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      link_previews: [],
      call_thread: {
        kind: "dm",
        sid: "dm-session-uuid",
        media: ["audio", "video"],
        initiator: "alice@example.com",
        started: "2026-06-07T14:30:00Z",
      },
    }, "alice@example.com");

    expect(live?.threadId).toBe("dm-call-thread-uuid");
    expect(live?.callThread).toEqual({
      kind: "dm",
      sid: "dm-session-uuid",
      media: ["audio", "video"],
      initiator: "alice@example.com",
      started: "2026-06-07T14:30:00Z",
    });

    const timeline = fromLiveDmMessage(session, live!);
    expect(timeline.callThread).toEqual(live?.callThread);
  });

  test("DM archive codec aliases the call-thread anchor by sid for live dedup", () => {
    const live = dmMessageFromArchived({
      mam_id: "mam-dm-anchor-1",
      id: "dm-anchor-1",
      from: "bob@example.com/phone",
      to: "alice@example.com/web",
      message_type: "chat",
      timestamp: "2026-06-07T14:30:00Z",
      reaction_emojis: [],
      is_muc: false,
      thread: "dm-session-uuid",
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      link_previews: [],
      call_thread: {
        kind: "dm",
        sid: "dm-session-uuid",
        media: ["audio"],
        initiator: "bob@example.com",
        started: "2026-06-07T14:30:00Z",
      },
    }, "alice@example.com");

    // The deterministic alias lets a same-session MAM backfill collapse onto
    // the synthesized live anchor (same call sid) instead of duplicating it.
    expect(live?.wireIds).toContain(dmCallAnchorId("dm-session-uuid"));
  });

  test("DM archive codec maps ended metadata and suppresses stale live activity", () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-session-uuid",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-06-07T14:31:00Z",
      },
    });
    const live = dmMessageFromArchived({
      mam_id: "mam-dm-anchor-ended",
      id: "dm-anchor-ended",
      from: "bob@example.com/phone",
      to: "alice@example.com/web",
      message_type: "chat",
      timestamp: "2026-06-07T14:35:00Z",
      reaction_emojis: [],
      is_muc: false,
      thread: "dm-call-thread-uuid",
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      link_previews: [],
      call_thread: {
        kind: "dm",
        sid: "dm-session-uuid",
        media: ["audio"],
        initiator: "alice@example.com",
        started: "2026-06-07T14:30:00Z",
      },
      call_thread_ended: {
        anchor_id: "dm-anchor-ended",
        ended: "2026-06-07T14:35:00Z",
        duration: "PT5M",
      },
    }, "alice@example.com");

    const timeline = fromLiveDmMessage(session, live!);
    expect(timeline.callThread).toMatchObject({
      kind: "dm",
      ended: "2026-06-07T14:35:00Z",
      duration: "PT5M",
    });
    expect(readCallAnchorCardState(timeline, "bob@example.com")).toMatchObject({
      status: "ended",
      title: "Call ended · 5m",
      actionLabel: null,
    });
  });

  test("DM MAM merge updates an existing call anchor with ended metadata", () => {
    const existing = fromLiveDmMessage(session, {
      id: "dm-anchor-ended",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/phone",
      nick: "Bob",
      body: "",
      createdAt: "2026-06-07T14:30:00Z",
      createdAtSource: "live",
      type: "message",
      threadId: "dm-call-thread-uuid",
      callThread: {
        kind: "dm",
        sid: "dm-session-uuid",
        media: ["audio"],
        initiator: "alice@example.com",
        started: "2026-06-07T14:30:00Z",
      },
    });
    const archived = dmMessageFromArchived({
      mam_id: "mam-dm-anchor-ended",
      id: "dm-anchor-ended",
      from: "bob@example.com/phone",
      to: "alice@example.com/web",
      message_type: "chat",
      timestamp: "2026-06-07T14:35:00Z",
      reaction_emojis: [],
      is_muc: false,
      thread: "dm-call-thread-uuid",
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
      link_previews: [],
      call_thread: {
        kind: "dm",
        sid: "dm-session-uuid",
        media: ["audio"],
        initiator: "alice@example.com",
        started: "2026-06-07T14:30:00Z",
      },
      call_thread_ended: {
        anchor_id: "dm-anchor-ended",
        ended: "2026-06-07T14:35:00Z",
        duration: "PT5M",
      },
    }, "alice@example.com");

    const timeline = buildDmTimelineFromMamResults({
      session,
      existing: [existing],
      mamResults: [archived!],
    });
    expect(timeline).toHaveLength(1);
    expect(timeline[0]?.callThread).toMatchObject({
      kind: "dm",
      ended: "2026-06-07T14:35:00Z",
      duration: "PT5M",
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
      title: "Call ended · 5m",
      actionLabel: null,
      ariaLabel: "Call ended · 5m",
    });
  });
});
