/**
 * #1165: reactions (XEP-0444) and displayed markers (XEP-0333) that
 * arrive while the resume barrier is open must be buffered alongside
 * body messages and drained AFTER the bodies, so a follow-up whose
 * target arrives in the same catch-up window is applied to an
 * existing timeline row instead of being dropped by the merge
 * layer's target-miss short-circuit.
 *
 * Like `resume-ordering.test.ts`, these tests drive the resume-gate
 * in `handleMessage` directly and complete the barrier via
 * `completeResumeBarrier` — the gate's contract is independent of
 * the WASM session machinery.
 */
import { afterEach, describe, expect, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime-durable-store";

type PrivateState = {
  xmpp: unknown;
  connectEpoch: number;
  pendingDuringResume: unknown[] | null;
  resumeBarrier: {
    xmpp: unknown;
    generation: number;
    promise: Promise<void>;
  } | null;
  currentRoom: string | null;
  joinedMucReady: Set<string>;
  handleMessage: (message: unknown) => void;
  openResumeBarrier: (
    xmpp: unknown,
    generation: number,
    promise: Promise<void>,
  ) => void;
  completeResumeBarrier: (xmpp: unknown, generation: number) => Promise<void>;
};

const ROOM_JID = "general@conference.example.com";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

const createdClients: BrowserXmppClient[] = [];

async function bindCurrentGeneration(client: BrowserXmppClient): Promise<void> {
  const state = client as unknown as {
    connectEpoch: number;
    outboundQueueHydration: Promise<void>;
    outboundQueue: {
      beginConnectionGeneration: (generation: number) => number;
      whenQuiescent: () => Promise<void>;
    };
  };
  await state.outboundQueueHydration;
  await state.outboundQueue.whenQuiescent();
  state.outboundQueue.beginConnectionGeneration(state.connectEpoch);
}

async function createTestClient(): Promise<BrowserXmppClient> {
  const client = new BrowserXmppClient(session(), {
    durableRuntimeStore: new MemoryDurableOutboundStore(),
  });
  createdClients.push(client);
  await bindCurrentGeneration(client);
  return client;
}

afterEach(async () => {
  for (const client of createdClients) {
    await bindCurrentGeneration(client);
    const state = client as unknown as { xmpp: null; connected: boolean };
    state.xmpp = null;
    state.connected = false;
  }
  await Promise.all(createdClients.map((client) => client.dispose()));
  createdClients.length = 0;
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
    from: `${ROOM_JID}/bob`,
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

function roomReactionWasmMessage(targetId: string) {
  return {
    id: `reaction-${targetId}`,
    from: `${ROOM_JID}/bob`,
    to: "alice@example.com/desktop",
    message_type: "groupchat",
    reaction_target_id: targetId,
    reaction_emojis: ["👍"],
    shared_files: [],
    is_muc: true,
  };
}

function roomDisplayedWasmMessage(targetId: string) {
  return {
    id: `displayed-${targetId}`,
    from: `${ROOM_JID}/bob`,
    to: "alice@example.com/desktop",
    message_type: "groupchat",
    displayed_marker_id: targetId,
    reaction_emojis: [],
    shared_files: [],
    is_muc: true,
  };
}

function dmReactionWasmMessage(targetId: string) {
  return {
    id: `reaction-${targetId}`,
    from: "bob@example.com/phone",
    to: "alice@example.com/desktop",
    message_type: "chat",
    reaction_target_id: targetId,
    reaction_emojis: ["🎉"],
    shared_files: [],
    is_muc: false,
  };
}

function dmDisplayedWasmMessage(targetId: string) {
  return {
    id: `displayed-${targetId}`,
    from: "bob@example.com/phone",
    to: "alice@example.com/desktop",
    message_type: "chat",
    displayed_marker_id: targetId,
    reaction_emojis: [],
    shared_files: [],
    is_muc: false,
  };
}

async function clientWithOpenBarrier() {
  const client = await createTestClient();
  const state = client as unknown as PrivateState;
  state.currentRoom = ROOM_JID;
  // Ready room ⇒ post-drain queue flush is a no-op instead of a join.
  state.joinedMucReady.add(ROOM_JID);
  const handle = {};
  state.connectEpoch = 0;
  state.xmpp = handle;
  state.openResumeBarrier(handle, 0, Promise.resolve());
  return { client, state, handle };
}

describe("resume-time reaction/displayed-marker buffering (#1165)", () => {
  test("room reaction arriving before its target in the catch-up window drains after the body", async () => {
    const { client, state, handle } = await clientWithOpenBarrier();
    const order: string[] = [];
    client.setMessageHandler((message) => order.push(`body:${message.body}`));
    client.setReactionHandler((event) => order.push(`reaction:${event.messageId}`));

    state.handleMessage(roomReactionWasmMessage("room-1"));
    state.handleMessage(roomWasmMessage("room-1", "hello", "2026-05-20T10:00:00.000Z"));
    expect(order).toHaveLength(0);
    expect(state.pendingDuringResume).toHaveLength(2);

    await state.completeResumeBarrier(handle, 0);

    expect(order).toEqual(["body:hello", "reaction:room-1"]);
    expect(state.pendingDuringResume).toBeNull();
  });

  test("room displayed marker arriving before its target drains after the body", async () => {
    const { client, state, handle } = await clientWithOpenBarrier();
    const order: string[] = [];
    client.setMessageHandler((message) => order.push(`body:${message.body}`));
    client.setDisplayedHandler((event) => order.push(`displayed:${event.messageId}`));

    state.handleMessage(roomDisplayedWasmMessage("room-2"));
    state.handleMessage(roomWasmMessage("room-2", "read me", "2026-05-20T10:00:00.000Z"));
    expect(order).toHaveLength(0);

    await state.completeResumeBarrier(handle, 0);

    expect(order).toEqual(["body:read me", "displayed:room-2"]);
  });

  test("DM reaction and displayed marker drain after the DM body", async () => {
    const { client, state, handle } = await clientWithOpenBarrier();
    const order: string[] = [];
    client.setDirectMessageHandler((message) => order.push(`body:${message.body}`));
    client.setDmReactionHandler((event) => order.push(`reaction:${event.messageId}`));
    client.setDmDisplayedHandler((event) => order.push(`displayed:${event.messageId}`));

    state.handleMessage(dmReactionWasmMessage("dm-1"));
    state.handleMessage(dmDisplayedWasmMessage("dm-1"));
    state.handleMessage(dmWasmMessage("dm-1", "hi", "2026-05-20T10:00:00.000Z"));
    expect(order).toHaveLength(0);

    await state.completeResumeBarrier(handle, 0);

    expect(order).toEqual(["body:hi", "reaction:dm-1", "displayed:dm-1"]);
  });

  test("relative order within reactions/markers is preserved across the drain", async () => {
    const { client, state, handle } = await clientWithOpenBarrier();
    const order: string[] = [];
    client.setReactionHandler((event) => order.push(`reaction:${event.messageId}`));
    client.setDisplayedHandler((event) => order.push(`displayed:${event.messageId}`));

    state.handleMessage(roomReactionWasmMessage("a"));
    state.handleMessage(roomDisplayedWasmMessage("b"));
    state.handleMessage(roomReactionWasmMessage("c"));

    await state.completeResumeBarrier(handle, 0);

    expect(order).toEqual(["reaction:a", "displayed:b", "reaction:c"]);
  });

  // F2: the barrier's `.finally` fires after the connection died
  // mid-catch-up (`this.xmpp` moved on). The buffered stanzas were
  // SM-acked — the server will never replay them — so a failed barrier
  // must carry them into the NEXT barrier instead of discarding them.
  test("a reaction buffered during a failed barrier drains exactly once when the next barrier completes", async () => {
    const client = await createTestClient();
    const state = client as unknown as PrivateState;
    state.currentRoom = ROOM_JID;
    state.joinedMucReady.add(ROOM_JID);
    const reactions: string[] = [];
    client.setReactionHandler((event) => reactions.push(event.messageId));

    // Barrier A opens on handle A; a reaction arrives mid-catch-up.
    const handleA = {};
    state.connectEpoch = 0;
    state.xmpp = handleA;
    state.openResumeBarrier(handleA, 0, Promise.resolve());
    state.handleMessage(roomReactionWasmMessage("room-9"));
    expect(state.pendingDuringResume).toHaveLength(1);

    // The connection dies mid-catch-up (handleDisconnected nulls
    // this.xmpp), then barrier A's `.finally` fires: no dispatch, but
    // no loss either.
    state.xmpp = null;
    await state.completeResumeBarrier(handleA, 0);
    expect(reactions).toHaveLength(0);

    // The next session's barrier completes: the carried reaction
    // drains exactly once.
    const handleB = {};
    state.connectEpoch = 1;
    await bindCurrentGeneration(client);
    state.xmpp = handleB;
    state.openResumeBarrier(handleB, 1, Promise.resolve());
    await state.completeResumeBarrier(handleB, 1);
    expect(reactions).toEqual(["room-9"]);

    // A third barrier must not re-dispatch it.
    state.openResumeBarrier(handleB, 1, Promise.resolve());
    await state.completeResumeBarrier(handleB, 1);
    expect(reactions).toEqual(["room-9"]);
  });

  test("carried entries keep drain ordering: bodies before targeted follow-ups across barriers", async () => {
    const client = await createTestClient();
    const state = client as unknown as PrivateState;
    state.currentRoom = ROOM_JID;
    state.joinedMucReady.add(ROOM_JID);
    const order: string[] = [];
    client.setMessageHandler((message) => order.push(`body:${message.body}`));
    client.setReactionHandler((event) => order.push(`reaction:${event.messageId}`));
    client.setDisplayedHandler((event) => order.push(`displayed:${event.messageId}`));

    // Barrier A buffers a reaction and a marker, then fails.
    const handleA = {};
    state.connectEpoch = 0;
    state.xmpp = handleA;
    state.openResumeBarrier(handleA, 0, Promise.resolve());
    state.handleMessage(roomReactionWasmMessage("room-7"));
    state.handleMessage(roomDisplayedWasmMessage("room-7"));
    state.xmpp = null;
    await state.completeResumeBarrier(handleA, 0);
    expect(order).toHaveLength(0);

    // Barrier B buffers the follow-ups' target body, then completes:
    // the body drains before the carried follow-ups, which keep their
    // relative arrival order.
    const handleB = {};
    state.connectEpoch = 1;
    await bindCurrentGeneration(client);
    state.xmpp = handleB;
    state.openResumeBarrier(handleB, 1, Promise.resolve());
    state.handleMessage(roomWasmMessage("room-7", "hello", "2026-05-20T10:00:00.000Z"));
    await state.completeResumeBarrier(handleB, 1);

    expect(order).toEqual(["body:hello", "reaction:room-7", "displayed:room-7"]);
  });

  test("live (non-barrier) reactions and displayed markers still dispatch immediately", async () => {
    const client = await createTestClient();
    const state = client as unknown as PrivateState;
    const order: string[] = [];
    client.setReactionHandler((event) => order.push(`reaction:${event.messageId}`));
    client.setDmDisplayedHandler((event) => order.push(`displayed:${event.messageId}`));

    expect(state.pendingDuringResume).toBeNull();
    state.handleMessage(roomReactionWasmMessage("live-1"));
    state.handleMessage(dmDisplayedWasmMessage("live-2"));

    expect(order).toEqual(["reaction:live-1", "displayed:live-2"]);
  });
});
