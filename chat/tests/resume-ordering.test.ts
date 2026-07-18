/**
 * Resume-time ordering: live body messages received during the MAM
 * catch-up window must be buffered and replayed after pagination
 * completes, so they cannot race the catch-up cursor and cause the
 * next `before=` page fetch to skip or duplicate messages near the
 * page boundary.
 *
 * These tests exercise the resume-gate in `handleMessage` directly.
 * Driving the full session lifecycle through a mocked
 * `XmppClientInstance` would require modelling the WASM client; the
 * gate's contract is independent of that machinery — when
 * `pendingDuringResume` is an array, live arrivals are pushed onto
 * it; when it is `null` (the post-drain state), they dispatch
 * synchronously.
 */
import { afterEach, describe, expect, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient, type LiveDmMessage, type LiveRoomMessage } from "../src/lib/xmpp-client";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";

type PrivateState = {
  pendingDuringResume: unknown[] | null;
  handleMessage: (message: unknown) => void;
};

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

const createdClients: BrowserXmppClient[] = [];

function createClient(): BrowserXmppClient {
  const client = new BrowserXmppClient(session(), {
    durableRuntimeStore: new MemoryDurableOutboundStore(),
  });
  createdClients.push(client);
  return client;
}

afterEach(async () => {
  const clients = createdClients.splice(0);
  await Promise.all(clients.map((client) => client.dispose()));
});

function dmWasmMessage(id: string, body: string, timestamp: string) {
  return {
    mam_id: id,
    id,
    from: "bob@example.com/phone",
    to: "alice@example.com/desktop",
    message_type: "chat",
    body,
    timestamp,
    reaction_emojis: [],
    shared_files: [],
    is_muc: false,
  };
}

function roomWasmMessage(id: string, body: string, timestamp: string) {
  return {
    mam_id: id,
    id,
    from: "general@conference.example.com/bob",
    to: "alice@example.com/desktop",
    message_type: "groupchat",
    body,
    timestamp,
    reaction_emojis: [],
    is_muc: true,
    markup_spans: [],
    mention_uris: [],
    references: [],
    is_sticker: false,
    shared_files: [],
  };
}

describe("resume-time live-message buffering", () => {
  test("DM live messages arriving while the resume buffer is open are deferred", () => {
    const client = createClient();
    const state = client as unknown as PrivateState;
    const seen: LiveDmMessage[] = [];
    client.setDirectMessageHandler((message) => { seen.push(message); });

    state.pendingDuringResume = [];
    state.handleMessage(dmWasmMessage("dm-1", "during-catchup-1", "2026-05-20T10:00:00.000Z"));
    state.handleMessage(dmWasmMessage("dm-2", "during-catchup-2", "2026-05-20T10:00:01.000Z"));

    expect(seen).toHaveLength(0);
    expect(state.pendingDuringResume).toHaveLength(2);
  });

  test("room live messages arriving while the resume buffer is open are deferred", () => {
    const client = createClient();
    const state = client as unknown as PrivateState;
    const seen: LiveRoomMessage[] = [];
    client.setMessageHandler((message) => { seen.push(message); });
    // Match the room the message is addressed to so the activity-gate doesn't
    // divert it to the activityHandler.
    (client as unknown as { currentRoom: string }).currentRoom = "general@conference.example.com";

    state.pendingDuringResume = [];
    state.handleMessage(roomWasmMessage("room-1", "during-catchup-room-1", "2026-05-20T10:00:00.000Z"));

    expect(seen).toHaveLength(0);
    expect(state.pendingDuringResume).toHaveLength(1);
  });

  test("closing the resume buffer and dispatching it fires handlers in arrival order", () => {
    const client = createClient();
    const state = client as unknown as PrivateState & {
      dispatchLiveBodyMessage: (message: unknown) => void;
    };
    const seen: LiveDmMessage[] = [];
    client.setDirectMessageHandler((message) => { seen.push(message); });

    state.pendingDuringResume = [];
    state.handleMessage(dmWasmMessage("dm-1", "first", "2026-05-20T10:00:00.000Z"));
    state.handleMessage(dmWasmMessage("dm-2", "second", "2026-05-20T10:00:01.000Z"));
    state.handleMessage(dmWasmMessage("dm-3", "third", "2026-05-20T10:00:02.000Z"));
    expect(seen).toHaveLength(0);

    // Drain — mirror the finally-block in runSessionReady.
    const buffered = state.pendingDuringResume as Parameters<typeof state.dispatchLiveBodyMessage>[0][];
    state.pendingDuringResume = null;
    for (const message of buffered) state.dispatchLiveBodyMessage(message);

    expect(seen.map((m) => m.body)).toEqual(["first", "second", "third"]);
  });

  test("live messages arriving after the drain bypass the buffer and dispatch immediately", () => {
    const client = createClient();
    const state = client as unknown as PrivateState;
    const seen: LiveDmMessage[] = [];
    client.setDirectMessageHandler((message) => { seen.push(message); });

    // `pendingDuringResume === null` (the post-catchup state) means
    // live arrivals dispatch synchronously.
    expect(state.pendingDuringResume).toBeNull();
    state.handleMessage(dmWasmMessage("dm-late", "after-resume", "2026-05-20T10:00:03.000Z"));
    expect(seen).toHaveLength(1);
  });

  test("ephemeral events (chat-state) bypass the resume buffer; displayed markers defer (#1165)", () => {
    const client = createClient();
    const state = client as unknown as PrivateState;
    const dmSeen: LiveDmMessage[] = [];
    let chatStateFired = 0;
    let displayedFired = 0;
    client.setDirectMessageHandler((message) => { dmSeen.push(message); });
    client.setDmChatStateHandler(() => { chatStateFired += 1; });
    client.setDmDisplayedHandler(() => { displayedFired += 1; });

    state.pendingDuringResume = [];
    state.handleMessage({
      id: "cs-1",
      from: "bob@example.com/phone",
      to: "alice@example.com/desktop",
      chat_state: "composing",
      reaction_emojis: [],
      shared_files: [],
      is_muc: false,
    });
    state.handleMessage({
      id: "displayed-1",
      from: "bob@example.com/phone",
      to: "alice@example.com/desktop",
      displayed_marker_id: "msg-7",
      reaction_emojis: [],
      shared_files: [],
      is_muc: false,
    });

    expect(chatStateFired).toBe(1);
    // #1165: displayed markers buffer with the bodies so their targets
    // exist before they apply; they drain via `completeResumeBarrier`.
    expect(displayedFired).toBe(0);
    expect(dmSeen).toHaveLength(0);
    expect(state.pendingDuringResume).toHaveLength(1);
  });
});
