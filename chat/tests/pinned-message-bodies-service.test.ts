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

function archived(id: string, body = "live body"): WasmArchivedMessage {
  return {
    id,
    mam_id: id,
    message_type: "groupchat",
    from: "room@conf.example/alice",
    body,
    timestamp: "2026-05-11T11:50:00Z",
    reaction_emojis: [],
    is_muc: true,
    mention_uris: [],
    references: [],
    markup_spans: [],
    is_sticker: false,
    shared_files: [],
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
    const client = {
      fetchRoomMessagesByStanzaIds: mock(async (_s: string, _c: string, ids: string[]) =>
        ids.map((id) => archived(id)),
      ),
    };
    hydratePinnedRoom("room@conf.example", [pinEntry("sid-A"), pinEntry("sid-B")]);

    await hydratePinnedBodiesOnPanelOpen({
      client,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@conf.example",
      timelineMessages: [],
      convert: fakeConvert,
    });

    expect(client.fetchRoomMessagesByStanzaIds).toHaveBeenCalledWith(
      "space1",
      "channel1",
      ["sid-A", "sid-B"],
    );
    const room = $pinnedMessageBodies.get().get("room@conf.example");
    expect(room?.size).toBe(2);
  });

  it("skips fetching ids that already resolve from the timeline", async () => {
    const client = {
      fetchRoomMessagesByStanzaIds: mock(async (_s: string, _c: string, ids: string[]) =>
        ids.map((id) => archived(id)),
      ),
    };
    hydratePinnedRoom("room@conf.example", [pinEntry("sid-A"), pinEntry("sid-B")]);

    await hydratePinnedBodiesOnPanelOpen({
      client,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@conf.example",
      timelineMessages: [
        { id: "sid-A", author: "alice", body: "live", createdAt: "2026-05-11T11:50:00Z", isSelf: false } as TimelineMessage,
      ],
      convert: fakeConvert,
    });

    expect(client.fetchRoomMessagesByStanzaIds).toHaveBeenCalledWith(
      "space1",
      "channel1",
      ["sid-B"],
    );
  });

  it("is a no-op when every id is already in the timeline", async () => {
    const client = { fetchRoomMessagesByStanzaIds: mock(async () => []) };
    hydratePinnedRoom("room@x", [pinEntry("sid-A")]);
    await hydratePinnedBodiesOnPanelOpen({
      client,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      timelineMessages: [
        { id: "sid-A", author: "alice", body: "x", createdAt: "x", isSelf: false } as TimelineMessage,
      ],
      convert: fakeConvert,
    });
    expect(client.fetchRoomMessagesByStanzaIds).not.toHaveBeenCalled();
  });

  it("skips fetching ids that are already in the body cache", async () => {
    const client = {
      fetchRoomMessagesByStanzaIds: mock(async (_s: string, _c: string, ids: string[]) =>
        ids.map((id) => archived(id)),
      ),
    };
    hydratePinnedRoom("room@conf.example", [pinEntry("sid-A"), pinEntry("sid-B")]);
    // Pre-populate the cache with sid-A so only sid-B should be fetched.
    cachePinnedMessageBody(
      "room@conf.example",
      "sid-A",
      fakeConvert(archived("sid-A")),
      pinnedMessageBodiesEpoch(),
    );

    await hydratePinnedBodiesOnPanelOpen({
      client,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@conf.example",
      timelineMessages: [],
      convert: fakeConvert,
    });

    expect(client.fetchRoomMessagesByStanzaIds).toHaveBeenCalledWith(
      "space1",
      "channel1",
      ["sid-B"],
    );
  });
});

describe("hydrateSinglePinnedBody", () => {
  beforeEach(() => {
    resetPinnedRooms();
    resetPinnedMessageBodies();
  });

  it("fetches the single id when not in the timeline", async () => {
    const client = {
      fetchRoomMessagesByStanzaIds: mock(async (_s: string, _c: string, _ids: string[]) =>
        [archived("sid-new")],
      ),
    };
    await hydrateSinglePinnedBody({
      client,
      spaceId: "space1",
      channelId: "channel1",
      roomJid: "room@x",
      stanzaId: "sid-new",
      timelineMessages: [],
      convert: fakeConvert,
    });
    expect(client.fetchRoomMessagesByStanzaIds).toHaveBeenCalledWith(
      "space1",
      "channel1",
      ["sid-new"],
    );
    expect($pinnedMessageBodies.get().get("room@x")?.get("sid-new")).toBeTruthy();
  });

  it("short-circuits when id is in the timeline", async () => {
    const client = { fetchRoomMessagesByStanzaIds: mock(async () => []) };
    await hydrateSinglePinnedBody({
      client,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      stanzaId: "sid-new",
      timelineMessages: [
        { id: "sid-new", author: "alice", body: "x", createdAt: "x", isSelf: false } as TimelineMessage,
      ],
      convert: fakeConvert,
    });
    expect(client.fetchRoomMessagesByStanzaIds).not.toHaveBeenCalled();
  });

  it("short-circuits when id is already cached", async () => {
    const client = { fetchRoomMessagesByStanzaIds: mock(async () => []) };
    cachePinnedMessageBody(
      "room@x",
      "sid-cached",
      fakeConvert(archived("sid-cached")),
      pinnedMessageBodiesEpoch(),
    );
    await hydrateSinglePinnedBody({
      client,
      spaceId: "s",
      channelId: "c",
      roomJid: "room@x",
      stanzaId: "sid-cached",
      timelineMessages: [],
      convert: fakeConvert,
    });
    expect(client.fetchRoomMessagesByStanzaIds).not.toHaveBeenCalled();
  });
});
