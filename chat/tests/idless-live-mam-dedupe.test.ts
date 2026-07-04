// #1182: live stanzas lacking any wire identity (client id, XEP-0359
// stanza-id/origin-id) get a fabricated UUID as their primary id and can
// never dedupe against their MAM copy. These tests pin the codec marking
// (`synthesizedId`) and the content-based reconciliation fallback at both
// merge directions.

import { describe, expect, test } from "bun:test";

import { dmMessageFromArchived, roomMessageFromArchived } from "@/lib/xmpp/wasm-message-codecs";
import { buildChannelTimelineFromMamResults } from "../src/channels/message-timeline-state";
import { buildDmTimelineFromMamResults, fromLiveDmMessage } from "../src/dms/message-timeline-state";
import { mapLiveRoomMessageToTimeline } from "../src/channels/timeline";
import { insertLiveMessage } from "../src/lib/messaging/timeline-insert";
import type { WasmArchivedMessage } from "../src/lib/xmpp/wasm-types";
import type { WaddleSession } from "../src/lib/server-auth";

const session: WaddleSession = {
  session_id: "tok",
  user_id: "alice-id",
  username: "alice",
  avatar_url: null,
  xmpp_localpart: "alice",
  jid: "alice@example.com/desktop",
  xmpp_websocket_url: "wss://example.com/ws",
  is_expired: false,
  expires_at: null,
};

const baseArchivedRoom: WasmArchivedMessage = {
  mam_id: "fabricated-uuid-1",
  message_type: "groupchat",
  from: "room@conf.example.com/bob",
  to: "alice@example.com",
  body: "hello",
  reaction_emojis: [],
  is_muc: true,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

const baseArchivedDm: WasmArchivedMessage = {
  mam_id: "fabricated-uuid-2",
  message_type: "chat",
  from: "bob@example.com",
  to: "alice@example.com",
  body: "hello",
  reaction_emojis: [],
  is_muc: false,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

describe("codec synthesized-primary-id marking (#1182)", () => {
  test("live MUC stanza with no wire identity is flagged and gets no archiveId", () => {
    const result = roomMessageFromArchived(baseArchivedRoom, "live");
    expect(result?.id).toBe("fabricated-uuid-1");
    expect(result?.synthesizedId).toBe(true);
    expect(result?.archiveId).toBeUndefined();
  });

  test("live MUC stanza with a room-stamped stanza-id is not flagged", () => {
    const result = roomMessageFromArchived(
      {
        ...baseArchivedRoom,
        stanza_id: "room-stamped-1",
        stanza_id_by: "room@conf.example.com",
      },
      "live",
    );
    expect(result?.id).toBe("room-stamped-1");
    expect(result?.synthesizedId).toBeUndefined();
  });

  test("live DM stanza with no wire identity is flagged and gets no archiveId", () => {
    const result = dmMessageFromArchived(baseArchivedDm, "alice@example.com", "live");
    expect(result?.id).toBe("fabricated-uuid-2");
    expect(result?.synthesizedId).toBe(true);
    expect(result?.archiveId).toBeUndefined();
  });

  test("live DM stanza with an origin-id is not flagged", () => {
    const result = dmMessageFromArchived(
      { ...baseArchivedDm, origin_id: "origin-1" },
      "alice@example.com",
      "live",
    );
    expect(result?.id).toBe("origin-1");
    expect(result?.synthesizedId).toBeUndefined();
  });

  test("archive MUC row is never flagged and keeps its archiveId", () => {
    const result = roomMessageFromArchived({ ...baseArchivedRoom, mam_id: "mam-1" });
    expect(result?.synthesizedId).toBeUndefined();
    expect(result?.archiveId).toBe("mam-1");
  });
});

describe("channel MAM merge reconciles synthesized live rows (#1182)", () => {
  const liveIdless = (overrides: Partial<WasmArchivedMessage> = {}) =>
    roomMessageFromArchived(
      { ...baseArchivedRoom, timestamp: "2026-07-01T10:00:00.000Z", ...overrides },
      "live",
    )!;
  const mamCopy = (overrides: Partial<WasmArchivedMessage> = {}) =>
    roomMessageFromArchived({
      ...baseArchivedRoom,
      mam_id: "archive-uid-1",
      stanza_id: "archive-uid-1",
      stanza_id_by: "room@conf.example.com",
      timestamp: "2026-07-01T10:00:01.000Z",
      ...overrides,
    })!;

  test("MAM copy merges into the synthesized live row instead of duplicating", () => {
    const existing = [mapLiveRoomMessageToTimeline(session, liveIdless())];
    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [mamCopy()],
      existing,
    });
    expect(timeline).toHaveLength(1);
    const row = timeline[0]!;
    expect(row.id).toBe("archive-uid-1");
    expect(row.wireIds).toContain(existing[0]!.id);
    expect(row.synthesizedId).toBeUndefined();
    expect(row.replyableId).toBe("archive-uid-1");
  });

  test("a different-body MAM row still appends", () => {
    const existing = [mapLiveRoomMessageToTimeline(session, liveIdless())];
    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [mamCopy({ body: "something else" })],
      existing,
    });
    expect(timeline).toHaveLength(2);
  });

  test("a same-body MAM row outside the timestamp window still appends", () => {
    const existing = [mapLiveRoomMessageToTimeline(session, liveIdless())];
    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [mamCopy({ timestamp: "2026-07-01T11:00:00.000Z" })],
      existing,
    });
    expect(timeline).toHaveLength(2);
  });

  test("two identical MAM rows only consume the synthesized row once", () => {
    const existing = [mapLiveRoomMessageToTimeline(session, liveIdless())];
    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [
        mamCopy(),
        mamCopy({ mam_id: "archive-uid-2", stanza_id: "archive-uid-2" }),
      ],
      existing,
    });
    expect(timeline).toHaveLength(2);
    expect(timeline.map((m) => m.id).sort()).toEqual(["archive-uid-1", "archive-uid-2"]);
  });

  test("a same-body MAM row from a different occupant still appends", () => {
    const existing = [mapLiveRoomMessageToTimeline(session, liveIdless())];
    const timeline = buildChannelTimelineFromMamResults({
      session,
      channelIsForum: false,
      mamResults: [mamCopy({ from: "room@conf.example.com/carol" })],
      existing,
    });
    expect(timeline).toHaveLength(2);
  });
});

describe("DM MAM merge reconciles synthesized live rows (#1182)", () => {
  const liveIdlessDm = () =>
    dmMessageFromArchived(
      { ...baseArchivedDm, timestamp: "2026-07-01T10:00:00.000Z" },
      "alice@example.com",
      "live",
    )!;
  const mamDmCopy = (overrides: Partial<WasmArchivedMessage> = {}) =>
    dmMessageFromArchived(
      {
        ...baseArchivedDm,
        mam_id: "dm-archive-uid-1",
        timestamp: "2026-07-01T10:00:01.000Z",
        ...overrides,
      },
      "alice@example.com",
    )!;

  test("MAM copy merges into the synthesized live DM row instead of duplicating", () => {
    const existing = [fromLiveDmMessage(session, liveIdlessDm())];
    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [mamDmCopy()],
      existing,
    });
    expect(timeline).toHaveLength(1);
    const row = timeline[0]!;
    expect(row.id).toBe("dm-archive-uid-1");
    expect(row.wireIds).toContain(existing[0]!.id);
    expect(row.synthesizedId).toBeUndefined();
  });

  test("a same-body MAM row from the other conversation side still appends", () => {
    const existing = [fromLiveDmMessage(session, liveIdlessDm())];
    const timeline = buildDmTimelineFromMamResults({
      session,
      mamResults: [
        mamDmCopy({ from: "alice@example.com", to: "bob@example.com", id: "self-1" }),
      ],
      existing,
    });
    expect(timeline).toHaveLength(2);
  });
});

describe("live insert reconciles id-less redelivery onto MAM-seeded rows (#1182)", () => {
  const mamSeededRow = () =>
    mapLiveRoomMessageToTimeline(
      session,
      roomMessageFromArchived({
        ...baseArchivedRoom,
        mam_id: "archive-uid-1",
        stanza_id: "archive-uid-1",
        stanza_id_by: "room@conf.example.com",
        timestamp: "2026-07-01T10:00:01.000Z",
      })!,
    );
  // The dispatcher fabricates a fresh UUID per id-less delivery — mirror
  // that so two rows never share the fabricated primary id.
  const idlessLiveRow = (fabricatedId = crypto.randomUUID()) =>
    mapLiveRoomMessageToTimeline(
      session,
      roomMessageFromArchived(
        { ...baseArchivedRoom, mam_id: fabricatedId, timestamp: "2026-07-01T10:00:00.000Z" },
        "live",
      )!,
    );

  test("id-less live redelivery merges into the archive-keyed row", () => {
    const result = insertLiveMessage([mamSeededRow()], idlessLiveRow(), new Set());
    expect(result.appended).toBe(false);
    expect(result.messages).toHaveLength(1);
    const row = result.messages[0]!;
    expect(row.id).toBe("archive-uid-1");
    expect(row.synthesizedId).toBeUndefined();
  });

  test("two distinct id-less live messages with equal bodies never merge", () => {
    const first = insertLiveMessage([], idlessLiveRow(), new Set());
    const second = insertLiveMessage(first.messages, idlessLiveRow(), new Set());
    expect(second.appended).toBe(true);
    expect(second.messages).toHaveLength(2);
  });
});
