import { describe, expect, it, beforeEach, mock } from "bun:test";
import {
  hydratePinnedBodiesOnPanelOpen,
  hydrateSinglePinnedBody,
} from "@/services/pinned-message-bodies";
import {
  $pinnedMessageBodies,
  resetPinnedMessageBodies,
  cachePinnedMessageBody,
  pinnedMessageBodiesEpoch,
} from "@/stores/pinned-message-bodies";
import { resetPinnedRooms, hydratePinnedRoom } from "@/stores/pinned-messages";
import type { WasmPinEntry, WasmArchivedMessage } from "@/lib/xmpp/wasm-types";
import type { TimelineMessage } from "@/lib/chat-ui";

function pinEntry(id: string, text = ""): WasmPinEntry {
  return {
    target_stanza_id: id,
    pinner_jid: "admin@example.com",
    pinned_at: "2026-05-11T12:00:00Z",
    preview: {
      author_jid: "alice@example.com",
      text,
      message_timestamp: "2026-05-11T11:50:00Z",
    },
  };
}

function archived(id: string, body = "live body", roomJid = "room@conf.example"): WasmArchivedMessage {
  return {
    id,
    mam_id: id,
    message_type: "groupchat",
    from: `${roomJid}/alice`,
    body,
    timestamp: "2026-05-11T11:50:00Z",
    reaction_emojis: [],
    is_muc: true,
    mention_uris: [],
    references: [],
    markup_spans: [],
    is_sticker: false,
    shared_files: [],
    // XEP-0359 room-scoped stanza-id: branch 1 of matchRequestedStanzaId.
    stanza_ids: [{ by: roomJid, id }],
  } as unknown as WasmArchivedMessage;
}

function fakeConvert(a: WasmArchivedMessage): TimelineMessage {
  return {
    id: (a.id ?? a.mam_id) as string,
    author: "alice",
    body: a.body ?? "",
    createdAt: a.timestamp ?? "2026-05-11T11:50:00Z",
    isSelf: false,
  } as TimelineMessage;
}

describe("hydratePinnedBodiesOnPanelOpen", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("fetches every stanza-id not already in the timeline", async () => {
    const fetchByStanzaIds = mock(async (ids: string[]) => ids.map((id) => archived(id)));
    hydratePinnedRoom("room@conf.example", [pinEntry("sid-A"), pinEntry("sid-B")]);

    await hydratePinnedBodiesOnPanelOpen({
      fetchByStanzaIds,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@conf.example",
      timelineMessages: [],
      convert: fakeConvert,
    });

    expect(fetchByStanzaIds).toHaveBeenCalledWith(["sid-A", "sid-B"]);
    const room = $pinnedMessageBodies.get().get("room@conf.example");
    expect(room?.size).toBe(2);
  });

  it("skips fetching ids that already resolve from the timeline", async () => {
    const fetchByStanzaIds = mock(async (ids: string[]) => ids.map((id) => archived(id)));
    hydratePinnedRoom("room@conf.example", [pinEntry("sid-A"), pinEntry("sid-B")]);

    await hydratePinnedBodiesOnPanelOpen({
      fetchByStanzaIds,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@conf.example",
      timelineMessages: [
        { id: "sid-A", author: "alice", body: "live", createdAt: "2026-05-11T11:50:00Z", isSelf: false } as TimelineMessage,
      ],
      convert: fakeConvert,
    });

    expect(fetchByStanzaIds).toHaveBeenCalledWith(["sid-B"]);
  });

  it("is a no-op when every id is already in the timeline", async () => {
    const fetchByStanzaIds = mock(async () => []);
    hydratePinnedRoom("room@x", [pinEntry("sid-A")]);
    await hydratePinnedBodiesOnPanelOpen({
      fetchByStanzaIds,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      timelineMessages: [
        { id: "sid-A", author: "alice", body: "x", createdAt: "x", isSelf: false } as TimelineMessage,
      ],
      convert: fakeConvert,
    });
    expect(fetchByStanzaIds).not.toHaveBeenCalled();
  });

  it("is a no-op when the room has no pinned entries", async () => {
    const fetchByStanzaIds = mock(async () => []);
    hydratePinnedRoom("room@x", []);

    await hydratePinnedBodiesOnPanelOpen({
      fetchByStanzaIds,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@x",
      timelineMessages: [],
      convert: fakeConvert,
    });

    expect(fetchByStanzaIds).not.toHaveBeenCalled();
  });

  it("skips fetching ids that are already in the body cache", async () => {
    const fetchByStanzaIds = mock(async (ids: string[]) => ids.map((id) => archived(id)));
    hydratePinnedRoom("room@conf.example", [pinEntry("sid-A"), pinEntry("sid-B")]);
    // Pre-populate the cache with sid-A so only sid-B should be fetched.
    cachePinnedMessageBody(
      "room@conf.example",
      "sid-A",
      fakeConvert(archived("sid-A")),
      pinnedMessageBodiesEpoch(),
    );

    await hydratePinnedBodiesOnPanelOpen({
      fetchByStanzaIds,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@conf.example",
      timelineMessages: [],
      convert: fakeConvert,
    });

    expect(fetchByStanzaIds).toHaveBeenCalledWith(["sid-B"]);
  });
});

describe("hydrateSinglePinnedBody", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("fetches the single id when not in the timeline", async () => {
    const fetchByStanzaIds = mock(async () => [archived("sid-new", "live body", "room@x")]);
    await hydrateSinglePinnedBody({
      fetchByStanzaIds,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@x",
      stanzaId: "sid-new",
      timelineMessages: [],
      convert: fakeConvert,
    });
    expect(fetchByStanzaIds).toHaveBeenCalledWith(["sid-new"]);
    expect($pinnedMessageBodies.get().get("room@x")?.get("sid-new")).toBeTruthy();
  });

  it("short-circuits when id is in the timeline", async () => {
    const fetchByStanzaIds = mock(async () => []);
    await hydrateSinglePinnedBody({
      fetchByStanzaIds,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      stanzaId: "sid-new",
      timelineMessages: [
        { id: "sid-new", author: "alice", body: "x", createdAt: "x", isSelf: false } as TimelineMessage,
      ],
      convert: fakeConvert,
    });
    expect(fetchByStanzaIds).not.toHaveBeenCalled();
  });

  it("short-circuits when id is already cached", async () => {
    const fetchByStanzaIds = mock(async () => []);
    cachePinnedMessageBody(
      "room@x",
      "sid-cached",
      fakeConvert(archived("sid-cached")),
      pinnedMessageBodiesEpoch(),
    );
    await hydrateSinglePinnedBody({
      fetchByStanzaIds,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      stanzaId: "sid-cached",
      timelineMessages: [],
      convert: fakeConvert,
    });
    expect(fetchByStanzaIds).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// matchRequestedStanzaId branch coverage
// ---------------------------------------------------------------------------

describe("matchRequestedStanzaId — branch 2: singular stanza_id field", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("caches body under singular stanza_id when stanza_ids array is absent", async () => {
    // Branch 2: no stanza_ids array, but stanza_id + stanza_id_by are set.
    const archivedFixture = {
      id: "wire-id",
      mam_id: "mam-id",
      nick: "alice",
      body: "branch-2 body",
      createdAt: "x",
      roomJid: "room@x",
      message_type: "groupchat",
      reaction_emojis: [],
      is_muc: true,
      mention_uris: [],
      references: [],
      markup_spans: [],
      is_sticker: false,
      shared_files: [],
      stanza_id: "uuid-X",
      stanza_id_by: "room@x",
      // deliberately no stanza_ids array — branch 1 must not match
    } as unknown as WasmArchivedMessage;

    const fetchByStanzaIds = mock(async () => [archivedFixture]);
    hydratePinnedRoom("room@x", [pinEntry("uuid-X")]);

    await hydratePinnedBodiesOnPanelOpen({
      fetchByStanzaIds,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      timelineMessages: [],
      convert: fakeConvert,
    });

    expect($pinnedMessageBodies.get().get("room@x")?.get("uuid-X")?.body).toBe("branch-2 body");
  });
});

describe("matchRequestedStanzaId — personal archive stanza-id", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("matches a requested stanza-id even when by is a personal archive JID", async () => {
    const archivedFixture = {
      id: "uuid-canonical",
      mam_id: "mam-id",
      nick: "alice",
      body: "foreign body",
      createdAt: "x",
      roomJid: "room@x",
      message_type: "groupchat",
      reaction_emojis: [],
      is_muc: true,
      mention_uris: [],
      references: [],
      markup_spans: [],
      is_sticker: false,
      shared_files: [],
      stanza_ids: [{ by: "alice@example.com", id: "uuid-canonical" }],
    } as unknown as WasmArchivedMessage;

    const fetchByStanzaIds = mock(async () => [archivedFixture]);
    hydratePinnedRoom("room@x", [pinEntry("uuid-canonical")]);

    await hydratePinnedBodiesOnPanelOpen({
      fetchByStanzaIds,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      timelineMessages: [],
      convert: fakeConvert,
    });

    const room = $pinnedMessageBodies.get().get("room@x");
    expect(room?.get("uuid-canonical")?.body).toBe("foreign body");
  });
});

// ---------------------------------------------------------------------------
// Bug A & B regression tests
// ---------------------------------------------------------------------------

describe("hydratePinnedBodiesOnPanelOpen — alias + requested-id fixes", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("skips fetching when the requested id is in timeline under reactionTargetId alias", async () => {
    const fetchByStanzaIds = mock(async () => []);
    hydratePinnedRoom("room@x", [pinEntry("uuid-canonical")]);

    await hydratePinnedBodiesOnPanelOpen({
      fetchByStanzaIds,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      timelineMessages: [
        {
          id: "wire-id",
          author: "alice",
          body: "body",
          createdAt: "x",
          isSelf: false,
          reactionTargetId: "uuid-canonical",
        } as TimelineMessage,
      ],
      convert: fakeConvert,
    });

    expect(fetchByStanzaIds).not.toHaveBeenCalled();
  });

  it("caches body under requested stanza-id even when result's id is the wire message-id", async () => {
    // Simulate the production case: server returns archived row whose
    // stanza_ids array contains the canonical room-scoped UUID. The
    // convert function returns a TimelineMessage whose `id` is the wire
    // message-id (a different string). The cache MUST be keyed by the
    // requested canonical id, not the wire id.
    const archivedFixture = {
      id: "wire-id",
      mam_id: "uuid-canonical",
      nick: "alice",
      body: "live",
      createdAt: "x",
      roomJid: "room@x",
      message_type: "groupchat",
      reaction_emojis: [],
      is_muc: true,
      mention_uris: [],
      references: [],
      markup_spans: [],
      is_sticker: false,
      shared_files: [],
      stanza_ids: [{ by: "room@x", id: "uuid-canonical" }],
    } as unknown as WasmArchivedMessage;

    const fetchByStanzaIds = mock(async () => [archivedFixture]);
    hydratePinnedRoom("room@x", [pinEntry("uuid-canonical")]);

    // Convert maps the archived to a TimelineMessage whose `id` is the
    // wire id, not the canonical uuid.
    const convertWithWireId = (a: WasmArchivedMessage): TimelineMessage => ({
      id: "wire-id",
      author: a.nick ?? "alice",
      body: a.body ?? "",
      createdAt: a.timestamp ?? "x",
      isSelf: false,
      reactionTargetId: "uuid-canonical",
    } as TimelineMessage);

    await hydratePinnedBodiesOnPanelOpen({
      fetchByStanzaIds,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      timelineMessages: [],
      convert: convertWithWireId,
    });

    expect($pinnedMessageBodies.get().get("room@x")?.get("uuid-canonical")?.body).toBe("live");
  });
});
