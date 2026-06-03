import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { ChannelSummary } from "../src/lib/chat-types";
import type { InboxEntry } from "../src/lib/xmpp/inbox-types";
import { applyEntries, createInboxState } from "../src/services/inbox";
import {
  findChannelForRoomJid,
  lastFeedVisibleUnread,
  lastThreadUnread,
  selectUnreadRoomCandidates,
} from "../src/lib/unread-overview-state";

function makeMessage(
  partial: Partial<TimelineMessage> & { id: string; createdAt: string },
): TimelineMessage {
  return { author: "alice", body: "", isSelf: false, ...partial };
}

function muc(partial: Partial<InboxEntry> & { partner: string }): InboxEntry {
  return {
    kind: "muc",
    lastStanzaId: "s",
    lastUpdated: 0,
    unread: 0,
    ...partial,
  };
}

const ROOM = "general@muc.waddle.example";
const channels: ChannelSummary[] = [
  { id: "general", name: "General", jid: ROOM, spaceId: "space" },
];

describe("selectUnreadRoomCandidates", () => {
  test("includes channel-level and thread-level unread, excludes DMs and zeros", () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 3, lastUpdated: 100 }),
      muc({ partner: ROOM, thread: "t1", threadTitle: "Deploys", unread: 2, lastUpdated: 150 }),
      muc({ partner: ROOM, thread: "t2", unread: 0, lastUpdated: 90 }), // read thread, skipped
      { partner: "bob@waddle.example", kind: "direct", lastStanzaId: "s", lastUpdated: 999, unread: 5 },
      muc({ partner: "quiet@muc.waddle.example", unread: 0, lastUpdated: 10 }), // no unread, skipped
    ]);

    const candidates = selectUnreadRoomCandidates(state);
    expect(candidates).toHaveLength(1);
    const room = candidates[0]!;
    expect(room.roomJid).toBe(ROOM);
    expect(room.channelUnread).toBe(3);
    expect(room.lastUpdated).toBe(150);
    expect(room.threads).toHaveLength(1);
    expect(room.threads[0]).toMatchObject({ threadId: "t1", unread: 2, title: "Deploys" });
  });

  test("surfaces a room that has only unread threads (no channel-level unread)", () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, thread: "t1", preview: "hi there", unread: 1, lastUpdated: 50 }),
    ]);
    const candidates = selectUnreadRoomCandidates(state);
    expect(candidates).toHaveLength(1);
    expect(candidates[0]!.channelUnread).toBe(0);
    expect(candidates[0]!.threads[0]).toMatchObject({ threadId: "t1", title: "hi there" });
  });

  test("sorts rooms most-recently-active first", () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: "a@muc.x", unread: 1, lastUpdated: 10 }),
      muc({ partner: "b@muc.x", unread: 1, lastUpdated: 30 }),
      muc({ partner: "c@muc.x", unread: 1, lastUpdated: 20 }),
    ]);
    expect(selectUnreadRoomCandidates(state).map((c) => c.roomJid)).toEqual([
      "b@muc.x",
      "c@muc.x",
      "a@muc.x",
    ]);
  });
});

describe("lastFeedVisibleUnread", () => {
  const messages = [
    makeMessage({ id: "m1", createdAt: "2026-01-01T00:00:00Z" }),
    makeMessage({ id: "r1", createdAt: "2026-01-01T00:01:00Z", threadId: "m1" }), // threaded reply
    makeMessage({ id: "m2", createdAt: "2026-01-01T00:02:00Z" }),
    makeMessage({ id: "m3", createdAt: "2026-01-01T00:03:00Z" }),
  ];

  test("takes the last N feed-visible (non-threaded) messages", () => {
    expect(lastFeedVisibleUnread(messages, 2).map((m) => m.id)).toEqual(["m2", "m3"]);
  });

  test("treats a thread root (id === threadId) as feed-visible", () => {
    const withRoot = [makeMessage({ id: "m1", createdAt: "t", threadId: "m1" })];
    expect(lastFeedVisibleUnread(withRoot, 1).map((m) => m.id)).toEqual(["m1"]);
  });

  test("clamps when unread exceeds available feed-visible messages", () => {
    expect(lastFeedVisibleUnread(messages, 99).map((m) => m.id)).toEqual(["m1", "m2", "m3"]);
  });

  test("returns nothing for non-positive unread", () => {
    expect(lastFeedVisibleUnread(messages, 0)).toEqual([]);
  });
});

describe("lastThreadUnread", () => {
  const messages = [
    makeMessage({ id: "a", createdAt: "t1" }),
    makeMessage({ id: "b", createdAt: "t2" }),
    makeMessage({ id: "c", createdAt: "t3" }),
  ];

  test("takes the last N messages and clamps", () => {
    expect(lastThreadUnread(messages, 2).map((m) => m.id)).toEqual(["b", "c"]);
    expect(lastThreadUnread(messages, 99).map((m) => m.id)).toEqual(["a", "b", "c"]);
    expect(lastThreadUnread(messages, 0)).toEqual([]);
  });
});

describe("findChannelForRoomJid", () => {
  test("matches on the channel JID", () => {
    expect(findChannelForRoomJid(ROOM, channels)?.id).toBe("general");
  });

  test("falls back to the JID local-part slug for managed rooms", () => {
    const slugOnly: ChannelSummary[] = [{ id: "general", name: "General" }];
    expect(findChannelForRoomJid(ROOM, slugOnly)?.id).toBe("general");
  });

  test("returns null when the room is not in the topology", () => {
    expect(findChannelForRoomJid("missing@muc.waddle.example", channels)).toBeNull();
  });
});
