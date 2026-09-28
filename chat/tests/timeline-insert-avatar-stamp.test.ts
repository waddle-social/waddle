import { describe, expect, test } from "bun:test";
import { insertLiveMessage } from "../src/lib/messaging/timeline-insert";
import { buildChannelTimelineFromMamResults } from "../src/channels/message-timeline-state";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { LiveRoomMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";

const ROOM = "room@muc.example.com";

function row(overrides: Partial<TimelineMessage>): TimelineMessage {
  return {
    id: "origin-x",
    author: "sam",
    authorJid: `${ROOM}/sam`,
    authorOccupantJid: `${ROOM}/sam`,
    body: "hi",
    createdAt: "2026-09-27T10:00:00Z",
    createdAtSource: "fallback",
    isSelf: false,
    ...overrides,
  };
}

describe("live merge never lends author identity across an unvouched twin", () => {
  test("the first attribution wins over a redelivered copy stamped with a later nick holder", () => {
    const first = insertLiveMessage([row({ authorAvatarJid: "sam@example.com" })], row({ authorAvatarJid: "mallory@example.com" }), new Set());
    expect(first.messages).toHaveLength(1);
    expect(first.messages[0]?.authorAvatarJid).toBe("sam@example.com");
  });

  test("Mallory's live message reusing Sam's origin-id cannot stamp Sam's unstamped row", () => {
    // Sam's row arrived delayed and could not be attributed (no mapping then).
    const sams = row({ createdAtSource: "delay" });
    const mallorys = row({ createdAt: "2026-09-27T11:00:00Z", authorAvatarJid: "mallory@example.com" });
    const merged = insertLiveMessage([sams], mallorys, new Set());
    // The copies match on the sender-chosen id and collapse into Sam's row;
    // that row must not become Mallory's.
    expect(merged.messages).toHaveLength(1);
    expect(merged.messages[0]?.authorAvatarJid).toBeUndefined();
  });

  test("an archive copy matched only by origin-id cannot lend its stamp or real JID", () => {
    const sams = row({ createdAtSource: "delay" });
    const archiveCopy = row({
      createdAtSource: "archive",
      authorAvatarJid: "mallory@example.com",
      authorRealJid: "mallory@example.com",
      stanzaId: "room-assigned-2",
      stanzaIdBy: ROOM,
    });
    const merged = insertLiveMessage([sams], archiveCopy, new Set());
    expect(merged.messages).toHaveLength(1);
    expect(merged.messages[0]?.authorAvatarJid).toBeUndefined();
    expect(merged.messages[0]?.authorRealJid).toBeUndefined();
  });

  test("the room's archive copy with the same stanza-id fills a missing stamp", () => {
    const live = row({ createdAtSource: "delay", stanzaId: "room-assigned-1", stanzaIdBy: ROOM });
    const archiveCopy = row({
      createdAtSource: "archive",
      stanzaId: "room-assigned-1",
      stanzaIdBy: ROOM,
      authorAvatarJid: "sam@example.com",
      authorRealJid: "sam@example.com",
    });
    const merged = insertLiveMessage([live], archiveCopy, new Set());
    expect(merged.messages).toHaveLength(1);
    expect(merged.messages[0]?.authorAvatarJid).toBe("sam@example.com");
    expect(merged.messages[0]?.authorRealJid).toBe("sam@example.com");
  });

  test("a live twin never fills a missing stamp, even with the same stanza-id", () => {
    const live = row({ createdAtSource: "delay", stanzaId: "room-assigned-1", stanzaIdBy: ROOM });
    const liveTwin = row({ stanzaId: "room-assigned-1", stanzaIdBy: ROOM, authorAvatarJid: "mallory@example.com" });
    const merged = insertLiveMessage([live], liveTwin, new Set());
    expect(merged.messages[0]?.authorAvatarJid).toBeUndefined();
  });
});

describe("MAM page merge never lends the archive real JID across an origin-id-only match", () => {
  const session = { username: "viewer", jid: "viewer@example.com/web", session_id: "s", xmpp_websocket_url: "wss://x" } as WaddleSession;

  function archived(overrides: Partial<LiveRoomMessage>): LiveRoomMessage {
    return {
      id: "origin-x",
      roomJid: ROOM,
      nick: "sam",
      body: "hi",
      createdAt: "2026-09-27T11:00:00Z",
      createdAtSource: "archive",
      ...overrides,
    } as LiveRoomMessage;
  }

  function build(existing: TimelineMessage[], mamResults: LiveRoomMessage[]): TimelineMessage[] {
    return buildChannelTimelineFromMamResults({ session, channelIsForum: false, existing, mamResults });
  }

  test("Mallory's archived message reusing Sam's origin-id does not give Sam's row Mallory's JID", () => {
    const sams = row({ createdAtSource: "delay" });
    const messages = build([sams], [archived({ authorRealJid: "mallory@example.com", stanzaId: "room-assigned-2", stanzaIdBy: ROOM })]);
    // The copies match on the sender-chosen id and collapse into one row,
    // which must not take Mallory's real JID.
    expect(messages).toHaveLength(1);
    expect(messages[0]?.authorRealJid).toBeUndefined();
  });

  test("the archive copy with the same room stanza-id still supplies the real JID", () => {
    const live = row({ createdAtSource: "fallback", stanzaId: "room-assigned-1", stanzaIdBy: ROOM });
    const messages = build([live], [archived({ createdAt: live.createdAt, authorRealJid: "sam@example.com", stanzaId: "room-assigned-1", stanzaIdBy: ROOM })]);
    expect(messages).toHaveLength(1);
    expect(messages[0]?.authorRealJid).toBe("sam@example.com");
  });
});
