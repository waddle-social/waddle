import { describe, expect, test } from "bun:test";
import { roomMessageFromArchived, dmMessageFromArchived } from "../src/lib/xmpp/wasm-message-codecs";
import { buildChannelTimelineFromMamResults } from "../src/channels/message-timeline-state";
import type { WasmArchivedMessage } from "../src/lib/xmpp/wasm-types";
import type { WaddleSession } from "../src/lib/server-auth";
import type { LiveRoomMessage } from "../src/lib/xmpp-client";

// Regression coverage for the "threads now show empty messages" bug.
//
// PR #724 (XEP-0201 §3 conformance) made chat-states / displayed markers /
// reactions / retractions / corrections targeting a threaded message echo
// the target's `<thread/>`. Combined with three layers of code that
// treated `<thread/>` as "real content" (server `is_archivable`, client
// codec null-check, client MAM merge gate), bodyless metadata stanzas
// started surfacing as empty rows inside the thread panel.
//
// The fix recognises that `<thread/>` is scope metadata, not content. A
// stanza needs an actual body / subject / file / reaction / retraction /
// moderation / forum-action / sticker payload to materialise as a
// timeline row. These tests lock that semantic at every layer the bug
// touched.

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

const ROOM_JID = "topic@conf.example.com";
const SENDER_FULL = `${ROOM_JID}/bob`;
const PEER_BARE = "bob@example.com";

function baseArchivedRoom(overrides: Partial<WasmArchivedMessage> = {}): WasmArchivedMessage {
  return {
    mam_id: "mam-1",
    message_type: "groupchat",
    from: SENDER_FULL,
    to: "alice@example.com",
    reaction_emojis: [],
    is_muc: true,
    markup_spans: [],
    mention_uris: [],
    references: [],
    is_sticker: false,
    shared_files: [],
    ...overrides,
  };
}

function baseArchivedDm(overrides: Partial<WasmArchivedMessage> = {}): WasmArchivedMessage {
  return {
    mam_id: "mam-dm-1",
    message_type: "chat",
    from: `${PEER_BARE}/desktop`,
    to: "alice@example.com",
    reaction_emojis: [],
    is_muc: false,
    markup_spans: [],
    mention_uris: [],
    references: [],
    is_sticker: false,
    shared_files: [],
    ...overrides,
  };
}

describe("XEP-0201 `<thread/>` is scope metadata, not content", () => {
  test("room codec drops a stanza whose only content is a thread reference", () => {
    // An XEP-0085 `composing` chat-state echoed inside a thread per
    // XEP-0201 §3: the wire stanza has no body / no subject / no
    // attachments / no extension annotations, just `<thread/>` plus
    // (in practice) a `<composing/>` element the Rust parser surfaces
    // through `chat_state`. Before the fix, the codec's null-check
    // included `!message.thread`, so this stanza built a TimelineMessage
    // with `body: ""` and `threadId: <id>` — which then rendered as an
    // empty row in the thread panel.
    const stanza = baseArchivedRoom({ thread: "topic-root", chat_state: "composing" });

    expect(roomMessageFromArchived(stanza)).toBeNull();
  });

  test("room codec drops a thread-only stanza with no chat-state either", () => {
    // Pure paranoia: a stanza that carries ONLY a thread reference and
    // nothing else (no body, no chat-state, no marker) is also rejected.
    // Such a stanza has no legitimate use in the protocol — there is
    // nothing to render and nothing to act on — but a misbehaving peer
    // or stale archive row could produce one.
    const stanza = baseArchivedRoom({ thread: "topic-root" });

    expect(roomMessageFromArchived(stanza)).toBeNull();
  });

  test("room codec still keeps a threaded message that carries a body", () => {
    // Sanity check that the fix doesn't over-correct: a real reply with
    // body content is still surfaced as a timeline message.
    const stanza = baseArchivedRoom({ thread: "topic-root", body: "agreed", id: "reply-1" });

    const result = roomMessageFromArchived(stanza);
    expect(result).not.toBeNull();
    expect(result?.body).toBe("agreed");
    expect(result?.threadId).toBe("topic-root");
  });

  test("dm codec drops a stanza whose only content is a thread reference", () => {
    // DMs don't surface threads in the Waddle UI, but the codec stays
    // symmetric with the room path so a stray foreign-server archive
    // row can't manifest a bodyless DM ghost either.
    const stanza = baseArchivedDm({ thread: "any-thread", chat_state: "composing" });

    expect(dmMessageFromArchived(stanza, "alice@example.com")).toBeNull();
  });

  test("channel MAM merge does not surface a thread-only row even if the codec slips", () => {
    // Defence-in-depth: if a future codec change (or a foreign server's
    // own decoder) ever hands the merge layer a LiveRoomMessage with an
    // empty body and a threadId, the merge gate must still refuse to
    // promote it to a timeline row. The pre-fix gate accepted any
    // `msg.threadId` as enough; this asserts the corrected semantic.
    const phantom: LiveRoomMessage = {
      id: "phantom-1",
      roomJid: ROOM_JID,
      nick: "bob",
      body: "",
      createdAt: "2026-05-20T09:00:00Z",
      createdAtSource: "archive",
      type: "message",
      threadId: "topic-root",
    };
    const real: LiveRoomMessage = {
      id: "reply-1",
      roomJid: ROOM_JID,
      nick: "bob",
      body: "agreed",
      createdAt: "2026-05-20T09:01:00Z",
      createdAtSource: "archive",
      type: "message",
      threadId: "topic-root",
    };

    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [phantom, real],
    });

    expect(timeline.map((m) => m.id)).toEqual(["reply-1"]);
  });
});
