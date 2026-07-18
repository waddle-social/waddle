/**
 * Gap-safety of the reconnect catch-up cursor: live arrivals must NOT
 * advance the archive `after` cursor, even though they carry the
 * XEP-0359 stanza-id that is the XEP-0313 archive UID. A live stanza-id
 * cannot prove delivery continuity (a message can be missed while later
 * live messages still arrive — MUC gap during a kick/rejoin, XEP-0198
 * replay-window eviction), so catch-up re-fetches from the last
 * archive-CONFIRMED cursor and filters already-seen ids via `seenIds`.
 * The re-emitted archive rows are side-effect-free (see the
 * `fromArchive` flag below) so the re-fetch cannot corrupt unread or
 * notification state.
 */
import { afterEach, describe, expect, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient, type LiveRoomMessage } from "../src/lib/xmpp-client";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";
import { roomActivityEventFromMessage } from "../src/lib/xmpp/client-mam";
import type { ReconnectCatchup } from "../src/lib/xmpp/reconnect-catchup";

type PrivateState = {
  catchup: ReconnectCatchup;
  dispatchLiveBodyMessage: (message: unknown) => void;
};

function session(jid: string): WaddleSession {
  return {
    username: jid.split("@")[0],
    jid,
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

const createdClients: BrowserXmppClient[] = [];

function createClient(jid: string): BrowserXmppClient {
  const client = new BrowserXmppClient(session(jid), {
    durableRuntimeStore: new MemoryDurableOutboundStore(),
  });
  createdClients.push(client);
  return client;
}

function clientState(jid: string): PrivateState {
  return createClient(jid) as unknown as PrivateState;
}

afterEach(async () => {
  const clients = createdClients.splice(0);
  await Promise.all(clients.map((client) => client.dispose()));
});

describe("reconnect catch-up cursor gap-safety", () => {
  test("a live room stanza-id does not advance the archive `after` cursor", () => {
    const state = clientState("cursor-room@example.com/desktop");
    expect(state.catchup.onSessionStarted()).toEqual([]);
    state.catchup.recordRoomSeen(
      "general@conference.example.com",
      "2026-06-01T09:00:00.000Z",
      "archive-confirmed-1",
    );

    state.dispatchLiveBodyMessage({
      id: "wire-1",
      from: "general@conference.example.com/bob",
      to: "cursor-room@example.com/desktop",
      message_type: "groupchat",
      body: "hello room",
      timestamp: "2026-06-01T10:00:00.000Z",
      stanza_ids: [{ id: "room-arch-42", by: "general@conference.example.com" }],
      reaction_emojis: [],
      is_muc: true,
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
    });

    const entries = state.catchup.onSessionStarted();
    expect(entries).toHaveLength(1);
    // The cursor stays at the archive-confirmed point; the live message's
    // ids join `seenIds` so its re-fetch is filtered, not re-emitted.
    expect(entries[0]).toMatchObject({
      kind: "room",
      key: "general@conference.example.com",
      after: "archive-confirmed-1",
    });
    expect((entries[0] as { seenIds?: string[] }).seenIds).toContain("room-arch-42");
  });

  test("a live-only room keeps the timestamp fallback (no fabricated archive cursor)", () => {
    const state = clientState("cursor-fallback@example.com/desktop");
    expect(state.catchup.onSessionStarted()).toEqual([]);

    state.dispatchLiveBodyMessage({
      id: "wire-3",
      from: "general@conference.example.com/bob",
      to: "cursor-fallback@example.com/desktop",
      message_type: "groupchat",
      body: "seen live only",
      timestamp: "2026-06-01T12:00:00.000Z",
      stanza_ids: [{ id: "room-arch-7", by: "general@conference.example.com" }],
      reaction_emojis: [],
      is_muc: true,
      markup_spans: [],
      mention_uris: [],
      references: [],
      is_sticker: false,
      shared_files: [],
    });

    const entries = state.catchup.onSessionStarted();
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      kind: "room",
      key: "general@conference.example.com",
      since: "2026-06-01T12:00:00.000Z",
    });
    expect(entries[0]).not.toHaveProperty("after");
  });
});

describe("archive-sourced activity flag", () => {
  const base: LiveRoomMessage = {
    id: "m-1",
    roomJid: "general@conference.example.com",
    nick: "bob",
    body: "hi",
    createdAt: "2026-06-01T10:00:00.000Z",
    createdAtSource: "archive",
    type: "message",
  };

  test("roomActivityEventFromMessage marks archive decodes", () => {
    expect(roomActivityEventFromMessage(base).fromArchive).toBe(true);
  });

  test("live decodes carry no archive flag", () => {
    const live = { ...base, createdAtSource: "delay" as const };
    expect(roomActivityEventFromMessage(live).fromArchive).toBeUndefined();
  });
});
