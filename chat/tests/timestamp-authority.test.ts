// End-to-end tests for the PR1 timestamp-authority fix.
//
// These cover:
//   * the codec stamps `createdAtSource` correctly when `timestamp`
//     is present vs. absent and when called from a live vs. archive
//     entry point (XEP-0203 / XEP-0313 distinction)
//   * `mergeLiveMessage` (DM + channel) keeps the higher-authority
//     `createdAt` when a redelivered live stanza arrives after a
//     MAM hit has already populated the row (the user-visible bug)
//   * `buildDmTimelineFromMamResults` dedupes against `wireIds`
//     aliases (the Bug 5 channel-vs-DM divergence)

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";

import { useDmLiveMerge } from "../src/dms/live-merge";
import { useChannelLiveMerge } from "../src/channels/live-merge";
import { buildDmTimelineFromMamResults, fromLiveDmMessage } from "../src/dms/message-timeline-state";
import { mapLiveRoomMessageToTimeline } from "../src/channels/timeline";
import { dmMessageFromArchived, roomMessageFromArchived } from "@/lib/xmpp/wasm-message-codecs";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { LiveDmMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { WasmArchivedMessage } from "../src/lib/xmpp/wasm-types";

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

const baseArchivedDm: WasmArchivedMessage = {
  mam_id: "mam-dm-1",
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

const baseArchivedRoom: WasmArchivedMessage = {
  mam_id: "mam-room-1",
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

describe("codec createdAtSource tagging", () => {
  test("dmMessageFromArchived defaults to archive source and trusts wire timestamp", () => {
    const result = dmMessageFromArchived(
      { ...baseArchivedDm, id: "m1", timestamp: "2026-05-20T10:00:00.000Z" },
      "alice@example.com",
    );
    expect(result?.createdAt).toBe("2026-05-20T10:00:00.000Z");
    expect(result?.createdAtSource).toBe("archive");
  });

  test("dmMessageFromArchived('live') tags a server-stamped live arrival as delay", () => {
    const result = dmMessageFromArchived(
      { ...baseArchivedDm, id: "m1", timestamp: "2026-05-20T10:00:00.000Z" },
      "alice@example.com",
      "live",
    );
    expect(result?.createdAtSource).toBe("delay");
  });

  test("dmMessageFromArchived('live') without `timestamp` falls back and tags as fallback", () => {
    const result = dmMessageFromArchived(
      { ...baseArchivedDm, id: "m1", timestamp: undefined },
      "alice@example.com",
      "live",
    );
    expect(result?.createdAtSource).toBe("fallback");
    expect(typeof result?.createdAt).toBe("string");
  });

  test("roomMessageFromArchived defaults to archive source", () => {
    const result = roomMessageFromArchived(
      { ...baseArchivedRoom, id: "r1", timestamp: "2026-05-20T10:00:00.000Z" },
    );
    expect(result?.createdAtSource).toBe("archive");
  });

  test("roomMessageFromArchived('live') without `timestamp` is fallback", () => {
    const result = roomMessageFromArchived(
      { ...baseArchivedRoom, id: "r1", timestamp: undefined },
      "live",
    );
    expect(result?.createdAtSource).toBe("fallback");
  });
});

function dmHarness() {
  const messages = ref<TimelineMessage[]>([]);
  const pendingEchoClientIds = new Set<string>();
  const liveMerge = useDmLiveMerge({
    session: ref(session),
    messages,
    activePeerJid: ref("bob@example.com"),
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin: mock(async () => true),
    persistLastSeen: mock(() => {}),
    isFeedVisible: () => true,
  });
  return { messages, pendingEchoClientIds, liveMerge };
}

function channelHarness() {
  const messages = ref<TimelineMessage[]>([]);
  const pendingEchoClientIds = new Set<string>();
  const liveMerge = useChannelLiveMerge({
    session: ref(session),
    messages,
    activeChannelId: ref("channel-1"),
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin: mock(async () => true),
    persistLastSeen: mock(() => {}),
  });
  return { messages, liveMerge };
}

describe("mergeLiveMessage timestamp authority", () => {
  test("DM: redelivered fallback stanza does NOT overwrite an archive stamp", () => {
    // Setup: the MAM catch-up already inserted the row with the
    // server-authoritative `archive` stamp.
    const h = dmHarness();
    h.messages.value = [
      {
        id: "m1",
        body: "hello",
        author: "bob",
        authorJid: "bob@example.com",
        createdAt: "2026-05-20T10:00:00.000Z",
        createdAtSource: "archive",
        isSelf: false,
      },
    ];
    // The buffered live drain replays the same message but the WASM
    // parser didn't see a `<delay/>` so the codec stamped it `fallback`
    // with `Date.now()`.
    h.liveMerge.mergeLiveMessage({
      id: "m1",
      body: "hello",
      author: "bob",
      authorJid: "bob@example.com",
      createdAt: "2026-05-20T11:00:00.000Z",
      createdAtSource: "fallback",
      isSelf: false,
    });
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.createdAt).toBe("2026-05-20T10:00:00.000Z");
    expect(h.messages.value[0]?.createdAtSource).toBe("archive");
  });

  test("DM: an incoming `delay` stamp still upgrades over `queued`", () => {
    const h = dmHarness();
    h.messages.value = [
      {
        id: "client-1",
        body: "hi",
        author: "alice",
        authorJid: "alice@example.com",
        createdAt: "2026-05-20T10:00:01.500Z",
        createdAtSource: "queued",
        isSelf: true,
        deliveryStatus: "sending",
      },
    ];
    h.liveMerge.mergeLiveMessage({
      id: "client-1",
      body: "hi",
      author: "alice",
      authorJid: "alice@example.com",
      createdAt: "2026-05-20T10:00:00.000Z",
      createdAtSource: "delay",
      isSelf: true,
    });
    expect(h.messages.value[0]?.createdAt).toBe("2026-05-20T10:00:00.000Z");
    expect(h.messages.value[0]?.createdAtSource).toBe("delay");
    expect(h.messages.value[0]?.deliveryStatus).toBe("delivered");
  });

  test("channel: redelivered fallback stanza does NOT overwrite a delay stamp", () => {
    const h = channelHarness();
    h.messages.value = [
      {
        id: "r1",
        wireIds: ["origin-r1"],
        body: "hello",
        author: "bob",
        authorJid: "room@conf.example.com/bob",
        authorOccupantJid: "room@conf.example.com/bob",
        createdAt: "2026-05-20T10:00:00.000Z",
        createdAtSource: "delay",
        isSelf: false,
      },
    ];
    h.liveMerge.mergeLiveMessage({
      id: "r1",
      body: "hello",
      author: "bob",
      authorJid: "room@conf.example.com/bob",
      authorOccupantJid: "room@conf.example.com/bob",
      createdAt: "2026-05-20T12:00:00.000Z",
      createdAtSource: "fallback",
      isSelf: false,
    });
    expect(h.messages.value[0]?.createdAt).toBe("2026-05-20T10:00:00.000Z");
    expect(h.messages.value[0]?.createdAtSource).toBe("delay");
  });
});

describe("buildDmTimelineFromMamResults wireIds dedup (Bug 5)", () => {
  test("MAM hit using stanza-id as primary id dedupes against existing row's wireIds alias", () => {
    // Existing live row inserted with the client-side id; the
    // canonical stanza-id is held in `wireIds`.
    const existing: TimelineMessage[] = [
      {
        id: "client-id",
        wireIds: ["server-stanza-id"],
        body: "hello",
        author: "bob",
        authorJid: "bob@example.com",
        createdAt: "2026-05-20T11:00:00.000Z",
        createdAtSource: "fallback",
        isSelf: false,
      },
    ];
    // MAM result returns the same logical message but with the
    // server stanza-id as the primary id.
    const mam: LiveDmMessage[] = [
      {
        id: "server-stanza-id",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com",
        nick: "bob",
        body: "hello",
        createdAt: "2026-05-20T10:00:00.000Z",
        createdAtSource: "archive",
        type: "message",
      },
    ];
    const result = buildDmTimelineFromMamResults({ session, mamResults: mam, existing });
    expect(result).toHaveLength(1);
    // Existing row is updated in place; its createdAt is upgraded to
    // the MAM `archive` stamp.
    expect(result[0]?.createdAt).toBe("2026-05-20T10:00:00.000Z");
    expect(result[0]?.createdAtSource).toBe("archive");
  });

  test("MAM hit with a totally new id appends rather than duplicating", () => {
    const existing: TimelineMessage[] = [
      {
        id: "existing-1",
        body: "first",
        author: "bob",
        authorJid: "bob@example.com",
        createdAt: "2026-05-20T10:00:00.000Z",
        createdAtSource: "archive",
        isSelf: false,
      },
    ];
    const mam: LiveDmMessage[] = [
      {
        id: "different-id",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com",
        nick: "bob",
        body: "second",
        createdAt: "2026-05-20T10:00:01.000Z",
        createdAtSource: "archive",
        type: "message",
      } satisfies LiveDmMessage,
    ];
    const result = buildDmTimelineFromMamResults({ session, mamResults: mam, existing });
    expect(result).toHaveLength(2);
  });
});

describe("end-to-end tab-restore scenario (codec → merge)", () => {
  test("DM: MAM hit lands first, then redelivered live stanza with no <delay/> does NOT corrupt the row", () => {
    // Simulate exactly the user-visible scenario:
    //   1. Tab is resumed; MAM catch-up decodes a `WasmArchivedMessage`
    //      with the server delay stamp into the timeline.
    //   2. The buffered live drain replays the SAME message but with
    //      no `<delay/>` — the WASM parser leaves `timestamp` as None
    //      (Option<String>), the codec falls back to `Date.now()`,
    //      and the live merge spread would historically have
    //      overwritten the row's `createdAt`.
    const h = dmHarness();
    const archive = dmMessageFromArchived(
      { ...baseArchivedDm, id: "msg-1", timestamp: "2026-05-20T10:00:00.000Z" },
      "alice@example.com",
    );
    expect(archive).not.toBeNull();
    h.messages.value = [fromLiveDmMessage(session, archive!)];
    expect(h.messages.value[0]?.createdAtSource).toBe("archive");

    const live = dmMessageFromArchived(
      { ...baseArchivedDm, id: "msg-1", timestamp: undefined },
      "alice@example.com",
      "live",
    );
    expect(live).not.toBeNull();
    expect(live?.createdAtSource).toBe("fallback");
    h.liveMerge.mergeLiveMessage(fromLiveDmMessage(session, live!));

    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.createdAt).toBe("2026-05-20T10:00:00.000Z");
    expect(h.messages.value[0]?.createdAtSource).toBe("archive");
  });

  test("channel: same scenario via roomMessageFromArchived → mapLiveRoomMessageToTimeline → mergeLiveMessage", () => {
    const h = channelHarness();
    const archive = roomMessageFromArchived(
      { ...baseArchivedRoom, id: "msg-1", timestamp: "2026-05-20T10:00:00.000Z" },
    );
    expect(archive).not.toBeNull();
    h.messages.value = [mapLiveRoomMessageToTimeline(session, archive!)];
    expect(h.messages.value[0]?.createdAtSource).toBe("archive");

    const live = roomMessageFromArchived(
      { ...baseArchivedRoom, id: "msg-1", timestamp: undefined },
      "live",
    );
    expect(live).not.toBeNull();
    expect(live?.createdAtSource).toBe("fallback");
    h.liveMerge.mergeLiveMessage(mapLiveRoomMessageToTimeline(session, live!));

    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.createdAt).toBe("2026-05-20T10:00:00.000Z");
    expect(h.messages.value[0]?.createdAtSource).toBe("archive");
  });
});
