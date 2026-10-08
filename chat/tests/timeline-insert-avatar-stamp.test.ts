import { describe, expect, test } from "bun:test";
import { timelineRowKey } from "../src/lib/timeline-row-key";
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
  test("retained room twins have stable independent presentation keys", () => {
    const first = insertLiveMessage([], row({}), new Set()).messages;
    const second = insertLiveMessage(first, row({ body: "new holder" }), new Set()).messages;
    expect(second).toHaveLength(2);
    expect(second[0]?.rowKey).toBeDefined();
    expect(second[1]?.rowKey).toBeDefined();
    expect(second.map(timelineRowKey)).toHaveLength(new Set(second.map(timelineRowKey)).size);
    expect(second[0]?.rowKey).not.toBe(second[1]?.rowKey);
    expect(second[0]?.rowKey).toBe(first[0]?.rowKey);
    expect(second.map((message) => message.id)).toEqual(["origin-x", "origin-x"]);
  });

  test("the first attribution wins over a redelivered copy stamped with a later nick holder", () => {
    const first = insertLiveMessage([row({ authorAvatarJid: "sam@example.com" })], row({ authorAvatarJid: "mallory@example.com" }), new Set());
    expect(first.messages).toHaveLength(2);
    expect(first.messages[0]?.authorAvatarJid).toBe("sam@example.com");
  });

  test("Mallory's live message reusing Sam's origin-id cannot stamp Sam's unstamped row", () => {
    // Sam's row arrived delayed and could not be attributed (no mapping then).
    const sams = row({ createdAtSource: "delay" });
    const mallorys = row({ createdAt: "2026-09-27T11:00:00Z", authorAvatarJid: "mallory@example.com" });
    const merged = insertLiveMessage([sams], mallorys, new Set());
    // A reused sender ID must keep the older row separate.
    expect(merged.messages).toHaveLength(2);
    expect(merged.messages[0]?.body).toBe(sams.body);
    expect(merged.messages[0]?.createdAt).toBe(sams.createdAt);
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
    expect(merged.messages).toHaveLength(2);
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

  test("repeated anonymous MAM results reconcile by room-scoped archive UID", () => {
    const result = archived({ id: "archive-1", archiveId: "archive-1", senderChosenIds: [] });
    const first = build([], [result]);
    for (const seedExistingOnly of [false, true]) {
      const repeated = buildChannelTimelineFromMamResults({
        session, channelIsForum: false, existing: first,
        mamResults: [{ ...result }], options: { seedExistingOnly },
      });
      expect(repeated).toHaveLength(1);
      expect(repeated[0]?.body).toBe("hi");
      expect(repeated[0]?.authorRealJid).toBeUndefined();
      expect(repeated[0]?.rowKey).toBe(first[0]?.rowKey);
    }
    expect(build([], [result, { ...result }])).toHaveLength(1);
  });

  test("a verified MAM twin retains its archive UID for later anonymous replay", () => {
    const result = archived({
      id: "archive-1", archiveId: "archive-1", originId: "origin-x",
      senderChosenIds: ["origin-x"], authorRealJid: "sam@example.com",
    });
    for (const archiveId of [undefined, ""]) {
      const live = row({ archiveId, rowKey: "live-row", authorRealJid: "sam@example.com", senderChosenIds: ["origin-x"] });
      const first = build([live], [result]);
      expect(first).toHaveLength(1);
      expect(first[0]?.archiveId).toBe("archive-1");
      const repeated = build(first, [{ ...result, authorRealJid: undefined }]);
      expect(repeated).toHaveLength(1);
      expect(repeated[0]?.rowKey).toBe("live-row");
      expect(repeated[0]?.authorRealJid).toBe("sam@example.com");
    }
  });

  test("Mallory's archived message reusing Sam's origin-id does not give Sam's row Mallory's JID", () => {
    const sams = row({ createdAtSource: "delay" });
    const messages = build([sams], [archived({ authorRealJid: "mallory@example.com", stanzaId: "room-assigned-2", stanzaIdBy: ROOM })]);
    // The earlier row retains its content, timestamp, and attribution.
    expect(messages).toHaveLength(2);
    expect(messages[0]?.body).toBe(sams.body);
    expect(messages[0]?.createdAt).toBe(sams.createdAt);
    expect(messages[0]?.authorRealJid).toBeUndefined();
  });

  test("live and MAM aliases preserve both copies when either sender is unknown", () => {
    for (const [firstReal, secondReal] of [[undefined, undefined], ["sam@example.com", undefined], [undefined, "sam@example.com"], ["sam@example.com", "mallory@example.com"]]) {
      const first = row({ authorRealJid: firstReal, body: "first", createdAtSource: "delay" });
      const second = row({ id: "new-envelope", wireIds: ["origin-x"], authorRealJid: secondReal, body: "second", createdAt: "2026-09-27T11:00:00Z" });
      expect(insertLiveMessage([first], second, new Set()).messages.map((message) => message.body)).toEqual(["first", "second"]);
      expect(build([first], [archived({ id: second.id, wireIds: second.wireIds, body: second.body, authorRealJid: secondReal })]).map((message) => message.body)).toEqual(["first", "second"]);
    }
  });

  test("MAM canonical room tuple reconciles a nick change independently of aliases", () => {
    const first = row({ stanzaId: "room-stanza", stanzaIdBy: ROOM });
    const messages = build([first], [archived({ id: "new-envelope", nick: "new-nick", stanzaId: "room-stanza", stanzaIdBy: ROOM, authorRealJid: "sam@example.com" })]);
    expect(messages).toHaveLength(1);
    expect(messages[0]?.authorRealJid).toBe("sam@example.com");
  });

  test("MAM reconciliation preserves the presentation key in either seed mode", () => {
    const first = row({ rowKey: "stable-row", stanzaId: "room-stanza", stanzaIdBy: ROOM });
    for (const seedExistingOnly of [false, true]) {
      const messages = buildChannelTimelineFromMamResults({
        session, channelIsForum: false, existing: [first], options: { seedExistingOnly },
        mamResults: [archived({ id: "new-envelope", stanzaId: "room-stanza", stanzaIdBy: ROOM })],
      });
      expect(messages).toHaveLength(1);
      expect(timelineRowKey(messages[0]!)).toBe("stable-row");
    }
  });

  test("canonical-only MAM twins cannot lend their envelope alias to a new sender ID", () => {
    const first = row({ id: "room-stanza", stanzaId: "room-stanza", stanzaIdBy: ROOM, authorRealJid: "sam@example.com" });
    const reconciled = build([first], [archived({ id: "mam-twin", archiveId: "mam-twin", stanzaId: "room-stanza", stanzaIdBy: ROOM, authorRealJid: "sam@example.com" })]);
    expect(reconciled).toHaveLength(1);
    expect(build(reconciled, [archived({ id: "new-envelope", archiveId: "new-envelope", originId: "mam-twin", authorRealJid: "sam@example.com" })])).toHaveLength(2);
  });

  test("reconciled MAM envelopes never enter the authored ID namespace", () => {
    const first = row({ id: "mam-one", archiveId: "mam-one", originId: "real-origin", authorRealJid: "sam@example.com" });
    const reconciled = build([first], [archived({ id: "mam-two", archiveId: "mam-two", originId: "real-origin", authorRealJid: "sam@example.com" })]);
    expect(reconciled).toHaveLength(1);
    for (const forgedId of ["mam-one", "mam-two"]) {
      const messages = build(reconciled, [archived({ id: "new-mam", archiveId: "new-mam", originId: forgedId, authorRealJid: "sam@example.com", body: "different" })]);
      expect(messages).toHaveLength(2);
    }
  });

  test("the archive copy with the same room stanza-id still supplies the real JID", () => {
    const live = row({ createdAtSource: "fallback", stanzaId: "room-assigned-1", stanzaIdBy: ROOM });
    const messages = build([live], [archived({ createdAt: live.createdAt, authorRealJid: "sam@example.com", stanzaId: "room-assigned-1", stanzaIdBy: ROOM })]);
    expect(messages).toHaveLength(1);
    expect(messages[0]?.authorRealJid).toBe("sam@example.com");
  });
});
